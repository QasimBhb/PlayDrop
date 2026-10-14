//! Ship gate (PLAN.md build phase 6): "official KDE Connect can stay installed alongside".
//!
//! Starts the real `playdropd` binary the way a login would (discovery on, default
//! flags), then reads the kernel's view of every socket the process owns, several times
//! over a few seconds while the beacon and mDNS run, and asserts that none is bound to or
//! connected to KDE Connect's 1714-1764 or the 53317 of `LocalSend`, and that the process
//! did not join the `LocalSend` multicast group 224.0.0.167 (protocol.md section 1,
//! CLAUDE.md hard rules).
//!
//! The daemon gets a session bus address that does not exist, so it runs without its IPC,
//! tray, portals and notifications: the test never meets a daemon the user is running, and
//! the host, which owns every network socket, starts exactly as in production.
//!
//! Limit: an unconnected UDP `sendto` leaves no trace in the socket tables. The beacon's
//! destination is the constant group and port, asserted here too.

#![cfg(target_os = "linux")]

use std::collections::HashSet;
use std::net::Ipv4Addr;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use playdrop_core::constants::{FORBIDDEN_PORTS, MULTICAST_GROUP, TCP_PORT, UDP_PORT};

/// The `LocalSend` multicast group.
const LOCALSEND_GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 167);
/// The mDNS port.
const MDNS_PORT: u16 = 5353;
/// How long the sockets are watched once the daemon listens.
const WATCH_FOR: Duration = Duration::from_secs(4);
/// How long the daemon may take to start listening.
const START_WITHIN: Duration = Duration::from_secs(30);

fn forbidden(port: u16) -> bool {
    FORBIDDEN_PORTS.iter().any(|r| r.contains(&port))
}

/// One socket the process owns, from `/proc/<pid>/net/{tcp,tcp6,udp,udp6}`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Sock {
    table: &'static str,
    local_port: u16,
    remote_port: u16,
}

/// Socket inodes behind the process's file descriptors.
fn socket_inodes(pid: u32) -> HashSet<u64> {
    let Ok(dir) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
        return HashSet::new();
    };
    dir.filter_map(|e| std::fs::read_link(e.ok()?.path()).ok())
        .filter_map(|l| {
            let l = l.to_string_lossy().into_owned();
            l.strip_prefix("socket:[")?.strip_suffix(']')?.parse().ok()
        })
        .collect()
}

fn port_of(addr: &str) -> Option<u16> {
    u16::from_str_radix(addr.rsplit_once(':')?.1, 16).ok()
}

/// The process's TCP and UDP sockets, IPv4 and IPv6.
fn sockets(pid: u32) -> Vec<Sock> {
    let inodes = socket_inodes(pid);
    let mut out = Vec::new();
    for table in ["tcp", "tcp6", "udp", "udp6"] {
        let Ok(text) = std::fs::read_to_string(format!("/proc/{pid}/net/{table}")) else {
            continue;
        };
        for line in text.lines().skip(1) {
            let f: Vec<&str> = line.split_whitespace().collect();
            let (Some(local), Some(remote), Some(inode)) = (f.get(1), f.get(2), f.get(9)) else {
                continue;
            };
            if !inode.parse().is_ok_and(|i: u64| inodes.contains(&i)) {
                continue;
            }
            if let (Some(local_port), Some(remote_port)) = (port_of(local), port_of(remote)) {
                out.push(Sock {
                    table,
                    local_port,
                    remote_port,
                });
            }
        }
    }
    out
}

/// IPv4 multicast groups joined on any interface (`/proc/net/igmp`, little-endian hex).
fn igmp_groups() -> HashSet<Ipv4Addr> {
    let text = std::fs::read_to_string("/proc/net/igmp").unwrap_or_default();
    text.lines()
        .filter(|l| l.starts_with(char::is_whitespace))
        .filter_map(|l| l.split_whitespace().next())
        .filter_map(|hex| u32::from_str_radix(hex, 16).ok())
        .map(|v| Ipv4Addr::from(v.to_le_bytes()))
        .collect()
}

