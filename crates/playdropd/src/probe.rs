//! The daemon's half of `Request::Diagnose`: a port self-check, a best-effort firewall
//! reading, and the summary lines a UI shows as is (`docs/ipc.md` section 5, SR-31).
//!
//! The core cannot see a firewall: a dropped inbound connection looks like nobody dialling
//! (`playdrop_core::host::diagnostics`). The probe dials our own port on loopback (proves
//! the listener) and on every LAN interface address. On Linux a connection to one's own LAN
//! address travels over the loopback device, which ufw and firewalld accept, so the LAN
//! dial mostly catches local filters and odd routing; the firewall tools themselves are
//! asked where possible, without privileges.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use playdrop_core::host::{Diagnostics, DiscoveryPath, InterfaceInfo};
use playdrop_ipc::{AddrProbe, FirewallHint, PortProbe};

/// How long one dial of the probe may take.
pub const DIAL_TIMEOUT: Duration = Duration::from_secs(1);

/// How long a firewall tool may take to answer.
const TOOL_TIMEOUT: Duration = Duration::from_secs(3);

/// The line shown when nothing is wrong.
pub const ALL_FINE: &str = "all paths fine";

/// Dials `127.0.0.1:port` and every interface address on `port`, all at once, each with a
/// [`DIAL_TIMEOUT`], and reads the host firewall.
pub async fn probe(port: u16, interfaces: &[InterfaceInfo]) -> PortProbe {
    let loopback = dial(SocketAddr::from(([127, 0, 0, 1], port)));
    let lan = futures_util::future::join_all(interfaces.iter().map(|i| {
        let addr = SocketAddr::new(IpAddr::V4(i.addr), port);
        async move {
            let r = dial(addr).await;
            AddrProbe {
                addr,
                ok: r.is_ok(),
                error: r.err(),
            }
        }
    }));
    let (loopback, lan, firewall) = tokio::join!(loopback, lan, firewall_hint(port));
    PortProbe {
        port,
        loopback_ok: loopback.is_ok(),
        lan,
        firewall,
    }
}

/// The TCP port of KDE Connect and `GSConnect`, the first of their 1714-1764 range.
pub const KDE_CONNECT_PORT: u16 = 1716;

/// An informational `Diagnose` line when another program (KDE Connect or `GSConnect`)
/// listens on [`KDE_CONNECT_PORT`]: running both is fine, `PlayDrop` never uses their
/// ports (protocol.md section 1). Read from the kernel's socket tables, so nothing is
/// sent to that port. Linux only; `None` elsewhere.
pub fn coexistence_note(our_port: u16) -> Option<String> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let listening = ["/proc/net/tcp", "/proc/net/tcp6"]
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .any(|table| listens_on(&table, KDE_CONNECT_PORT));
    listening.then(|| {
        format!(
            "note: port {KDE_CONNECT_PORT} is used by another program, most likely KDE Connect or GSConnect; that is fine, PlayDrop uses port {our_port} and both can stay installed"
        )
    })
}

/// Whether a `/proc/net/tcp` or `tcp6` table has a socket in `LISTEN` (state `0A`) on
/// local `port`.
pub fn listens_on(table: &str, port: u16) -> bool {
    table.lines().skip(1).any(|line| {
        let mut fields = line.split_whitespace().skip(1);
        let (Some(local), Some(_remote), Some(state)) =
            (fields.next(), fields.next(), fields.next())
        else {
            return false;
        };
        state == "0A"
            && local
                .rsplit_once(':')
                .and_then(|(_, p)| u16::from_str_radix(p, 16).ok())
                == Some(port)
    })
}

async fn dial(addr: SocketAddr) -> Result<(), String> {
    match tokio::time::timeout(DIAL_TIMEOUT, tokio::net::TcpStream::connect(addr)).await {
        Ok(Ok(_stream)) => Ok(()),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err("timed out".to_owned()),
    }
}

