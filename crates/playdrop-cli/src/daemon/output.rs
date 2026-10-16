//! Human and JSON output of the daemon commands.
//!
//! Human output is plain columns: no colours, no emoji, padded with spaces so it lines up
//! in a terminal and still splits on whitespace. JSON output is the IPC types serialised
//! as they are, one object per command.

use std::fmt::Write as _;
use std::time::{SystemTime, UNIX_EPOCH};

use playdrop_core::host::{
    MdnsStatus, MulticastStatus, OutboxItem, PeerSummary, TransferSnapshot, TransferStatus,
};
use playdrop_ipc::{ClipSkip, DiagnosticsReport, PortProbe, Status};
use serde::Serialize;

const KIB: f64 = 1024.0;
const MIB: f64 = 1024.0 * 1024.0;
const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

/// Seconds since the Unix epoch, for "ago" columns.
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// One JSON object on one line.
pub fn json<T: Serialize>(value: &T) -> String {
    // The IPC types serialise without fail: plain structs, strings and numbers.
    serde_json::to_string(value).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
}

/// Byte count with a binary unit, one decimal above bytes.
// Display only: precision lost converting u64 to f64 is far below the one decimal shown.
#[allow(clippy::cast_precision_loss)]
pub fn size(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= GIB {
        format!("{:.1} GiB", b / GIB)
    } else if b >= MIB {
        format!("{:.1} MiB", b / MIB)
    } else if b >= KIB {
        format!("{:.1} KiB", b / KIB)
    } else {
        format!("{bytes} B")
    }
}

/// A rate in MiB/s with one decimal.
#[allow(clippy::cast_precision_loss)]
pub fn mib_per_sec(bytes_per_sec: f64) -> String {
    format!("{:.1} MiB/s", bytes_per_sec / MIB)
}

/// "12 s ago", "5 min ago", "3 h ago", "2 d ago"; "never" for `None`.
pub fn ago(now: u64, then: Option<u64>) -> String {
    let Some(then) = then else {
        return "never".to_owned();
    };
    let s = now.saturating_sub(then);
    match s {
        0..=59 => format!("{s} s ago"),
        60..=3_599 => format!("{} min ago", s / 60),
        3_600..=86_399 => format!("{} h ago", s / 3_600),
        _ => format!("{} d ago", s / 86_400),
    }
}

