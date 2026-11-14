//! The computer's own output volume on Linux, offered to peers as the reserved player
//! `@system` (protocol.md 5.9). What is common with Windows (the entry, the messages, the
//! worker loop) is in `system_volume_model.rs`.
//!
//! Backend: the `PulseAudio` native protocol on the user's own socket
//! (`$XDG_RUNTIME_DIR/pulse/native`), which `PipeWire` (`pipewire-pulse`) and `PulseAudio` both
//! serve. The `pulseaudio` crate is a pure Rust implementation of that protocol, so there
//! is no C library to link or package. The volume is the default sink's: the loudest
//! channel as a percentage of "100%" (what desktop sliders show), also while the sink is
//! muted, and the sink's mute flag. Setting a level scales every channel by the same
//! factor, which keeps the left/right balance, and unmutes when the level is above zero.
//!
//! Two connections, both blocking and on their own threads so no bus or runtime task ever
//! waits on the sound server: one only receives change events, the other asks and sets.
//! When the sound server goes away the entry is reported gone and the worker tries again
//! every `RETRY_EVERY`.

use std::ffi::CString;
use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::sync::mpsc as std_mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use playdrop_core::host::MediaState;
use pulseaudio::protocol::{self, ChannelVolume, Command, ProtocolError, Volume};
use tokio::sync::mpsc;

use super::system_volume_model::{self as model, Msg, Output, Reading, SystemVolume};

/// The sink name the sound server resolves to the current default output.
const DEFAULT_SINK: &str = "@DEFAULT_SINK@";
/// A sound server that stops answering must not hang the worker for ever.
const IO_TIMEOUT: Duration = Duration::from_secs(5);
/// `PulseAudio`'s "100%" (`PA_VOLUME_NORM`).
const NORM: u32 = 0x1_0000;

/// Starts following the default output. Never fails: with no sound server there is simply
/// no entry, and the worker keeps looking for one.
pub(super) fn start(out: mpsc::Sender<MediaState>) -> SystemVolume {
    SystemVolume::start(out, worker)
}

fn worker(
    rx: &std_mpsc::Receiver<Msg>,
    tx: &std_mpsc::Sender<Msg>,
    out: &mpsc::Sender<MediaState>,
) {
    model::run(rx, out, || Pulse::open(tx));
}

/// The two connections of one life: the control connection, and the event connection on
/// its own thread. Dropping it ends that thread.
struct Pulse {
    control: Conn,
    /// A clone of the event connection's socket: the event thread blocks in a read, and
    /// closing this ends it.
    events_socket: UnixStream,
    events: Option<JoinHandle<()>>,
}

impl Pulse {
    fn open(tx: &std_mpsc::Sender<Msg>) -> anyhow::Result<Self> {
        let control = Conn::open("PlayDrop")?;
        let events = Conn::open("PlayDrop volume watcher")?;
        let events_socket = events.reader.get_ref().try_clone()?;
        let event_tx = tx.clone();
        let events = std::thread::Builder::new()
            .name("system-volume-events".into())
            .spawn(move || follow_events(events, &event_tx))?;
        Ok(Self {
            control,
            events_socket,
            events: Some(events),
        })
    }
}

impl Drop for Pulse {
    fn drop(&mut self) {
        let _ = self.events_socket.shutdown(std::net::Shutdown::Both);
        if let Some(events) = self.events.take() {
            let _ = events.join();
        }
    }
}

impl Output for Pulse {
    fn read(&mut self) -> anyhow::Result<Reading> {
        Ok(self.control.reading()?)
    }

    fn set_level(&mut self, level: u8) -> anyhow::Result<()> {
        Ok(self.control.set_level(level)?)
    }

    fn set_muted(&mut self, muted: bool) -> anyhow::Result<()> {
        Ok(self.control.set_muted(muted)?)
    }
}

/// Subscribes to sink and default-sink changes and turns each event into [`Msg::Changed`].
fn follow_events(mut conn: Conn, tx: &std_mpsc::Sender<Msg>) {
    let mask = protocol::SubscriptionMask::SINK | protocol::SubscriptionMask::SERVER;
    let subscribed = conn
        .send(&Command::Subscribe(mask))
        .and_then(|()| protocol::read_ack_message(&mut conn.reader).map(|_| ()));
    if let Err(e) = subscribed {
        tracing::debug!(error = %e, "subscribing to sound server events failed");
        let _ = tx.send(Msg::Lost);
        return;
    }
    // Events can be hours apart: this connection reads with no timeout.
    let _ = conn.reader.get_ref().set_read_timeout(None);
    loop {
        match protocol::read_command_message(&mut conn.reader, conn.version) {
            Ok((_, Command::SubscribeEvent(_))) => {
                if tx.send(Msg::Changed).is_err() {
                    return;
                }
            }
            Ok(_) => {}
            Err(e) => {
                tracing::debug!(error = %e, "sound server events ended");
                let _ = tx.send(Msg::Lost);
                return;
            }
        }
    }
}

/// One authenticated connection to the sound server.
struct Conn {
    reader: BufReader<UnixStream>,
    version: u16,
    seq: u32,
}

