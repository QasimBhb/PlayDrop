//! This computer's battery, for the `battery` frame (protocol.md 5.8, PLAN.md Phase 3
//! "session stream ... battery").
//!
//! Linux reads `/sys/class/power_supply/*`: every supply of `type` `Battery` that is
//! present and not a peripheral (`scope` `Device`, a mouse or headset) counts, combined by
//! capacity when a laptop has two. Windows asks `GetSystemPowerStatus`. A desktop without a
//! battery reports nothing, and the daemon then never calls `Host::set_battery`.
//!
//! The daemon polls every [`POLL`] (two small sysfs reads, no wake-ups on a desktop, which
//! stops polling when the first read finds no battery) and pushes when the reading changed
//! or [`REFRESH`] passed since the last push; the core rate-limits the wire to once per 60 s
//! unless `charging` or `low` changed.
//!
//! `charging` means "on external power", the way phones show it: a full battery on the
//! charger, or one held at a charge limit, still reads as charging rather than draining.

use std::time::Duration;

use tokio::time::Instant;

/// How often the battery is read.
pub const POLL: Duration = Duration::from_secs(10);
/// Longest time between two pushes of an unchanged reading.
pub const REFRESH: Duration = Duration::from_mins(1);
/// At or below this percentage, a battery not on power counts as low (Android's default
/// low-battery warning level).
pub const LOW_PERCENT: u8 = 15;

/// One battery reading, as the `battery` frame carries it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reading {
    /// Percent, 0 to 100.
    pub level: u8,
    /// On external power.
    pub charging: bool,
    /// Low and not on power.
    pub low: bool,
}

/// Whether to push `now_reading` given the last push: on change, or after [`REFRESH`].
pub fn should_push(last: Option<(Reading, Instant)>, reading: Reading, now: Instant) -> bool {
    match last {
        None => true,
        Some((prev, at)) => prev != reading || now.duration_since(at) >= REFRESH,
    }
}

/// Reads the battery now. `None` when the computer has none or it cannot be read.
///
/// Blocking (file reads or a system call): call it from `spawn_blocking`.
pub fn read() -> Option<Reading> {
    platform::read()
}

/// One power supply's state, parsed from its sysfs files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
struct Supply {
    level: u8,
    /// `energy_full` or `charge_full`, to weigh two batteries by size.
    full: Option<u64>,
    on_power: bool,
    /// `capacity_level` says `Low` or `Critical`.
    low_flag: bool,
}

/// The sysfs files of one supply, each trimmed, `None` when missing.
#[derive(Debug, Default, Clone, Copy)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
struct SupplyFiles<'a> {
    kind: Option<&'a str>,
    scope: Option<&'a str>,
    present: Option<&'a str>,
    capacity: Option<&'a str>,
    capacity_level: Option<&'a str>,
    status: Option<&'a str>,
    energy_full: Option<&'a str>,
    charge_full: Option<&'a str>,
}

/// Parses one supply. `None` for anything that is not a present system battery with a
/// readable capacity.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_supply(f: &SupplyFiles<'_>) -> Option<Supply> {
    if f.kind != Some("Battery") || f.scope == Some("Device") || f.present == Some("0") {
        return None;
    }
    let level = f.capacity?.parse::<u8>().ok()?.min(100);
    let on_power = matches!(f.status, Some("Charging" | "Full" | "Not charging"));
    let low_flag = matches!(f.capacity_level, Some("Low" | "Critical"));
    let full = f
        .energy_full
        .or(f.charge_full)
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0);
    Some(Supply {
        level,
        full,
        on_power,
        low_flag,
    })
}

/// Combines a laptop's batteries into one reading: weighted by size when every battery
/// reports it, a plain average otherwise. On power if any is; low if the result is.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn combine(supplies: &[Supply]) -> Option<Reading> {
    if supplies.is_empty() {
        return None;
    }
    let weighted: Option<Vec<(u64, u64)>> = supplies
        .iter()
        .map(|s| s.full.map(|f| (u64::from(s.level) * f, f)))
        .collect();
    let level = if let Some(parts) = weighted {
        let (sum, total) = parts
            .iter()
            .fold((0u64, 0u64), |(a, b), (x, y)| (a + x, b + y));
        sum.checked_div(total).unwrap_or(0)
    } else {
        let sum: u64 = supplies.iter().map(|s| u64::from(s.level)).sum();
        sum / supplies.len() as u64
    };
    let level = u8::try_from(level.min(100)).unwrap_or(100);
    let charging = supplies.iter().any(|s| s.on_power);
    let low = !charging && (level <= LOW_PERCENT || supplies.iter().all(|s| s.low_flag));
    Some(Reading {
        level,
        charging,
        low,
    })
}