/// Reads ufw, then firewalld. Windows: nothing yet (the installer adds a program-scoped
/// rule, SR-31; reading it needs COM `INetFwPolicy2`, Phase 2 follow-up).
async fn firewall_hint(port: u16) -> Option<FirewallHint> {
    if cfg!(windows) {
        return None;
    }
    if let Some(ufw) = find_tool("ufw") {
        let out = run(&ufw, &["status", "verbose"]).await;
        let conf = std::fs::read_to_string("/etc/ufw/ufw.conf").ok();
        if let Some(h) = parse_ufw(out.as_ref(), conf.as_deref(), port) {
            return Some(h);
        }
    }
    if let Some(fw) = find_tool("firewall-cmd") {
        let state = run(&fw, &["--state"]).await;
        let query_arg = format!("--query-port={port}/tcp");
        let query = match &state {
            Some(s) if s.stdout.trim() == "running" => run(&fw, &[query_arg.as_str()]).await,
            _ => None,
        };
        return parse_firewalld(state.as_ref(), query.as_ref(), port);
    }
    None
}

/// Output of a firewall tool.
#[derive(Debug, Clone, Default)]
struct ToolOutput {
    stdout: String,
    stderr: String,
}

async fn run(tool: &std::path::Path, args: &[&str]) -> Option<ToolOutput> {
    let child = tokio::process::Command::new(tool)
        .args(args)
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    match tokio::time::timeout(TOOL_TIMEOUT, child).await {
        Ok(Ok(out)) => Some(ToolOutput {
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }),
        Ok(Err(e)) => {
            tracing::debug!(tool = %tool.display(), error = %e, "firewall tool did not run");
            None
        }
        Err(_) => {
            tracing::debug!(tool = %tool.display(), "firewall tool timed out");
            None
        }
    }
}