/// Columns padded to the widest cell, two spaces apart, no trailing spaces.
pub fn table(header: &[&str], rows: &[Vec<String>]) -> String {
    let mut widths: Vec<usize> = header.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if let Some(w) = widths.get_mut(i) {
                *w = (*w).max(cell.chars().count());
            }
        }
    }
    let mut out = String::new();
    let header: Vec<String> = header.iter().map(|h| (*h).to_owned()).collect();
    for row in std::iter::once(&header).chain(rows) {
        let mut line = String::new();
        for (i, cell) in row.iter().enumerate() {
            if i > 0 {
                line.push_str("  ");
            }
            line.push_str(cell);
            let pad = widths.get(i).copied().unwrap_or(0) - cell.chars().count();
            line.extend(std::iter::repeat_n(' ', pad));
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// `label: value` lines with the values in one column.
fn key_values(rows: &[(&str, String)]) -> String {
    let width = rows.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
    let mut out = String::new();
    for (k, v) in rows {
        let _ = writeln!(out, "{k:<width$}  {v}");
    }
    out
}

/// "online", "offline" or "removed": whether the peer still has us pinned matters more
/// than whether it is reachable.
pub fn peer_state(p: &PeerSummary) -> &'static str {
    if p.removed_by_peer {
        "removed"
    } else if p.online {
        "online"
    } else {
        "offline"
    }
}

/// The short list printed when `--to` is missing or matches nothing.
pub fn peer_choices(peers: &[PeerSummary]) -> String {
    let rows: Vec<Vec<String>> = peers
        .iter()
        .map(|p| {
            vec![
                p.short_id.clone(),
                p.label.clone(),
                peer_state(p).to_owned(),
            ]
        })
        .collect();
    table(&["SHORT ID", "LABEL", "STATE"], &rows)
}

/// `playdrop peers`.
pub fn peers(peers: &[PeerSummary], now: u64) -> String {
    if peers.is_empty() {
        return "no paired devices; pair one from the PlayDrop window\n".to_owned();
    }
    let rows: Vec<Vec<String>> = peers
        .iter()
        .map(|p| {
            vec![
                p.short_id.clone(),
                p.label.clone(),
                peer_state(p).to_owned(),
                if p.online {
                    "now".to_owned()
                } else {
                    ago(now, p.last_seen)
                },
                p.last_known
                    .first()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
            ]
        })
        .collect();
    table(
        &["SHORT ID", "LABEL", "STATE", "LAST SEEN", "ADDRESS"],
        &rows,
    )
}

/// The label of a hex Device ID, or its Short ID when the peer is gone.
fn peer_name(peers: &[PeerSummary], device_id: &str, fallback: &str) -> String {
    peers
        .iter()
        .find(|p| p.device_id == device_id)
        .map_or_else(|| fallback.to_owned(), |p| p.label.clone())
}

/// `playdrop status`.
pub fn status(s: &Status) -> String {
    let on = |b: bool| if b { "on" } else { "off" };
    let mut rows = vec![
        ("name", s.settings.device_name.clone()),
        ("short id", s.short_id.clone()),
        ("device id", s.device_id.clone()),
        ("address", s.addr.to_string()),
        (
            "daemon",
            format!("{} (ipc {})", s.daemon_version, s.ipc_version),
        ),
        ("receive dir", s.settings.receive_dir.display().to_string()),
        (
            "clipboard",
            format!(
                "sync {}, images {}{}",
                on(s.settings.clipboard_sync),
                on(s.settings.clipboard_images),
                if s.settings.clipboard_watch_supported {
                    ""
                } else {
                    " (this desktop cannot watch the clipboard)"
                }
            ),
        ),
        (
            "remote input",
            if s.settings.remote_input_supported {
                "available".to_owned()
            } else {
                format!(
                    "unavailable{}",
                    s.settings
                        .remote_input_reason
                        .as_deref()
                        .map(|r| format!(": {r}"))
                        .unwrap_or_default()
                )
            },
        ),
        ("start at login", on(s.settings.autostart).to_owned()),
        ("notifications", on(s.settings.notifications).to_owned()),
        (
            "add device",
            if s.add_device_window_open {
                "open"
            } else {
                "closed"
            }
            .to_owned(),
        ),
        ("locked", if s.locked { "yes" } else { "no" }.to_owned()),
    ];
    if !s.input_from.is_empty() {
        rows.push(("input from", s.input_from.join(", ")));
    }
    key_values(&rows)
}

/// The UI word for a transfer state.
pub fn state_name(s: TransferStatus) -> &'static str {
    match s {
        TransferStatus::Offered => "offered",
        TransferStatus::Active => "active",
        TransferStatus::Complete => "complete",
        TransferStatus::Failed => "failed",
        TransferStatus::Cancelled => "cancelled",
    }
}

fn direction_name(d: playdrop_core::host::TransferDirection) -> &'static str {
    match d {
        playdrop_core::host::TransferDirection::Send => "send",
        playdrop_core::host::TransferDirection::Receive => "receive",
    }
}

/// `playdrop transfers`.
pub fn transfers(list: &[TransferSnapshot], peers: &[PeerSummary], now: u64) -> String {
    if list.is_empty() {
        return "no transfers in the last 10 minutes\n".to_owned();
    }
    let rows: Vec<Vec<String>> = list
        .iter()
        .map(|t| {
            vec![
                t.id.clone(),
                direction_name(t.direction).to_owned(),
                peer_name(peers, &t.peer, &t.short_id),
                state_name(t.state).to_owned(),
                format!("{} of {}", size(t.bytes), size(t.total)),
                files_column(t),
                ago(now, Some(t.started_at)),
                t.note.clone().unwrap_or_default(),
            ]
        })
        .collect();
    table(
        &[
            "ID", "DIR", "PEER", "STATE", "BYTES", "FILES", "STARTED", "NOTE",
        ],
        &rows,
    )
}

/// The FILES column: "done/total" while a bundle is moving, the total otherwise. A daemon
/// that does not report finished files (0) shows the total only.
fn files_column(t: &TransferSnapshot) -> String {
    if t.state == TransferStatus::Active && t.file_count > 1 && t.files_done > 0 {
        format!("{}/{}", t.files_done, t.file_count)
    } else {
        t.file_count.to_string()
    }
}

/// `playdrop outbox`.
pub fn outbox(items: &[OutboxItem], peers: &[PeerSummary], now: u64) -> String {
    if items.is_empty() {
        return "the outbox is empty\n".to_owned();
    }
    let rows: Vec<Vec<String>> = items
        .iter()
        .map(|i| {
            vec![
                i.id.clone(),
                peer_name(peers, &i.peer, i.peer.get(..8).unwrap_or(&i.peer)),
                i.file_count.to_string(),
                size(i.total_bytes),
                ago(now, Some(i.created_at_ms / 1000)),
                i.note.clone().unwrap_or_default(),
            ]
        })
        .collect();
    table(&["ID", "FOR", "FILES", "SIZE", "QUEUED", "NOTE"], &rows)
}

