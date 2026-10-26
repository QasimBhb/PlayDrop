//! The short daemon commands: one or two requests, one answer printed.

use std::io::Read as _;

use playdrop_core::host::TransferSnapshot;
use playdrop_ipc::{ClipSkip, Ok as Answer, Request};

use super::{
    absolute_paths, ask, connect, output, peers as list_peers, pick_peer, unexpected, CancelArgs,
    ClipArgs, Exit, Failure, JsonArgs, OutboxAction, OutboxAddArgs, OutboxArgs, PingArgs,
};

/// Most clipboard text the daemon takes (`docs/ipc.md` section 1.1).
const CLIP_MAX_BYTES: usize = 1024 * 1024;

/// `playdrop peers`.
pub async fn peers(args: JsonArgs) -> Result<(), Failure> {
    let client = connect().await?;
    let answer = ask(&client, Request::Peers).await?;
    let Answer::Peers(list) = &answer else {
        return Err(unexpected("peers", &answer));
    };
    if args.json {
        println!("{}", output::json(&answer));
    } else {
        print!("{}", output::peers(list, output::unix_now()));
    }
    Ok(())
}

/// `playdrop status`.
pub async fn status(args: JsonArgs) -> Result<(), Failure> {
    let client = connect().await?;
    let answer = ask(&client, Request::Status).await?;
    let Answer::Status(s) = &answer else {
        return Err(unexpected("status", &answer));
    };
    if args.json {
        println!("{}", output::json(&answer));
    } else {
        print!("{}", output::status(s));
    }
    Ok(())
}

/// `playdrop transfers`.
pub async fn transfers(args: JsonArgs) -> Result<(), Failure> {
    let client = connect().await?;
    let answer = ask(&client, Request::Transfers).await?;
    let Answer::Transfers(list) = &answer else {
        return Err(unexpected("transfers", &answer));
    };
    if args.json {
        println!("{}", output::json(&answer));
    } else {
        let peers = list_peers(&client).await?;
        print!("{}", output::transfers(list, &peers, output::unix_now()));
    }
    Ok(())
}

/// `playdrop cancel`. The ID is matched here, against the daemon's own list, so a short
/// start of it works like a short device ID does and a wrong one exits 2 with the list.
pub async fn cancel(args: CancelArgs) -> Result<(), Failure> {
    let client = connect().await?;
    let answer = ask(&client, Request::Transfers).await?;
    let Answer::Transfers(list) = &answer else {
        return Err(unexpected("transfers", &answer));
    };
    let id = resolve_transfer(list, &args.id)?.id.clone();
    ask(&client, Request::CancelTransfer { id: id.clone() }).await?;
    println!("cancelled {id}");
    Ok(())
}

/// The one transfer whose ID is `want` or starts with it. An exact match wins over a longer
/// ID that merely starts with the same text.
fn resolve_transfer<'a>(
    list: &'a [TransferSnapshot],
    want: &str,
) -> Result<&'a TransferSnapshot, Failure> {
    let want = want.trim();
    if let Some(t) = list.iter().find(|t| t.id == want) {
        return Ok(t);
    }
    let hits: Vec<&TransferSnapshot> = if want.is_empty() {
        Vec::new()
    } else {
        list.iter().filter(|t| t.id.starts_with(want)).collect()
    };
    match hits.as_slice() {
        [one] => Ok(one),
        [] => Err(Failure::new(
            Exit::Usage,
            format!("no transfer matches \"{want}\"; `playdrop transfers` lists them"),
        )),
        many => Err(Failure::new(
            Exit::Usage,
            format!(
                "\"{want}\" matches {} transfers; give more of the ID:\n{}",
                many.len(),
                many.iter()
                    .map(|t| format!("  {}", t.id))
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
        )),
    }
}

