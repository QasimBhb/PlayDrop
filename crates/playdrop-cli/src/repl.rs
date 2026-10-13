//! `playdrop host`: a foreground host with an event printer and a stdin command loop.

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context as _};
use playdrop_core::host::{
    Host, HostConfig, HostEvent, KeepAliveProfile, PairingPromptKind, SendSource, TransferDirection,
};
use playdrop_core::identity::DeviceId;
use tokio::io::AsyncBufReadExt as _;

use crate::HostArgs;

const MIB: f64 = 1024.0 * 1024.0;

fn default_receive_dir() -> PathBuf {
    std::env::var_os("HOME").map_or_else(
        || PathBuf::from("PlayDrop-received"),
        |h| PathBuf::from(h).join("Downloads").join("PlayDrop"),
    )
}

fn default_name() -> String {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_else(|| "PlayDrop CLI".to_owned())
}

/// Runs the host until Ctrl-C or `quit`.
pub async fn run(args: HostArgs) -> anyhow::Result<()> {
    let data_dir = match args.data_dir {
        Some(d) => d,
        None => playdrop_core::identity::default_dir()
            .ok_or_else(|| anyhow!("no data folder; pass --data-dir"))?,
    };
    let receive_dir = args.receive_dir.unwrap_or_else(default_receive_dir);
    let mut cfg = HostConfig::new(
        &data_dir,
        &receive_dir,
        args.name.unwrap_or_else(default_name),
    );
    cfg.port = args.port;
    cfg.bind_ip = args.bind;
    cfg.discovery = !args.no_discovery;
    if args.mobile {
        cfg.keep_alive_profile = KeepAliveProfile::Mobile;
    }
    let (host, mut events) = match Host::start(cfg).await {
        Ok(v) => v,
        Err(playdrop_core::Error::PortHeld { port }) => {
            bail!("port {port} is held by another process (another PlayDrop host?); pass --port")
        }
        Err(e) => return Err(e).context("host did not start"),
    };
    let host = Arc::new(host);
    let json = args.json;
    println!(
        "playdrop host {} ({}) on {}; receiving into {}. Type `help` for commands.",
        host.short_id(),
        host.device_id(),
        host.local_addr(),
        receive_dir.display()
    );
    let printer = tokio::spawn(async move {
        while let Some(ev) = events.recv().await {
            print_event(&ev, json);
        }
    });

    let stdin = tokio::io::BufReader::new(tokio::io::stdin());
    let mut lines = stdin.lines();
    loop {
        let line = tokio::select! {
            l = lines.next_line() => l?,
            _ = tokio::signal::ctrl_c() => None,
        };
        let Some(line) = line else { break };
        let words = split_args(&line);
        if words.is_empty() {
            continue;
        }
        if words[0] == "quit" || words[0] == "exit" {
            break;
        }
        if let Err(e) = command(&host, &words, json).await {
            println!("error: {e:#}");
        }
    }
    printer.abort();
    host.shutdown().await;
    Ok(())
}

/// Splits a command line on whitespace, keeping "double quoted" words together.
fn split_args(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut any = false;
    for c in line.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c => {
                cur.push(c);
                any = true;
            }
        }
    }
    if any {
        out.push(cur);
    }
    out
}

fn peer(host: &Host, q: &str) -> anyhow::Result<DeviceId> {
    host.resolve_peer(q)
        .ok_or_else(|| anyhow!("no single paired device matches {q:?}; see `peers`"))
}