/// Windows `SYSTEM_POWER_STATUS` fields to a reading. Pure, so it is tested everywhere.
///
/// `flag` 128 means no system battery and 255 unknown; `percent` 255 is unknown. `ac` 1 is
/// on power. Flag bits 2 (low) and 4 (critical) mark a low battery.
#[cfg_attr(not(windows), allow(dead_code))]
fn from_power_status(ac: u8, flag: u8, percent: u8) -> Option<Reading> {
    if flag == 128 || flag == 255 || percent > 100 {
        return None;
    }
    let charging = ac == 1;
    let low = !charging && (flag & 0b110 != 0 || percent <= LOW_PERCENT);
    Some(Reading {
        level: percent,
        charging,
        low,
    })
}

#[cfg(target_os = "linux")]
mod platform {
    use std::path::Path;

    use super::{combine, parse_supply, Reading, SupplyFiles};

    const ROOT: &str = "/sys/class/power_supply";

    pub(super) fn read() -> Option<Reading> {
        read_from(Path::new(ROOT))
    }

    /// Reads every supply under `root` (a `power_supply` class directory).
    pub(super) fn read_from(root: &Path) -> Option<Reading> {
        let entries = std::fs::read_dir(root).ok()?;
        let mut supplies = Vec::new();
        for entry in entries.flatten() {
            let dir = entry.path();
            let file = |name: &str| std::fs::read_to_string(dir.join(name)).ok();
            let (kind, scope, present) = (file("type"), file("scope"), file("present"));
            let (capacity, capacity_level, status) =
                (file("capacity"), file("capacity_level"), file("status"));
            let (energy_full, charge_full) = (file("energy_full"), file("charge_full"));
            let files = SupplyFiles {
                kind: kind.as_deref().map(str::trim),
                scope: scope.as_deref().map(str::trim),
                present: present.as_deref().map(str::trim),
                capacity: capacity.as_deref().map(str::trim),
                capacity_level: capacity_level.as_deref().map(str::trim),
                status: status.as_deref().map(str::trim),
                energy_full: energy_full.as_deref().map(str::trim),
                charge_full: charge_full.as_deref().map(str::trim),
            };
            if let Some(s) = parse_supply(&files) {
                supplies.push(s);
            }
        }
        combine(&supplies)
    }
}

#[cfg(windows)]
mod platform {
    use super::{from_power_status, Reading};

    /// `SYSTEM_POWER_STATUS` (winbase.h). Declared here because the `windows` crate's
    /// `Win32_System_Power` feature is not enabled for this crate.
    #[repr(C)]
    #[derive(Default)]
    struct SystemPowerStatus {
        ac_line_status: u8,
        battery_flag: u8,
        battery_life_percent: u8,
        system_status_flag: u8,
        battery_life_time: u32,
        battery_full_life_time: u32,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetSystemPowerStatus(status: *mut SystemPowerStatus) -> i32;
    }