/// `playdrop diagnose`.
pub async fn diagnose(args: JsonArgs) -> Result<(), Failure> {
    let client = connect().await?;
    let answer = ask(&client, Request::Diagnose).await?;
    let Answer::Diagnostics(report) = &answer else {
        return Err(unexpected("diagnose", &answer));
    };
    if args.json {
        println!("{}", output::json(&answer));
    } else {
        print!("{}", output::diagnose(report, output::unix_now()));
    }
    Ok(())
}

/// `playdrop clip`.
pub async fn clip(args: ClipArgs) -> Result<(), Failure> {
    let text = if args.stdin {
        read_clip(std::io::stdin().lock())?
    } else {
        args.text.unwrap_or_default()
    };
    check_clip(&text)?;
    let client = connect().await?;
    let peers = list_peers(&client).await?;
    let peer = pick_peer(&peers, args.to.as_deref())?;
    let answer = ask(
        &client,
        Request::SendClipboard {
            peer: peer.device_id.clone(),
            text,
            sensitive: args.sensitive,
        },
    )
    .await?;
    match answer {
        Answer::ClipboardPushed { sent_to, .. } if !sent_to.is_empty() => {
            println!("clipboard sent to {}", peer.label);
        }
        Answer::ClipboardPushed { skipped, .. } => match clip_reason(skipped) {
            ClipSkip::Echo => println!("{}", output::clip_not_sent(&peer.label, ClipSkip::Echo)),
            other => {
                return Err(Failure::new(
                    Exit::Failed,
                    output::clip_not_sent(&peer.label, other),
                ))
            }
        },
        other => return Err(unexpected("send_clipboard", &other)),
    }
    Ok(())
}

/// Why a clip was not sent, from the daemon's answer. An older daemon sends only
/// `skipped_echo`, and for `send_clipboard` (what `clip` uses) an echo was its only "nothing
/// sent" case, so a missing reason reads as an echo skip.
fn clip_reason(skipped: Option<ClipSkip>) -> ClipSkip {
    skipped.unwrap_or(ClipSkip::Echo)
}

/// Reads clipboard text from `reader`, at most one byte over the limit so an endless pipe
/// is refused instead of buffered. The size is checked before the UTF-8 decode, so a
/// multi-byte character cut at the limit still reads as "too large".
fn read_clip(reader: impl std::io::Read) -> Result<String, Failure> {
    let mut bytes = Vec::new();
    reader
        .take(CLIP_MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| Failure::new(Exit::Usage, format!("reading stdin: {e}")))?;
    if bytes.len() > CLIP_MAX_BYTES {
        return Err(too_large());
    }
    String::from_utf8(bytes)
        .map_err(|_| Failure::new(Exit::Usage, "stdin is not UTF-8 text; send it as a file"))
}

/// Refuses empty text and text over the daemon's limit.
fn check_clip(text: &str) -> Result<(), Failure> {
    if text.is_empty() {
        return Err(Failure::new(
            Exit::Usage,
            "nothing to send: the text is empty",
        ));
    }
    if text.len() > CLIP_MAX_BYTES {
        return Err(too_large());
    }
    Ok(())
}

fn too_large() -> Failure {
    Failure::new(
        Exit::Usage,
        "clipboard text is over 1 MiB, the most PlayDrop sends; send it as a file",
    )
}

/// `playdrop ping`.
pub async fn ping(args: PingArgs) -> Result<(), Failure> {
    let client = connect().await?;
    let peers = list_peers(&client).await?;
    let peer = pick_peer(&peers, args.to.as_deref())?;
    ask(
        &client,
        Request::Ping {
            peer: peer.device_id.clone(),
            message: args.message,
        },
    )
    .await?;
    println!("pinged {}", peer.label);
    Ok(())
}

/// `playdrop outbox` and `playdrop outbox add`.
pub async fn outbox(args: OutboxArgs) -> Result<(), Failure> {
    if let Some(OutboxAction::Add(add)) = args.action {
        return outbox_add(add).await;
    }
    let client = connect().await?;
    let answer = ask(&client, Request::Outbox).await?;
    let Answer::Outbox(items) = &answer else {
        return Err(unexpected("outbox", &answer));
    };
    if args.json {
        println!("{}", output::json(&answer));
    } else {
        let peers = list_peers(&client).await?;
        print!("{}", output::outbox(items, &peers, output::unix_now()));
    }
    Ok(())
}