/// Finds a tool on `PATH` or in the sbin directories, which a non-root `PATH` often lacks.
fn find_tool(name: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .chain(["/usr/sbin", "/sbin", "/usr/local/sbin"].map(std::path::PathBuf::from))
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

fn ufw_fix(port: u16) -> String {
    format!("sudo ufw allow {port}")
}

fn firewalld_fix(port: u16) -> String {
    format!(
        "sudo firewall-cmd --permanent --add-port={port}/tcp --add-port={port}/udp && sudo firewall-cmd --reload"
    )
}

/// Interprets `ufw status verbose`. Three shapes: "Status: active" with the rule list,
/// "Status: inactive", or a refusal because we are not root, in which case
/// `/etc/ufw/ufw.conf` (world-readable) tells whether it is enabled and the port state
/// stays unknown.
fn parse_ufw(out: Option<&ToolOutput>, conf: Option<&str>, port: u16) -> Option<FirewallHint> {
    let hint = |active: bool, port_allowed: Option<bool>| FirewallHint {
        tool: "ufw".to_owned(),
        active,
        port_allowed,
        fix: ufw_fix(port),
    };
    if let Some(o) = out {
        let text = o.stdout.as_str();
        if text.lines().any(|l| l.trim() == "Status: inactive") {
            return Some(hint(false, None));
        }
        if text.lines().any(|l| l.trim() == "Status: active") {
            return Some(hint(true, Some(ufw_allows(text, port))));
        }
    }
    let enabled = conf?.lines().any(|l| {
        let l = l.trim();
        !l.starts_with('#') && l.replace(' ', "").eq_ignore_ascii_case("ENABLED=yes")
    });
    if let Some(o) = out {
        tracing::debug!(stderr = %o.stderr.trim(), "ufw status refused; reading ufw.conf");
    }
    Some(hint(enabled, None))
}

/// True if the default incoming policy is allow, or an ALLOW rule names the port (alone,
/// with a protocol, or inside a range) for both TCP and UDP or without a protocol.
fn ufw_allows(status: &str, port: u16) -> bool {
    if status
        .lines()
        .any(|l| l.trim_start().starts_with("Default:") && l.contains("allow (incoming)"))
    {
        return true;
    }
    status.lines().any(|line| {
        let mut cols = line.split_whitespace();
        let Some(target) = cols.next() else {
            return false;
        };
        let rest: Vec<&str> = cols.collect();
        let allow = rest.first().is_some_and(|a| a.starts_with("ALLOW"));
        let inbound = rest.get(1).is_none_or(|d| *d != "OUT");
        allow && inbound && ufw_target_covers(target, port)
    })
}

/// "42187", "42187/tcp", "42000:43000/tcp" or a comma list of those.
fn ufw_target_covers(target: &str, port: u16) -> bool {
    let (ports, proto) = target.split_once('/').unwrap_or((target, ""));
    if !(proto.is_empty() || proto == "tcp") {
        return false;
    }
    ports.split(',').any(|p| match p.split_once(':') {
        Some((a, b)) => match (a.parse::<u16>(), b.parse::<u16>()) {
            (Ok(a), Ok(b)) => (a..=b).contains(&port),
            _ => false,
        },
        None => p.parse::<u16>() == Ok(port),
    })
}

/// Interprets `firewall-cmd --state` ("running" or "not running") and
/// `--query-port=<port>/tcp` ("yes" or "no"; anything else, such as a polkit refusal,
/// leaves the port state unknown).
fn parse_firewalld(
    state: Option<&ToolOutput>,
    query: Option<&ToolOutput>,
    port: u16,
) -> Option<FirewallHint> {
    let state = state?;
    let active = state.stdout.trim() == "running";
    let port_allowed = query.and_then(|q| match q.stdout.trim() {
        "yes" => Some(true),
        "no" => Some(false),
        _ => None,
    });
    Some(FirewallHint {
        tool: "firewalld".to_owned(),
        active,
        port_allowed,
        fix: firewalld_fix(port),
    })
}

/// The summary lines, most important first, each ready to show. Names the failed layer:
/// listener, firewall, LAN reachability, interfaces, multicast, mDNS, then each unreached
/// peer with the discovery path that failed. One line [`ALL_FINE`] when nothing is wrong.
pub fn summary(core: &Diagnostics, probe: &PortProbe) -> Vec<String> {
    let port = probe.port;
    let mut out = Vec::new();

    if !core.bind_ok {
        out.push(format!(
            "PlayDrop is not listening on port {port}: restart the daemon"
        ));
    } else if !probe.loopback_ok {
        out.push(format!(
            "port {port} does not answer on this computer: another program or a local filter is in the way"
        ));
    }

    let lan_failed: Vec<&AddrProbe> = probe.lan.iter().filter(|a| !a.ok).collect();
    let mut firewall_named = false;
    if let Some(fw) = probe.firewall.as_ref().filter(|f| f.active) {
        match fw.port_allowed {
            Some(true) => {}
            Some(false) => {
                out.push(format!(
                    "port {port} blocked by the host firewall ({}): run `{}`",
                    fw.tool, fw.fix
                ));
                firewall_named = true;
            }
            None if !lan_failed.is_empty() => {
                out.push(format!(
                    "port {port} blocked by the host firewall ({}): run `{}`",
                    fw.tool, fw.fix
                ));
                firewall_named = true;
            }
            None => {
                out.push(format!(
                    "port {port} may be blocked by the host firewall ({}, rules unreadable without root): run `{}`",
                    fw.tool, fw.fix
                ));
                firewall_named = true;
            }
        }
    }
    if !firewall_named {
        for a in &lan_failed {
            out.push(format!(
                "port {port} blocked on {}: {}",
                a.addr.ip(),
                a.error.as_deref().unwrap_or("no answer")
            ));
        }
    }

    if core.interfaces.is_empty() {
        out.push("no network interface is up".to_owned());
    }

    if core.multicast.enabled {
        for j in &core.multicast.join_failed {
            out.push(format!("multicast blocked on {}: {}", j.addr, j.error));
        }
        if !core.multicast.beacon_recently {
            for i in &core.interfaces {
                let online_here = core.peers.iter().find(|p| {
                    p.online && !p.beacon_seen && p.last_known.iter().any(|a| same_subnet(a, i))
                });
                if let Some(p) = online_here {
                    out.push(format!(
                        "multicast blocked on {}: no beacon heard although {} is connected on this network",
                        i.addr, p.label
                    ));
                }
            }
        }
    } else if !core.mdns.enabled {
        out.push("discovery is off: devices are found only at their last-known address".to_owned());
    }
    if core.mdns.enabled {
        if let Some(e) = &core.mdns.error {
            out.push(format!("mDNS failed: {e}"));
        }
    }

    for p in core.peers.iter().filter(|p| !p.online) {
        let reachable_net = p.last_known.is_empty()
            || p.last_known
                .iter()
                .any(|a| core.interfaces.iter().any(|i| same_subnet(a, i)));
        if !reachable_net && !core.interfaces.is_empty() {
            out.push(format!("no interface on the same network as {}", p.label));
            continue;
        }
        if let Some(f) = &p.last_result {
            out.push(format!(
                "{} not reached: {} failed ({})",
                p.label,
                path_name(f.path),
                f.failure
            ));
        }
    }

    if out.is_empty() {
        out.push(ALL_FINE.to_owned());
    }
    out
}

/// The discovery path as the user reads it.
pub fn path_name(p: DiscoveryPath) -> &'static str {
    match p {
        DiscoveryPath::Mdns => "mDNS",
        DiscoveryPath::Beacon => "multicast beacon",
        DiscoveryPath::LastKnown => "last-known address",
    }
}