    #[allow(unsafe_code)]
    pub(super) fn read() -> Option<Reading> {
        let mut status = SystemPowerStatus::default();
        // SAFETY: `status` is a writable, correctly laid out SYSTEM_POWER_STATUS that
        // outlives the call; the function only fills it in.
        let ok = unsafe { GetSystemPowerStatus(&raw mut status) };
        if ok == 0 {
            return None;
        }
        from_power_status(
            status.ac_line_status,
            status.battery_flag,
            status.battery_life_percent,
        )
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
mod platform {
    use super::Reading;

    pub(super) fn read() -> Option<Reading> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn battery<'a>(capacity: &'a str, status: &'a str) -> SupplyFiles<'a> {
        SupplyFiles {
            kind: Some("Battery"),
            scope: None,
            present: Some("1"),
            capacity: Some(capacity),
            capacity_level: Some("Normal"),
            status: Some(status),
            energy_full: None,
            charge_full: None,
        }
    }

    #[test]
    fn parses_a_laptop_battery_and_skips_the_rest() {
        let s = parse_supply(&battery("64", "Discharging")).unwrap();
        assert_eq!((s.level, s.on_power, s.low_flag), (64, false, false));
        for status in ["Charging", "Full", "Not charging"] {
            assert!(parse_supply(&battery("90", status)).unwrap().on_power);
        }
        assert!(!parse_supply(&battery("90", "Unknown")).unwrap().on_power);

        let mains = SupplyFiles {
            kind: Some("Mains"),
            ..battery("1", "Charging")
        };
        assert_eq!(parse_supply(&mains), None);
        let mouse = SupplyFiles {
            scope: Some("Device"),
            ..battery("40", "Discharging")
        };
        assert_eq!(parse_supply(&mouse), None);
        let absent = SupplyFiles {
            present: Some("0"),
            ..battery("40", "Discharging")
        };
        assert_eq!(parse_supply(&absent), None);
        assert_eq!(parse_supply(&battery("n/a", "Discharging")), None);
        assert_eq!(parse_supply(&battery("120", "Full")).unwrap().level, 100);
    }

    #[test]
    fn combines_batteries_and_flags_low() {
        assert_eq!(combine(&[]), None);
        let one = parse_supply(&battery("12", "Discharging")).unwrap();
        assert_eq!(
            combine(&[one]),
            Some(Reading {
                level: 12,
                charging: false,
                low: true
            })
        );
        let charging = parse_supply(&battery("12", "Charging")).unwrap();
        assert!(!combine(&[charging]).unwrap().low, "on power is never low");

        // A 20 Wh battery at 100% and a 60 Wh one at 20%: 40% by energy, 60% averaged.
        let big = SupplyFiles {
            energy_full: Some("60000000"),
            ..battery("20", "Discharging")
        };
        let small = SupplyFiles {
            energy_full: Some("20000000"),
            ..battery("100", "Discharging")
        };
        let both = [parse_supply(&big).unwrap(), parse_supply(&small).unwrap()];
        assert_eq!(combine(&both).unwrap().level, 40);
        let unweighted = [
            parse_supply(&battery("20", "Discharging")).unwrap(),
            parse_supply(&battery("100", "Charging")).unwrap(),
        ];
        let r = combine(&unweighted).unwrap();
        assert_eq!((r.level, r.charging, r.low), (60, true, false));

        let critical = SupplyFiles {
            capacity_level: Some("Critical"),
            ..battery("30", "Discharging")
        };
        assert!(combine(&[parse_supply(&critical).unwrap()]).unwrap().low);
    }

    #[test]
    fn windows_power_status() {
        assert_eq!(from_power_status(1, 128, 255), None, "no system battery");
        assert_eq!(from_power_status(0, 255, 50), None, "unknown");
        assert_eq!(from_power_status(0, 1, 255), None, "unknown percent");
        assert_eq!(
            from_power_status(0, 1, 80),
            Some(Reading {
                level: 80,
                charging: false,
                low: false
            })
        );
        assert!(from_power_status(0, 2, 30).unwrap().low, "low flag");
        assert!(from_power_status(0, 0, 10).unwrap().low, "under 15%");
        let r = from_power_status(1, 8 | 2, 10).unwrap();
        assert!(r.charging && !r.low);
    }

    #[test]
    fn pushes_on_change_or_after_a_minute() {
        let t0 = Instant::now();
        let r = Reading {
            level: 50,
            charging: false,
            low: false,
        };
        assert!(should_push(None, r, t0));
        assert!(!should_push(Some((r, t0)), r, t0 + POLL));
        assert!(should_push(Some((r, t0)), r, t0 + REFRESH));
        let plugged = Reading {
            charging: true,
            ..r
        };
        assert!(should_push(Some((r, t0)), plugged, t0 + POLL));
        let lower = Reading { level: 49, ..r };
        assert!(should_push(Some((r, t0)), lower, t0 + POLL));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn reads_a_sysfs_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let write = |dir: &str, files: &[(&str, &str)]| {
            let d = tmp.path().join(dir);
            std::fs::create_dir_all(&d).unwrap();
            for (name, value) in files {
                std::fs::write(d.join(name), format!("{value}\n")).unwrap();
            }
        };
        write("AC", &[("type", "Mains"), ("online", "1")]);
        assert_eq!(
            platform::read_from(tmp.path()),
            None,
            "a desktop: no battery"
        );
        write(
            "BAT0",
            &[
                ("type", "Battery"),
                ("present", "1"),
                ("capacity", "77"),
                ("status", "Charging"),
            ],
        );
        write(
            "hidpp_battery_0",
            &[
                ("type", "Battery"),
                ("scope", "Device"),
                ("capacity", "5"),
                ("status", "Discharging"),
            ],
        );
        assert_eq!(
            platform::read_from(tmp.path()),
            Some(Reading {
                level: 77,
                charging: true,
                low: false
            })
        );
        assert_eq!(platform::read_from(&tmp.path().join("missing")), None);
    }
}