async fn outbox_add(args: OutboxAddArgs) -> Result<(), Failure> {
    let paths = absolute_paths(&args.paths)?;
    let client = connect().await?;
    let peers = list_peers(&client).await?;
    let peer = pick_peer(&peers, Some(&args.to))?;
    let answer = client
        .queue_outbox(&peer.device_id, paths)
        .await
        .map_err(Failure::from)?;
    let Answer::OutboxQueued { id } = &answer else {
        return Err(unexpected("queue_outbox", &answer));
    };
    if args.json {
        println!("{}", output::json(&answer));
    } else {
        println!(
            "queued {id} for {}; it pulls the files when it next opens PlayDrop",
            peer.label
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transfer(id: &str) -> TransferSnapshot {
        serde_json::from_value(serde_json::json!({
            "id": id, "peer": "00", "short_id": "aaaa-bbbb", "direction": "send",
            "state": "failed", "bytes": 0, "total": 0, "bytes_per_sec": 0,
            "started_at": 0, "file_count": 1, "resumable": true
        }))
        .expect("snapshot")
    }

    #[test]
    fn a_transfer_id_matches_exactly_or_by_a_unique_start() {
        let list = [transfer("7f3a91"), transfer("7f3b02"), transfer("7f3a")];
        assert_eq!(resolve_transfer(&list, "7f3a").expect("exact").id, "7f3a");
        assert_eq!(
            resolve_transfer(&list, "7f3b").expect("prefix").id,
            "7f3b02"
        );
        let many = resolve_transfer(&list, "7f3").expect_err("ambiguous");
        assert_eq!(many.exit, Exit::Usage);
        assert!(many.message.contains("matches 3 transfers"));
        let none = resolve_transfer(&list, "zz").expect_err("unknown");
        assert_eq!(none.exit, Exit::Usage);
        assert_eq!(
            resolve_transfer(&list, "").expect_err("empty").exit,
            Exit::Usage
        );
    }

    #[test]
    fn a_missing_reason_from_an_older_daemon_reads_as_echo() {
        assert_eq!(clip_reason(None), ClipSkip::Echo);
        assert_eq!(clip_reason(Some(ClipSkip::NoPeers)), ClipSkip::NoPeers);
        assert_eq!(
            clip_reason(Some(ClipSkip::Unsupported)),
            ClipSkip::Unsupported
        );
    }

    #[test]
    fn clip_from_a_reader_is_bounded() {
        assert_eq!(read_clip(&b"hello\n"[..]).expect("small"), "hello\n");
        let exact = vec![b'a'; CLIP_MAX_BYTES];
        assert_eq!(
            read_clip(&exact[..]).expect("at the limit").len(),
            CLIP_MAX_BYTES
        );
        // An endless reader stops one byte past the limit.
        let f = read_clip(std::io::repeat(b'a')).expect_err("endless");
        assert_eq!(f.exit, Exit::Usage);
        assert!(f.message.contains("over 1 MiB"));
        // A multi-byte character cut by the limit still reads as too large.
        let mut cut = vec![b'a'; CLIP_MAX_BYTES - 1];
        cut.extend("\u{e9}\u{e9}".as_bytes());
        assert!(read_clip(&cut[..])
            .expect_err("cut")
            .message
            .contains("over 1 MiB"));
        assert!(read_clip(&[0xff, 0xfe][..])
            .expect_err("binary")
            .message
            .contains("UTF-8"));
    }

    #[test]
    fn clip_text_must_be_non_empty_and_small() {
        assert!(check_clip("hi").is_ok());
        assert_eq!(check_clip("").expect_err("empty").exit, Exit::Usage);
        let big = "a".repeat(CLIP_MAX_BYTES + 1);
        assert_eq!(check_clip(&big).expect_err("big").exit, Exit::Usage);
    }
}