#[allow(
    clippy::too_many_lines,
    reason = "one match arm per stdin command reads best"
)]
async fn command(host: &Arc<Host>, w: &[String], json: bool) -> anyhow::Result<()> {
    match (w[0].as_str(), w.len()) {
        ("help", _) => println!(
            "commands: peers | pair | pair <ip:port> | pair --qr | pair --scan <uri> | list | y | n | \
             send <peer> <path>... | accept <id> | decline <id> | cancel <id> | transfers | \
             grants <peer> [+g|-g]... | \
             outbox <peer> <path>... | outbox | outbox rm <id> | pull <peer> | \
             ping <peer> [msg] | find <peer> [stop] | clip <peer> <text> | unpair <peer> | \
             media <peer> <player> <play|pause|playPause|stop|next|previous|seek ms|volume n> | \
             input <peer> <start|stop|move dx dy|click|right|scroll dy|key <name>|text <words>|next|previous> | \
             inputs | stop-input | quit"
        ),
        ("peers", _) => {
            let peers = host.peers();
            if peers.is_empty() {
                println!("no paired devices; use `pair`");
            }
            for p in peers {
                println!(
                    "{}  {:<20} {}  last-known {:?}",
                    p.short_id,
                    p.label,
                    if p.online { "online " } else { "offline" },
                    p.last_known
                );
            }
        }
        ("pair", 1) => {
            host.open_add_device_window()?;
            println!(
                "Add device is open for 120 s. On the other device run `pair {}`, or `list` there.",
                host.local_addr()
            );
        }
        ("pair", 2) if w[1] == "--qr" => {
            let uri = host.show_qr()?;
            println!("QR pairing URI (one use, 120 s; keep it private):\n{uri}");
        }
        ("pair", 3) if w[1] == "--scan" => {
            host.scan_qr(&w[2]).await?;
            println!("pairing request sent; confirm on the other device");
        }
        ("pair", 2) => {
            let addr = w[1]
                .parse()
                .with_context(|| format!("{:?} is not ip:port", w[1]))?;
            host.pair_with(addr).await?;
        }
        ("list", _) => {
            let list = host.add_device_list();
            if list.is_empty() {
                println!("nothing seen; open `pair` here and on the other device");
            }
            for c in list {
                println!("{}  {}  {}", c.addr, c.device_type, c.name);
            }
        }
        ("y" | "yes", 1) => host.pairing_respond(true),
        ("n" | "no", 1) => host.pairing_respond(false),
        ("send", n) if n >= 3 => {
            let id = peer(host, &w[1])?;
            let paths: Vec<PathBuf> = w[2..].iter().map(PathBuf::from).collect();
            let handle = host.send(&id, paths).await?;
            tokio::spawn(watch_send(handle, json));
        }
        ("outbox", 1) => {
            let items = host.outbox();
            if json {
                println!("{}", serde_json::to_string(&items).unwrap_or_default());
            } else if items.is_empty() {
                println!("outbox is empty");
            }
            for i in items.iter().filter(|_| !json) {
                println!(
                    "{}  for {}  {} file(s), {} bytes{}",
                    i.id,
                    i.peer.chars().take(8).collect::<String>(),
                    i.file_count,
                    i.total_bytes,
                    i.note.as_deref().map(|n| format!("  ({n})")).unwrap_or_default()
                );
            }
        }
        ("outbox", 3) if w[1] == "rm" => {
            host.remove_outbox(&w[2]).await?;
            println!("removed {}", w[2]);
        }
        ("outbox", n) if n >= 3 => {
            let id = peer(host, &w[1])?;
            let sources = w[2..]
                .iter()
                .map(|p| SendSource::Path(PathBuf::from(p)))
                .collect();
            let item = host.queue_outbox(&id, sources, None).await?;
            println!("queued {item}; the device pulls it when it connects (`outbox` lists)");
        }
        ("pull", 2) => {
            let id = peer(host, &w[1])?;
            let ids = host.pull_outbox(&id).await?;
            if ids.is_empty() {
                println!("{} holds nothing for this device", w[1]);
            }
            for t in ids {
                println!("pulling {t}");
            }
        }
        ("accept" | "decline", 2) => {
            host.respond_offer(&w[1], w[0] == "accept").await?;
        }
        ("cancel", 2) => host.cancel_transfer(&w[1]).await?,
        ("transfers", _) => {
            let list = host.transfers();
            if json {
                println!("{}", serde_json::to_string(&list).unwrap_or_default());
            } else if list.is_empty() {
                println!("no transfers");
            }
            for t in list.iter().filter(|_| !json) {
                println!(
                    "{}  {:?} {:<9} {} / {} bytes  {} file(s)  {}{}",
                    t.id,
                    t.direction,
                    format!("{:?}", t.state).to_lowercase(),
                    t.bytes,
                    t.total,
                    t.file_count,
                    t.short_id,
                    t.note.as_deref().map(|n| format!("  ({n})")).unwrap_or_default()
                );
            }
        }
        ("grants", n) if n >= 2 => {
            let id = peer(host, &w[1])?;
            let mut g = host.grants(&id).ok_or_else(|| anyhow!("not paired"))?;
            for change in &w[2..] {
                let (on, name) = match change.split_at(1) {
                    ("+", n) => (true, n),
                    ("-", n) => (false, n),
                    _ => bail!("use +grant or -grant"),
                };
                match name {
                    "files" => g.files = on,
                    "clipboardIn" => g.clipboard_in = on,
                    "clipboardOut" => g.clipboard_out = on,
                    "input" => g.input = on,
                    "media" => g.media = on,
                    "find" => g.find = on,
                    other => bail!("unknown grant {other}"),
                }
            }
            if w.len() > 2 {
                host.set_grants(&id, g).await?;
            }
            println!("{g:?}");
        }
        ("ping", n) if n >= 2 => {
            let id = peer(host, &w[1])?;
            let msg = (n > 2).then(|| w[2..].join(" "));
            host.ping(&id, msg.as_deref())?;
        }
        ("find", 2) => host.find(&peer(host, &w[1])?, true)?,
        ("find", 3) if w[2] == "stop" => host.find(&peer(host, &w[1])?, false)?,
        ("clip", n) if n >= 3 => {
            let id = peer(host, &w[1])?;
            if !host.send_clipboard(&id, &w[2..].join(" "), false)? {
                println!("not sent: same as the last clipboard");
            }
        }
        ("media", n) if n >= 4 => {
            use playdrop_core::session::types::MediaAction;
            let id = peer(host, &w[1])?;
            let action = MediaAction::from_wire(&w[3]);
            let value = w.get(4).and_then(|v| v.parse::<u64>().ok());
            host.send_media_command(&id, &w[2], action, value)?;
        }
        ("input", n) if n >= 3 => {
            use playdrop_core::session::types::{
                ButtonAction, Input, KeyAction, KeyInput, PointerButton, PointerInput,
                PresenterAction, SessionOp,
            };
            let id = peer(host, &w[1])?;
            let num = |i: usize| w.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
            let input = match w[2].as_str() {
                "start" => Input::Session {
                    op: SessionOp::Start,
                },
                "stop" => Input::Session {
                    op: SessionOp::Stop,
                },
                "move" => Input::Pointer(PointerInput::Move {
                    dx: num(3),
                    dy: num(4),
                }),
                "click" | "right" => Input::Pointer(PointerInput::Button {
                    button: if w[2] == "right" {
                        PointerButton::Right
                    } else {
                        PointerButton::Left
                    },
                    action: ButtonAction::Click,
                }),
                "scroll" => Input::Pointer(PointerInput::Scroll {
                    dx: 0.0,
                    dy: num(3),
                    unit: None,
                }),
                "key" if n >= 4 => Input::Key(KeyInput {
                    key: Some(w[3].clone()),
                    text: None,
                    mods: vec![],
                    action: KeyAction::Press,
                }),
                "text" if n >= 4 => Input::Key(KeyInput {
                    key: None,
                    text: Some(w[3..].join(" ")),
                    mods: vec![],
                    action: KeyAction::Press,
                }),
                "next" | "previous" => Input::Presenter {
                    action: if w[2] == "next" {
                        PresenterAction::Next
                    } else {
                        PresenterAction::Previous
                    },
                },
                other => {
                    println!("unknown input `{other}`; see help");
                    return Ok(());
                }
            };
            host.send_input(&id, input)?;
        }
        ("inputs", _) => {
            for p in host.input_sessions() {
                println!("remote input session from {}", p.short_id());
            }
        }
        ("stop-input", _) => {
            println!("ended {} session(s)", host.stop_input_sessions().len());
        }
        ("unpair", 2) => {
            let id = peer(host, &w[1])?;
            host.unpair(&id).await?;
        }
        _ => println!("unknown command; type `help`"),
    }
    Ok(())
}