/// `playdrop diagnose`: the daemon's summary lines first, then the layers behind them.
pub fn diagnose(r: &DiagnosticsReport, now: u64) -> String {
    let mut out = String::new();
    for line in &r.summary {
        let _ = writeln!(out, "{line}");
    }
    out.push('\n');
    let c = &r.core;
    let interfaces = if c.interfaces.is_empty() {
        "none".to_owned()
    } else {
        c.interfaces
            .iter()
            .map(|i| format!("{} mask {}", i.addr, i.netmask))
            .collect::<Vec<_>>()
            .join(", ")
    };
    out.push_str(&key_values(&[
        (
            "this device",
            format!(
                "{} on {} ({})",
                c.short_id,
                c.addr,
                if c.bind_ok {
                    "listening"
                } else {
                    "NOT listening"
                }
            ),
        ),
        ("interfaces", interfaces),
        ("multicast", multicast_line(&c.multicast)),
        ("mdns", mdns_line(&c.mdns)),
        ("port", port_line(&r.port)),
    ]));
    if !c.peers.is_empty() {
        out.push('\n');
        let yes_no = |b: bool| if b { "yes" } else { "no" };
        let rows: Vec<Vec<String>> = c
            .peers
            .iter()
            .map(|p| {
                vec![
                    p.short_id.clone(),
                    p.label.clone(),
                    if p.online { "online" } else { "offline" }.to_owned(),
                    yes_no(p.mdns_seen).to_owned(),
                    yes_no(p.beacon_seen).to_owned(),
                    p.last_result.as_ref().map_or_else(String::new, |f| {
                        format!("{} ({})", f.failure, ago(now, Some(f.at)))
                    }),
                ]
            })
            .collect();
        out.push_str(&table(
            &[
                "SHORT ID",
                "LABEL",
                "STATE",
                "MDNS",
                "BEACON",
                "LAST FAILURE",
            ],
            &rows,
        ));
    }
    out
}

fn secs_ago(secs: Option<u64>) -> String {
    secs.map_or_else(|| "never".to_owned(), |s| format!("{s} s ago"))
}

fn multicast_line(m: &MulticastStatus) -> String {
    if !m.enabled {
        return "off".to_owned();
    }
    let mut s = format!(
        "joined on {}",
        if m.joined.is_empty() {
            "nothing".to_owned()
        } else {
            join(&m.joined)
        }
    );
    for f in &m.join_failed {
        let _ = write!(s, "; failed on {}: {}", f.addr, f.error);
    }
    let _ = write!(
        s,
        "; last beacon from another device {}",
        secs_ago(m.last_beacon_secs_ago)
    );
    s
}

fn mdns_line(m: &MdnsStatus) -> String {
    if !m.enabled {
        return "off".to_owned();
    }
    let mut s = if m.registered {
        "registered".to_owned()
    } else {
        "not registered".to_owned()
    };
    if let Some(e) = &m.error {
        let _ = write!(s, " ({e})");
    }
    let _ = write!(
        s,
        "; last advert from another device {}",
        secs_ago(m.last_seen_secs_ago)
    );
    s
}

fn port_line(p: &PortProbe) -> String {
    let mut s = format!(
        "{}: loopback {}",
        p.port,
        if p.loopback_ok { "ok" } else { "refused" }
    );
    for a in &p.lan {
        let _ = write!(
            s,
            "; {} {}",
            a.addr,
            if a.ok {
                "ok"
            } else {
                a.error.as_deref().unwrap_or("unreachable")
            }
        );
    }
    if let Some(fw) = &p.firewall {
        let _ = write!(
            s,
            "; firewall {} {}{}",
            fw.tool,
            if fw.active { "active" } else { "inactive" },
            match fw.port_allowed {
                Some(true) => ", port allowed".to_owned(),
                Some(false) => format!(", port not allowed: {}", fw.fix),
                None => String::new(),
            }
        );
    }
    s
}