fn free_port() -> u16 {
    // Bound and dropped at once; the daemon binds the same number for TCP and UDP.
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start_daemon(tmp: &Path, port: u16) -> Daemon {
    let log = std::fs::File::create(tmp.join("daemon.log")).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_playdropd"))
        .arg("--data-dir")
        .arg(tmp.join("data"))
        .arg("--receive-dir")
        .arg(tmp.join("in"))
        .args(["--port", &port.to_string(), "--name", "coexistence"])
        .args([
            "--no-tray",
            "--no-autostart",
            "--no-clipboard",
            "--no-notifications",
        ])
        .env("HOME", tmp)
        .env("XDG_CONFIG_HOME", tmp.join("config"))
        .env("XDG_DATA_HOME", tmp.join("share"))
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            format!("unix:path={}", tmp.join("no-bus").display()),
        )
        .env("RUST_LOG", "playdropd=info,playdrop_core=info")
        .stdin(Stdio::null())
        .stdout(log.try_clone().unwrap())
        .stderr(log)
        .spawn()
        .expect("playdropd starts");
    Daemon(child)
}

#[test]
fn constants_stay_clear_of_kde_connect_and_localsend() {
    assert!(!forbidden(TCP_PORT) && !forbidden(UDP_PORT));
    assert_ne!(MULTICAST_GROUP, LOCALSEND_GROUP);
    assert!(forbidden(1716) && forbidden(1714) && forbidden(1764) && forbidden(53317));
    assert!(!forbidden(1713) && !forbidden(1765));
}

#[test]
fn daemon_never_binds_or_dials_kde_connect_or_localsend_ports() {
    let tmp = tempfile::tempdir().unwrap();
    let port = free_port();
    let groups_before = igmp_groups();
    let mut daemon = start_daemon(tmp.path(), port);
    let pid = daemon.0.id();
    let log = || std::fs::read_to_string(tmp.path().join("daemon.log")).unwrap_or_default();

    // Wait for the listener.
    let started = Instant::now();
    loop {
        if let Some(status) = daemon.0.try_wait().unwrap() {
            panic!("playdropd exited early ({status}):\n{}", log());
        }
        if sockets(pid)
            .iter()
            .any(|s| s.table.starts_with("tcp") && s.local_port == port)
        {
            break;
        }
        assert!(
            started.elapsed() < START_WITHIN,
            "playdropd did not listen on {port}:\n{}",
            log()
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    // Watch while discovery runs (beacons, mDNS queries and answers).
    let mut seen: HashSet<Sock> = HashSet::new();
    let watch = Instant::now();
    while watch.elapsed() < WATCH_FOR {
        seen.extend(sockets(pid));
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(
        daemon.0.try_wait().unwrap().is_none(),
        "playdropd stopped while watched:\n{}",
        log()
    );

    let bad: Vec<&Sock> = seen
        .iter()
        .filter(|s| forbidden(s.local_port) || forbidden(s.remote_port))
        .collect();
    assert!(
        bad.is_empty(),
        "sockets on KDE Connect or LocalSend ports: {bad:?}"
    );
    // The sockets read are this daemon's, discovery included: the beacon socket (always
    // on UDP_PORT, whatever --port says) or mDNS.
    assert!(
        seen.iter()
            .any(|s| s.table.starts_with("udp") && [UDP_PORT, MDNS_PORT].contains(&s.local_port)),
        "no discovery socket among {seen:?}"
    );

    let joined: HashSet<Ipv4Addr> = igmp_groups().difference(&groups_before).copied().collect();
    assert!(
        !joined.contains(&LOCALSEND_GROUP),
        "joined LocalSend's group {LOCALSEND_GROUP}"
    );
    drop(daemon);
}