/// Prints a progress line (bytes, MB/s, ETA) until the send ends, then the roots.
async fn watch_send(mut handle: playdrop_core::host::TransferHandle, json: bool) {
    let total = handle.total_bytes();
    let started = Instant::now();
    let mut last = (Instant::now(), 0u64);
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    let result = loop {
        tokio::select! {
            r = handle.wait() => break r,
            _ = tick.tick() => {
                if json {
                    continue;
                }
                let sent = handle.bytes_sent().min(total);
                let now = Instant::now();
                #[allow(clippy::cast_precision_loss)] // display only
                let rate = (sent - last.1) as f64 / now.duration_since(last.0).as_secs_f64().max(0.001);
                last = (now, sent);
                #[allow(clippy::cast_precision_loss)] // display only
                let eta = if rate > 0.0 { (total - sent) as f64 / rate } else { f64::INFINITY };
                #[allow(clippy::cast_precision_loss)] // display only
                let line = format!(
                    "\rsend {}: {:.1} / {:.1} MiB  {:.1} MB/s  ETA {:.0} s   ",
                    &handle.id()[..8],
                    sent as f64 / MIB,
                    total as f64 / MIB,
                    rate / 1e6,
                    eta
                );
                print!("{line}");
                let _ = std::io::stdout().flush();
            }
        }
    };
    match result {
        Ok(s) if json => println!("{}", serde_json::to_string(&s).unwrap_or_default()),
        Ok(s) => {
            #[allow(clippy::cast_precision_loss)] // display only
            let rate = s.bytes as f64 / started.elapsed().as_secs_f64() / 1e6;
            println!(
                "\rsend {} done: {} bytes in {:.2} s ({rate:.1} MB/s), {} reconnect(s)",
                &s.id[..8],
                s.bytes,
                started.elapsed().as_secs_f64(),
                s.reconnects
            );
            for f in s.roots.iter().filter(|f| f.root.is_some()) {
                println!("  root {}  {}", f.root.as_deref().unwrap_or(""), f.path);
            }
            if let Some(b) = s.bundle_root {
                let n = s
                    .roots
                    .iter()
                    .filter(|f| f.root.is_none() && f.size > 0)
                    .count();
                println!("  root {b}  (bundle of {n} small files)");
            }
        }
        Err(e) => println!("\rsend {} failed: {e}", &handle.id()[..8]),
    }
}