impl Conn {
    fn open(client_name: &str) -> anyhow::Result<Self> {
        let path = pulseaudio::socket_path_from_env()
            .ok_or_else(|| anyhow::anyhow!("no PulseAudio or PipeWire socket was found"))?;
        let socket = UnixStream::connect(path)?;
        socket.set_read_timeout(Some(IO_TIMEOUT))?;
        socket.set_write_timeout(Some(IO_TIMEOUT))?;
        let mut conn = Self {
            reader: BufReader::new(socket),
            version: protocol::MAX_VERSION,
            seq: 0,
        };
        // PipeWire needs no cookie; PulseAudio reads it from the user's own config.
        let cookie = pulseaudio::cookie_path_from_env()
            .and_then(|p| std::fs::read(p).ok())
            .unwrap_or_default();
        let reply: protocol::AuthReply = conn.ask(&Command::Auth(protocol::AuthParams {
            version: protocol::MAX_VERSION,
            supports_shm: false,
            supports_memfd: false,
            cookie,
        }))?;
        conn.version = protocol::MAX_VERSION.min(reply.version);
        let mut props = protocol::Props::new();
        props.set(
            protocol::Prop::ApplicationName,
            CString::new(client_name).unwrap_or_default(),
        );
        let _: protocol::SetClientNameReply = conn.ask(&Command::SetClientName(props))?;
        Ok(conn)
    }

    fn send(&mut self, command: &Command) -> Result<(), ProtocolError> {
        let seq = self.seq;
        self.seq = self.seq.wrapping_add(1);
        protocol::write_command_message(self.reader.get_mut(), seq, command, self.version)
    }

    fn ask<R: protocol::CommandReply>(&mut self, command: &Command) -> Result<R, ProtocolError> {
        self.send(command)?;
        protocol::read_reply_message(&mut self.reader, self.version).map(|(_, reply)| reply)
    }

    fn tell(&mut self, command: &Command) -> Result<(), ProtocolError> {
        self.send(command)?;
        protocol::read_ack_message(&mut self.reader).map(|_| ())
    }

    fn default_sink(&mut self) -> Result<protocol::SinkInfo, ProtocolError> {
        self.ask(&Command::GetSinkInfo(protocol::GetSinkInfo {
            index: None,
            name: Some(default_sink_name()),
        }))
    }

    /// The default output's level (0 to 100) and mute flag.
    fn reading(&mut self) -> Result<Reading, ProtocolError> {
        let sink = self.default_sink()?;
        Ok(Reading {
            level: level_of(&raw_channels(&sink.cvolume)),
            muted: sink.muted,
        })
    }

    /// Sets the default output to `level`, keeping the balance between channels, and
    /// unmutes it when `level` is above zero.
    fn set_level(&mut self, level: u8) -> Result<(), ProtocolError> {
        let sink = self.default_sink()?;
        let mut volume = ChannelVolume::empty();
        for raw in scaled(&raw_channels(&sink.cvolume), level) {
            volume.push(Volume::from_u32_clamped(raw));
        }
        self.tell(&Command::SetSinkVolume(protocol::SetDeviceVolumeParams {
            device_index: Some(sink.index),
            device_name: None,
            volume,
        }))?;
        if sink.muted && level > 0 {
            self.set_sink_mute(sink.index, false)?;
        }
        Ok(())
    }

    /// Mutes or unmutes the default output.
    fn set_muted(&mut self, muted: bool) -> Result<(), ProtocolError> {
        let sink = self.default_sink()?;
        if sink.muted != muted {
            self.set_sink_mute(sink.index, muted)?;
        }
        Ok(())
    }

    fn set_sink_mute(&mut self, index: u32, mute: bool) -> Result<(), ProtocolError> {
        self.tell(&Command::SetSinkMute(protocol::SetDeviceMuteParams {
            device_index: Some(index),
            device_name: None,
            mute,
        }))
    }
}

fn default_sink_name() -> CString {
    CString::new(DEFAULT_SINK).unwrap_or_default()
}

fn raw_channels(volume: &ChannelVolume) -> Vec<u32> {
    volume.channels().iter().map(Volume::as_u32).collect()
}

/// The level shown for these channel volumes: the loudest channel as a percentage of
/// 100%, capped at 100 (a sink can be pushed past 100%). The mute flag is separate.
fn level_of(channels: &[u32]) -> u8 {
    let loudest = u64::from(channels.iter().copied().max().unwrap_or(0));
    let percent = (loudest * 100 + u64::from(NORM) / 2) / u64::from(NORM);
    u8::try_from(percent.min(100)).unwrap_or(100)
}