fn join<T: ToString>(items: &[T]) -> String {
    items
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Live state of one outgoing transfer, as the progress line shows it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Progress {
    /// Bytes sent so far.
    pub bytes: u64,
    /// Total bytes in the manifest.
    pub total: u64,
    /// Files in the manifest; 0 until the daemon lists the transfer.
    pub files: usize,
    /// Files finished so far; 0 until the daemon reports it (an older one never does).
    pub files_done: usize,
    /// Most recent rate.
    pub bytes_per_sec: u64,
    /// The receiver has not accepted yet.
    pub waiting: bool,
}

/// The one progress line of `playdrop send`.
#[allow(clippy::cast_precision_loss)]
pub fn progress_line(p: &Progress, to: &str) -> String {
    if p.waiting {
        return format!(
            "offered {} to {to}, waiting for it to accept",
            size(p.total)
        );
    }
    let percent = if p.total == 0 {
        100.0
    } else {
        p.bytes as f64 * 100.0 / p.total as f64
    };
    let files = match (p.files, p.files_done) {
        (0, _) => String::new(),
        (1, _) => "  1 file".to_owned(),
        (n, 0) => format!("  {} files", grouped(n)),
        (n, done) => format!("  {} of {} files", grouped(done), grouped(n)),
    };
    format!(
        "to {to}: {} of {}  {percent:.0}%{files}  {}",
        size(p.bytes),
        size(p.total),
        mib_per_sec(p.bytes_per_sec as f64)
    )
}

/// A count with a comma between thousands: 5000 is "5,000".
fn grouped(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// What `playdrop clip` says when nothing was sent to `to`.
pub fn clip_not_sent(to: &str, skip: ClipSkip) -> String {
    match skip {
        ClipSkip::Echo => format!(
            "not sent: {to} already has this text (it matches the last clip sent or received)"
        ),
        other => format!("not sent to {to}: {}", other.reason()),
    }
}

/// The closing line of a successful `playdrop send`.
#[allow(clippy::cast_precision_loss)]
pub fn sent_line(t: &TransferSnapshot, to: &str, wall_secs: f64) -> String {
    let rate = if wall_secs > 0.0 {
        t.total as f64 / wall_secs
    } else {
        0.0
    };
    format!(
        "sent {} in {} {} to {to} in {wall_secs:.2} s, {} ({})",
        size(t.total),
        t.file_count,
        if t.file_count == 1 { "file" } else { "files" },
        mib_per_sec(rate),
        t.id
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::peer::tests::peer;
    use playdrop_core::host::TransferDirection;

    #[test]
    fn sizes_pick_a_unit() {
        assert_eq!(size(0), "0 B");
        assert_eq!(size(1023), "1023 B");
        assert_eq!(size(1536), "1.5 KiB");
        assert_eq!(size(500 * 1024 * 1024), "500.0 MiB");
        assert_eq!(size(3 * 1024 * 1024 * 1024), "3.0 GiB");
    }

    #[test]
    fn ago_steps() {
        assert_eq!(ago(100, None), "never");
        assert_eq!(ago(100, Some(90)), "10 s ago");
        assert_eq!(ago(1_000, Some(100)), "15 min ago");
        assert_eq!(ago(10_000, Some(100)), "2 h ago");
        assert_eq!(ago(1_000_000, Some(100)), "11 d ago");
        // A clock that went backwards does not underflow.
        assert_eq!(ago(100, Some(200)), "0 s ago");
    }

    #[test]
    fn table_pads_columns_and_trims_line_ends() {
        let t = table(
            &["A", "LONG HEADER", "C"],
            &[
                vec!["wide cell".into(), "x".into(), String::new()],
                vec!["y".into(), "z".into(), "last".into()],
            ],
        );
        assert_eq!(
            t,
            "A          LONG HEADER  C\n\
             wide cell  x\n\
             y          z            last\n"
        );
    }

    #[test]
    fn peers_table_shows_state_and_last_seen() {
        let mut offline = peer("pgry-2xha-ncda-zdim", "bravo", false);
        offline.last_seen = Some(940);
        let mut removed = peer("2g5l-uebm-bgdu-qko4", "iPhone", false);
        removed.removed_by_peer = true;
        let out = peers(
            &[peer("q3pn-42by-v2iw-elcy", "Phone", true), offline, removed],
            1_000,
        );
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 4);
        assert!(lines[0].starts_with("SHORT ID"));
        assert!(
            lines[1].contains("Phone") && lines[1].contains("online") && lines[1].contains("now")
        );
        assert!(lines[2].contains("offline") && lines[2].contains("1 min ago"));
        assert!(lines[3].contains("removed") && lines[3].contains("never"));
        assert!(!out.contains('\t'));
        assert!(peers(&[], 0).contains("no paired devices"));
    }

    fn snapshot(state: TransferStatus) -> TransferSnapshot {
        TransferSnapshot {
            id: "t-1234".into(),
            peer: peer("pgry-2xha", "bravo", true).device_id,
            short_id: "pgry-2xha".into(),
            direction: TransferDirection::Send,
            state,
            bytes: 512 * 1024,
            total: 1024 * 1024,
            bytes_per_sec: 0,
            files_done: 0,
            started_at: 990,
            file_count: 3,
            note: None,
            saved: Vec::new(),
            folder: None,
            delivered: Vec::new(),
        }
    }

    #[test]
    fn transfers_table_names_the_peer_by_label() {
        let peers = [peer("pgry-2xha", "bravo", true)];
        let out = transfers(&[snapshot(TransferStatus::Active)], &peers, 1_000);
        let row = out.lines().nth(1).expect("one row");
        for want in [
            "t-1234",
            "send",
            "bravo",
            "active",
            "512.0 KiB of 1.0 MiB",
            "3",
            "10 s ago",
        ] {
            assert!(row.contains(want), "{want} missing from {row}");
        }
        assert!(transfers(&[], &peers, 0).starts_with("no transfers"));
    }

    #[test]
    fn counts_get_thousands_separators() {
        let got: Vec<String> = [0, 7, 999, 1000, 12_345, 1_234_567].map(grouped).into();
        assert_eq!(got, ["0", "7", "999", "1,000", "12,345", "1,234,567"]);
    }

    #[test]
    fn files_column_shows_progress_only_for_a_moving_bundle() {
        let mut t = snapshot(TransferStatus::Active);
        t.file_count = 40;
        assert_eq!(files_column(&t), "40");
        t.files_done = 12;
        assert_eq!(files_column(&t), "12/40");
        t.file_count = 1;
        assert_eq!(files_column(&t), "1");
        t.file_count = 40;
        t.state = TransferStatus::Complete;
        assert_eq!(files_column(&t), "40");
        t.state = TransferStatus::Active;
        let out = transfers(&[t], &[], 1_000);
        assert!(out.contains("12/40"), "{out}");
    }

    #[test]
    fn clip_not_sent_says_why() {
        assert_eq!(
            clip_not_sent("bravo", ClipSkip::Echo),
            "not sent: bravo already has this text (it matches the last clip sent or received)"
        );
        assert_eq!(
            clip_not_sent("bravo", ClipSkip::NoPeers),
            "not sent to bravo: no connected device has clipboard sharing turned on"
        );
        assert_eq!(
            clip_not_sent("bravo", ClipSkip::Unsupported),
            "not sent to bravo: the connected devices cannot receive this kind of clip"
        );
    }

    #[test]
    fn progress_and_summary_lines() {
        let p = Progress {
            bytes: 512 * 1024 * 1024,
            total: 1024 * 1024 * 1024,
            files: 2,
            files_done: 0,
            bytes_per_sec: 80 * 1024 * 1024,
            waiting: false,
        };
        assert_eq!(
            progress_line(&p, "bravo"),
            "to bravo: 512.0 MiB of 1.0 GiB  50%  2 files  80.0 MiB/s"
        );
        let bundle = Progress {
            files: 5000,
            files_done: 1204,
            ..p
        };
        assert_eq!(
            progress_line(&bundle, "bravo"),
            "to bravo: 512.0 MiB of 1.0 GiB  50%  1,204 of 5,000 files  80.0 MiB/s"
        );
        // Nothing finished yet (or an older daemon): just the total.
        let counting = Progress {
            files_done: 0,
            ..bundle
        };
        assert!(progress_line(&counting, "bravo").contains("  5,000 files  "));
        let waiting = Progress {
            waiting: true,
            total: 2048,
            ..Progress::default()
        };
        assert_eq!(
            progress_line(&waiting, "bravo"),
            "offered 2.0 KiB to bravo, waiting for it to accept"
        );
        let done = snapshot(TransferStatus::Complete);
        assert_eq!(
            sent_line(&done, "bravo", 0.5),
            "sent 1.0 MiB in 3 files to bravo in 0.50 s, 2.0 MiB/s (t-1234)"
        );
    }

    #[test]
    fn json_is_one_line_of_the_ipc_shape() {
        let out = json(&playdrop_ipc::Ok::TransferStarted { id: "t1".into() });
        assert_eq!(out, r#"{"kind":"transfer_started","data":{"id":"t1"}}"#);
        let snap = json(&snapshot(TransferStatus::Complete));
        assert!(!snap.contains('\n'));
        assert!(snap.contains(r#""state":"complete""#));
    }
}