#[allow(clippy::too_many_lines, reason = "one match arm per event reads best")]
fn print_event(ev: &HostEvent, json: bool) {
    if json {
        println!("{}", serde_json::to_string(ev).unwrap_or_default());
        return;
    }
    let short = |hex: &str| hex.chars().take(8).collect::<String>();
    match ev {
        HostEvent::PeerOnline {
            short_id,
            name,
            addr,
            ..
        } => {
            println!("online: {name} ({short_id}) at {addr}");
        }
        HostEvent::PeerOffline { short_id, .. } => println!("offline: {short_id}"),
        HostEvent::PairingPrompt {
            short_id,
            name,
            prompt,
            ..
        } => match prompt {
            PairingPromptKind::Sas { sas } => println!(
                "PAIR with {name} ({short_id})? Check both screens show {} then type y or n",
                sas.0
            ),
            PairingPromptKind::ScannerConfirm => {
                println!("ADD {name} ({short_id})? It scanned this host's code. Type y or n");
            }
        },
        HostEvent::Paired { short_id, name, .. } => println!("paired with {name} ({short_id})"),
        HostEvent::PairingFailed { reason } => println!("pairing failed: {reason}"),
        HostEvent::RemovedByPeer { short_id, .. } => {
            println!("{short_id} no longer has this device paired; pair again");
        }
        HostEvent::Unpaired { short_id, .. } => println!("unpaired {short_id}"),
        HostEvent::AddDeviceSeen {
            name,
            device_type,
            addr,
        } => {
            println!("seen in Add device: {name} ({device_type}) at {addr}; `pair {addr}`");
        }
        HostEvent::TransferOffer {
            id,
            short_id,
            files,
            file_count,
            total_bytes,
            note,
            ..
        } => {
            println!(
                "offer {id} from {short_id}: {file_count} file(s), {total_bytes} bytes{}. `accept {id}` or `decline {id}`",
                note.as_deref().map(|n| format!(" ({n})")).unwrap_or_default()
            );
            for f in files.iter().take(10) {
                println!("  {}  ({} bytes)", f.path, f.size);
            }
            if *file_count > 10 {
                println!("  ... and {} more", file_count - 10);
            }
        }
        HostEvent::TransferProgress {
            id,
            direction: TransferDirection::Receive,
            bytes,
            total,
            bytes_per_sec,
            ..
        } => {
            #[allow(clippy::cast_precision_loss)] // display only
            let line = format!(
                "receive {}: {:.1} / {:.1} MiB  {:.1} MB/s",
                &id[..8.min(id.len())],
                *bytes as f64 / MIB,
                *total as f64 / MIB,
                *bytes_per_sec as f64 / 1e6
            );
            println!("{line}");
        }
        HostEvent::TransferComplete {
            id,
            direction: TransferDirection::Receive,
            bytes,
            elapsed_ms,
            saved,
            ..
        } => {
            println!(
                "received {} ({bytes} bytes in {elapsed_ms} ms)",
                &id[..8.min(id.len())]
            );
            for p in saved.iter().take(10) {
                println!("  saved {}", p.display());
            }
            if saved.len() > 10 {
                println!("  ... and {} more", saved.len() - 10);
            }
        }
        HostEvent::TransferFailed {
            id,
            direction,
            reason,
            ..
        } => {
            println!("{direction:?} {} failed: {reason}", &id[..8.min(id.len())]);
        }
        HostEvent::Clipboard {
            peer,
            text,
            sensitive,
        } => {
            if *sensitive {
                println!("clipboard from {}: (sensitive, not shown)", short(peer));
            } else {
                println!(
                    "clipboard from {}: {}",
                    short(peer),
                    text.0.as_deref().unwrap_or("(image)")
                );
            }
        }
        HostEvent::ClipboardImage {
            peer,
            mime,
            data,
            sensitive,
        } => println!(
            "clipboard image from {}: {mime}, {} bytes{}",
            short(peer),
            data.0 .0.len(),
            if *sensitive { " (sensitive)" } else { "" }
        ),
        HostEvent::OutboxReady {
            peer, count, bytes, ..
        } => println!(
            "{} holds {count} item(s), {bytes} bytes for this device; `pull {}`",
            short(peer),
            short(peer)
        ),
        HostEvent::Ping { peer, message } => println!(
            "ping from {}{}",
            short(peer),
            message
                .as_deref()
                .map(|m| format!(": {m}"))
                .unwrap_or_default()
        ),
        HostEvent::Find { peer, start } => println!(
            "find from {} ({})",
            short(peer),
            if *start { "ring" } else { "stop" }
        ),
        HostEvent::Battery {
            peer,
            level,
            charging,
            ..
        } => {
            println!(
                "battery {}: {level}%{}",
                short(peer),
                if *charging { " charging" } else { "" }
            );
        }
        HostEvent::Presence { peer, state } => println!("presence {}: {state}", short(peer)),
        HostEvent::Unsupported { peer, frame_type } => {
            println!(
                "{} sent `{frame_type}`, which this build does not handle yet",
                short(peer)
            );
        }
        HostEvent::DiscoveryDiagnostic {
            short_id,
            path,
            failure,
            ..
        } => {
            println!("cannot reach {short_id} via {path:?}: {failure}");
        }
        HostEvent::MediaState { peer, state } => {
            let gone = !state.playing && state.can.is_empty();
            println!(
                "media {}: {} {} {}{}",
                short(peer),
                state.name.as_deref().unwrap_or(&state.player),
                if gone {
                    "gone"
                } else if state.playing {
                    "playing"
                } else {
                    "paused"
                },
                state.title.as_deref().unwrap_or("-"),
                state
                    .artist
                    .as_deref()
                    .map(|a| format!(" by {a}"))
                    .unwrap_or_default()
            );
        }
        HostEvent::Input { peer, input } => {
            // The CLI applies nothing; it shows the kind only (SR-32: never the text).
            let kind = match input {
                playdrop_core::session::types::Input::Session { op } => format!("session {op:?}"),
                playdrop_core::session::types::Input::Pointer(_) => "pointer".to_owned(),
                playdrop_core::session::types::Input::Key(_) => "key".to_owned(),
                playdrop_core::session::types::Input::Presenter { action } => {
                    format!("presenter {action:?}")
                }
            };
            println!("input from {}: {kind}", short(peer));
        }
        HostEvent::PeerRefused {
            peer,
            frame_type,
            code,
            message,
        } => println!(
            "{} refused {}: {code}{}",
            short(peer),
            frame_type.as_deref().unwrap_or("a frame"),
            message
                .as_deref()
                .map(|m| format!(" ({m})"))
                .unwrap_or_default()
        ),
        HostEvent::InputSessionEnded { peer, reason } => {
            println!("{} ended the input session: {reason}", short(peer));
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::split_args;

    #[test]
    fn quotes_keep_paths_with_spaces_together() {
        assert_eq!(
            split_args(r#"send k3qd "My Photos/a b.jpg" x"#),
            ["send", "k3qd", "My Photos/a b.jpg", "x"]
        );
        assert_eq!(split_args("   ").len(), 0);
        assert_eq!(split_args(r#"clip p """#), ["clip", "p", ""]);
    }
}