/// `channels` moved so the loudest sits at `level` percent, every channel by the same
/// factor. All-silent channels have no balance to keep and all go to `level`.
fn scaled(channels: &[u32], level: u8) -> Vec<u32> {
    let target = u64::from(NORM) * u64::from(level.min(100)) / 100;
    let loudest = u64::from(channels.iter().copied().max().unwrap_or(0));
    channels
        .iter()
        .map(|&c| {
            let raw = (u64::from(c) * target)
                .checked_div(loudest)
                .unwrap_or(target);
            u32::try_from(raw).unwrap_or(NORM)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::system_volume_model::SYSTEM_PLAYER;
    use playdrop_core::session::types::MediaAction;

    #[test]
    fn the_level_is_the_loudest_channel() {
        assert_eq!(level_of(&[NORM, NORM]), 100);
        assert_eq!(level_of(&[NORM / 2, NORM / 4]), 50);
        assert_eq!(level_of(&[NORM * 3 / 2]), 100, "past 100% shows as 100");
        assert_eq!(level_of(&[]), 0);
        assert_eq!(level_of(&[0, 0]), 0);
    }

    #[test]
    fn every_level_survives_a_round_trip() {
        for level in 0..=100u8 {
            assert_eq!(level_of(&scaled(&[NORM, NORM], level)), level);
            assert_eq!(level_of(&scaled(&[0, 0], level)), level);
        }
    }

    #[test]
    fn setting_a_level_keeps_the_balance() {
        // Left at 80%, right at 40%: at level 50 the right stays half of the left.
        let out = scaled(&[NORM * 8 / 10, NORM * 4 / 10], 50);
        assert_eq!(level_of(&out), 50);
        assert_eq!(out[1], out[0] / 2);
        assert_eq!(scaled(&[NORM, NORM / 2], 0), vec![0, 0]);
    }

    /// Puts the level and mute flag found at the start back, also when an assertion
    /// failed half way.
    struct Restore {
        conn: Conn,
        before: Reading,
    }

    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = self.conn.set_level(self.before.level);
            let _ = self.conn.set_muted(self.before.muted);
        }
    }

    /// One point away from `level`, so nobody listening notices.
    fn nearby(level: u8) -> u8 {
        if level >= 50 {
            level - 1
        } else {
            level + 1
        }
    }

    /// Reads and moves the real volume and mute: run by hand with
    /// `cargo test -p playdropd -- --ignored system_volume_live --nocapture`.
    #[test]
    #[ignore = "changes this computer's volume for a moment"]
    fn system_volume_live() {
        let mut conn = Conn::open("PlayDrop test").expect("a sound server");
        let before = conn.reading().expect("reading");
        println!("before: {before:?}");
        let mut guard = Restore { conn, before };
        let conn = &mut guard.conn;

        let target = nearby(before.level);
        conn.set_level(target).expect("set");
        let r = conn.reading().expect("reading");
        assert_eq!(r.level, target);
        assert!(!r.muted, "a level above zero unmutes");

        conn.set_muted(true).expect("mute");
        let r = conn.reading().expect("reading");
        assert_eq!(
            (r.level, r.muted),
            (target, true),
            "the level stays while muted"
        );
        conn.set_muted(false).expect("unmute");
        assert!(!conn.reading().expect("reading").muted);

        conn.set_level(before.level).expect("restore level");
        conn.set_muted(before.muted).expect("restore mute");
        assert_eq!(conn.reading().expect("reading"), before);
    }

    /// Receives states until one matches, so notices for in-between steps do not matter.
    fn wait_for(rx: &mut mpsc::Receiver<MediaState>, want: Reading) -> MediaState {
        for _ in 0..20 {
            let s = rx.blocking_recv().expect("a state");
            if s.volume == Some(want.level) && s.muted == Some(want.muted) {
                return s;
            }
        }
        panic!("no state with {want:?}");
    }

    /// The whole worker against the real sound server: the first state arrives, a level
    /// and a mute asked for come back as new states (through the event connection), and
    /// stopping ends the threads. Run by hand like `system_volume_live`.
    #[test]
    #[ignore = "changes this computer's volume for a moment"]
    fn system_volume_worker_live() {
        let mut conn = Conn::open("PlayDrop test").expect("a sound server");
        let before = conn.reading().expect("reading");
        let _guard = Restore { conn, before };

        let (tx, mut rx) = mpsc::channel(8);
        let volume = start(tx);
        let first = rx.blocking_recv().expect("first state");
        assert_eq!(first.player, SYSTEM_PLAYER);
        assert_eq!(first.can, vec![MediaAction::Volume, MediaAction::Mute]);
        assert_eq!(
            (first.volume, first.muted),
            (Some(before.level), Some(before.muted))
        );

        let target = nearby(before.level);
        volume
            .command(&MediaAction::Volume, Some(u64::from(target)))
            .expect("set");
        wait_for(
            &mut rx,
            Reading {
                level: target,
                muted: false,
            },
        );
        volume.command(&MediaAction::Mute, Some(1)).expect("mute");
        wait_for(
            &mut rx,
            Reading {
                level: target,
                muted: true,
            },
        );
        volume
            .command(&MediaAction::Volume, Some(u64::from(before.level)))
            .expect("restore level");
        volume
            .command(&MediaAction::Mute, Some(u64::from(before.muted)))
            .expect("restore mute");
        wait_for(&mut rx, before);

        drop(volume);
        while rx.blocking_recv().is_some() {}
    }
}