fn same_subnet(addr: &SocketAddr, i: &InterfaceInfo) -> bool {
    match addr.ip() {
        IpAddr::V4(a) => {
            let m = u32::from(i.netmask);
            u32::from(a) & m == u32::from(i.addr) & m
        }
        IpAddr::V6(a) => a.to_ipv4_mapped().is_some_and(|a| {
            let m = u32::from(i.netmask);
            u32::from(a) & m == u32::from(i.addr) & m
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use playdrop_core::host::{
        DialFailure, JoinFailure, MdnsStatus, MulticastStatus, PeerDiagnostic,
    };

    use super::*;

    fn iface() -> InterfaceInfo {
        InterfaceInfo {
            index: 2,
            addr: Ipv4Addr::new(192, 168, 1, 10),
            netmask: Ipv4Addr::new(255, 255, 255, 0),
        }
    }

    fn core() -> Diagnostics {
        Diagnostics {
            addr: "[::]:42187".parse().unwrap(),
            device_id: "ab".repeat(32),
            short_id: "k3qd-7f2m-xa9p-41bc".into(),
            port: 42187,
            bind_ok: true,
            interfaces: vec![iface()],
            multicast: MulticastStatus {
                enabled: true,
                joined: vec![Ipv4Addr::new(192, 168, 1, 10)],
                join_failed: vec![],
                beacon_recently: true,
                last_beacon_secs_ago: Some(3),
            },
            mdns: MdnsStatus {
                enabled: true,
                registered: true,
                error: None,
                peer_seen: true,
                last_seen_secs_ago: Some(3),
            },
            peers: vec![],
        }
    }

    fn probe_ok() -> PortProbe {
        PortProbe {
            port: 42187,
            loopback_ok: true,
            lan: vec![AddrProbe {
                addr: "192.168.1.10:42187".parse().unwrap(),
                ok: true,
                error: None,
            }],
            firewall: None,
        }
    }

    fn peer(label: &str, addr: &str, online: bool) -> PeerDiagnostic {
        PeerDiagnostic {
            peer: "cd".repeat(32),
            short_id: "aaaa-bbbb-cccc-dddd".into(),
            label: label.into(),
            online,
            last_known: vec![addr.parse().unwrap()],
            mdns_seen: false,
            beacon_seen: false,
            last_result: None,
        }
    }

    #[test]
    fn healthy_system_says_all_paths_fine() {
        assert_eq!(summary(&core(), &probe_ok()), vec![ALL_FINE.to_owned()]);
    }

    #[test]
    fn ufw_blocking_the_port_is_named_with_the_fix() {
        let mut p = probe_ok();
        p.firewall = Some(FirewallHint {
            tool: "ufw".into(),
            active: true,
            port_allowed: Some(false),
            fix: ufw_fix(42187),
        });
        assert_eq!(
            summary(&core(), &p),
            vec!["port 42187 blocked by the host firewall (ufw): run `sudo ufw allow 42187`"]
        );
        p.firewall.as_mut().unwrap().port_allowed = Some(true);
        assert_eq!(summary(&core(), &p), vec![ALL_FINE.to_owned()]);
        p.firewall.as_mut().unwrap().port_allowed = None;
        p.lan[0].ok = false;
        p.lan[0].error = Some("timed out".into());
        assert_eq!(
            summary(&core(), &p),
            vec!["port 42187 blocked by the host firewall (ufw): run `sudo ufw allow 42187`"]
        );
    }

    #[test]
    fn multicast_and_peer_failures_name_the_layer() {
        let mut c = core();
        c.multicast.join_failed = vec![JoinFailure {
            addr: Ipv4Addr::new(10, 0, 0, 5),
            error: "No such device".into(),
        }];
        c.multicast.beacon_recently = false;
        let mut phone = peer("Phone", "192.168.1.20:42187", false);
        phone.last_result = Some(DialFailure {
            path: DiscoveryPath::LastKnown,
            failure: "connection refused".into(),
            at: 0,
        });
        c.peers = vec![
            phone,
            peer("Laptop", "192.168.1.30:42187", true),
            peer("Tablet", "10.9.0.4:42187", false),
        ];
        let lines = summary(&c, &probe_ok());
        assert_eq!(
            lines,
            vec![
                "multicast blocked on 10.0.0.5: No such device".to_owned(),
                "multicast blocked on 192.168.1.10: no beacon heard although Laptop is connected on this network".to_owned(),
                "Phone not reached: last-known address failed (connection refused)".to_owned(),
                "no interface on the same network as Tablet".to_owned(),
            ]
        );
    }

    #[test]
    fn listener_down_comes_first() {
        let mut c = core();
        c.bind_ok = false;
        c.interfaces.clear();
        let lines = summary(&c, &probe_ok());
        assert_eq!(
            lines[0],
            "PlayDrop is not listening on port 42187: restart the daemon"
        );
        assert!(lines.contains(&"no network interface is up".to_owned()));
    }

    #[test]
    fn ufw_output_shapes() {
        let active = ToolOutput {
            stdout: "Status: active\nLogging: on (low)\nDefault: deny (incoming), allow (outgoing), disabled (routed)\n\nTo                         Action      From\n--                         ------      ----\n22/tcp                     ALLOW IN    Anywhere\n42000:42200/tcp            ALLOW IN    Anywhere\n".into(),
            stderr: String::new(),
        };
        let h = parse_ufw(Some(&active), None, 42187).unwrap();
        assert!(h.active);
        assert_eq!(h.port_allowed, Some(true));
        assert_eq!(
            parse_ufw(Some(&active), None, 42300).unwrap().port_allowed,
            Some(false)
        );

        let inactive = ToolOutput {
            stdout: "Status: inactive\n".into(),
            stderr: String::new(),
        };
        assert!(!parse_ufw(Some(&inactive), None, 42187).unwrap().active);

        let refused = ToolOutput {
            stdout: String::new(),
            stderr: "ERROR: You need to be root to run this script\n".into(),
        };
        let h = parse_ufw(Some(&refused), Some("# comment\nENABLED=yes\n"), 42187).unwrap();
        assert!(h.active);
        assert_eq!(h.port_allowed, None);
        let h = parse_ufw(Some(&refused), Some("ENABLED=no\n"), 42187).unwrap();
        assert!(!h.active);
        assert!(parse_ufw(Some(&refused), None, 42187).is_none());
    }

    #[test]
    fn firewalld_output_shapes() {
        let running = ToolOutput {
            stdout: "running\n".into(),
            stderr: String::new(),
        };
        let no = ToolOutput {
            stdout: "no\n".into(),
            stderr: String::new(),
        };
        let h = parse_firewalld(Some(&running), Some(&no), 42187).unwrap();
        assert!(h.active);
        assert_eq!(h.port_allowed, Some(false));
        let denied = ToolOutput {
            stdout: String::new(),
            stderr: "Authorization failed.\n".into(),
        };
        assert_eq!(
            parse_firewalld(Some(&running), Some(&denied), 42187)
                .unwrap()
                .port_allowed,
            None
        );
        let stopped = ToolOutput {
            stdout: "not running\n".into(),
            stderr: String::new(),
        };
        assert!(!parse_firewalld(Some(&stopped), None, 42187).unwrap().active);
    }

    #[tokio::test]
    async fn probe_finds_a_live_listener_on_loopback() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let p = probe(port, &[]).await;
        assert!(p.loopback_ok);
        drop(l);
        let p = probe(port, &[]).await;
        assert!(!p.loopback_ok);
    }

    #[test]
    fn listening_sockets_are_read_from_the_proc_table() {
        let table = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 00000000:06B4 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 4242 1 0000000000000000 100 0 0 10 0
   1: 0100007F:A4CB 0100007F:06B4 01 00000000:00000000 00:00000000 00000000  1000        0 4243 1 0000000000000000 20 4 30 10 -1
";
        assert!(listens_on(table, KDE_CONNECT_PORT));
        assert!(!listens_on(table, 42187));
        // A connection to 1716 is not a listener on it.
        let connected = "header\n   1: 0100007F:A4CB 0100007F:06B4 01 0 0 0 0 0 0\n";
        assert!(!listens_on(connected, KDE_CONNECT_PORT));
        let v6 = "header\n   0: 00000000000000000000000000000000:06B4 00000000000000000000000000000000:0000 0A 0\n";
        assert!(listens_on(v6, KDE_CONNECT_PORT));
        assert!(!listens_on("", KDE_CONNECT_PORT));
    }
}
