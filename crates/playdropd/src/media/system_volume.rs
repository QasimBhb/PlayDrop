//! The computer's own output volume, offered to peers as the reserved player
//! [`SYSTEM_PLAYER`] (protocol.md 5.9).
//!
//! A player's `volume` action changes that one player, and browsers accept the change and
//! then ignore it. What a person holding the phone wants is the volume the keyboard keys
//! change, so the daemon reports it as one more "player" that is never playing and can do
//! exactly one thing, `volume`. Nothing in the frame format changes, so a peer that knows
//! nothing about the reserved ID still shows a working slider.
//!
//! Backend: the `PulseAudio` native protocol on the user's own socket
//! (`$XDG_RUNTIME_DIR/pulse/native`), which `PipeWire` (`pipewire-pulse`) and `PulseAudio` both
//! serve. The `pulseaudio` crate is a pure Rust implementation of that protocol, so there
//! is no C library to link or package. The volume is the default sink's: the loudest
//! channel as a percentage of "100%" (what desktop sliders show), `0` while the sink is
//! muted. Setting a level scales every channel by the same factor, which keeps the
//! left/right balance, and unmutes when the level is above zero.
//!
//! Two connections, both blocking and on their own threads so no bus or runtime task ever
//! waits on the sound server: one only receives change events, the other asks and sets.
//! When the sound server goes away the entry is reported gone and the worker tries again
//! every [`RETRY_EVERY`].

use std::ffi::CString;
use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::sync::mpsc as std_mpsc;
use std::time::Duration;

use playdrop_core::host::MediaState;
use playdrop_core::session::types::MediaAction;
use pulseaudio::protocol::{self, ChannelVolume, Command, ProtocolError, Volume};
use tokio::sync::mpsc;

/// The reserved player ID. `@` cannot appear in an MPRIS bus name or a Windows app ID, so
/// no real player can have it.
pub const SYSTEM_PLAYER: &str = "@system";
/// The display name sent with the entry. Phone apps that know the ID show their own text.
const NAME: &str = "Computer volume";
/// The sink name the sound server resolves to the current default output.
const DEFAULT_SINK: &str = "@DEFAULT_SINK@";
/// How long to wait before looking for the sound server again.
const RETRY_EVERY: Duration = Duration::from_secs(10);
/// A sound server that stops answering must not hang the worker for ever.
const IO_TIMEOUT: Duration = Duration::from_secs(5);
/// `PulseAudio`'s "100%" (`PA_VOLUME_NORM`).
const NORM: u32 = 0x1_0000;

/// What the worker thread is asked to do.
#[derive(Debug)]
enum Msg {
    /// The sound server said a sink or the default sink changed.
    Changed,
    /// A peer asked for this level, 0 to 100.
    Set(u8),
    /// The event connection ended.
    Lost,
    /// The daemon is stopping.
    Stop,
}

/// Follows the default output's volume and applies peers' changes to it.
#[derive(Debug)]
pub(super) struct SystemVolume {
    tx: std_mpsc::Sender<Msg>,
}

impl SystemVolume {
    /// Starts the worker. Never fails: with no sound server there is simply no entry, and
    /// the worker keeps looking for one.
    pub(super) fn start(out: mpsc::Sender<MediaState>) -> Self {
        let (tx, rx) = std_mpsc::channel();
        let worker_tx = tx.clone();
        let spawned = std::thread::Builder::new()
            .name("system-volume".into())
            .spawn(move || worker(&rx, &worker_tx, &out));
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "the system volume worker could not be started");
        }
        Self { tx }
    }

    /// Sets the default output to `percent` (clamped to 100).
    pub(super) fn set(&self, percent: u64) -> anyhow::Result<()> {
        let level = u8::try_from(percent.min(100)).unwrap_or(100);
        self.tx
            .send(Msg::Set(level))
            .map_err(|_| anyhow::anyhow!("the computer's volume cannot be changed right now"))
    }
}

impl Drop for SystemVolume {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Stop);
    }
}

/// The worker: connect, report, serve messages; on any failure report "gone" and retry.
fn worker(
    rx: &std_mpsc::Receiver<Msg>,
    tx: &std_mpsc::Sender<Msg>,
    out: &mpsc::Sender<MediaState>,
) {
    let mut logged_missing = false;
    loop {
        let mut reported = false;
        let served = serve(rx, tx, out, &mut reported);
        if reported && served.is_err() {
            // Peers were shown a slider that no longer does anything: take it away.
            logged_missing = false;
            let _ = out.blocking_send(gone());
        }
        match served {
            Ok(()) => return,
            Err(e) => {
                if !logged_missing {
                    tracing::info!(error = %e, "the computer's volume is not available; will keep looking");
                    logged_missing = true;
                }
            }
        }
        // Wait before trying again. A level asked for meanwhile has nowhere to go, and a
        // `Lost` left over from the connection that just ended must not end the next one.
        let deadline = std::time::Instant::now() + RETRY_EVERY;
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            match rx.recv_timeout(left) {
                Ok(Msg::Stop) | Err(std_mpsc::RecvTimeoutError::Disconnected) => return,
                Ok(_) => {}
                Err(std_mpsc::RecvTimeoutError::Timeout) => break,
            }
        }
    }
}

/// One life of the two connections. `Ok` means the daemon is stopping. `reported` is set
/// once a level went out to peers.
fn serve(
    rx: &std_mpsc::Receiver<Msg>,
    tx: &std_mpsc::Sender<Msg>,
    out: &mpsc::Sender<MediaState>,
    reported: &mut bool,
) -> anyhow::Result<()> {
    let mut control = Conn::open("PlayDrop")?;
    let events = Conn::open("PlayDrop volume watcher")?;
    // The event thread blocks in a read; closing this clone of its socket ends it.
    let events_socket = events.reader.get_ref().try_clone()?;
    let event_tx = tx.clone();
    let event_thread = std::thread::Builder::new()
        .name("system-volume-events".into())
        .spawn(move || follow_events(events, &event_tx))?;

    let result = serve_messages(&mut control, rx, out, reported);
    let _ = events_socket.shutdown(std::net::Shutdown::Both);
    let _ = event_thread.join();
    result
}

fn serve_messages(
    control: &mut Conn,
    rx: &std_mpsc::Receiver<Msg>,
    out: &mpsc::Sender<MediaState>,
    reported: &mut bool,
) -> anyhow::Result<()> {
    let mut last: Option<u8> = None;
    let mut report = |control: &mut Conn| -> anyhow::Result<()> {
        let level = control.level()?;
        if last != Some(level) {
            last = Some(level);
            tracing::debug!(level, "computer volume");
            if out.blocking_send(state(level)).is_err() {
                anyhow::bail!("the daemon stopped reading media states");
            }
            *reported = true;
        }
        Ok(())
    };
    report(control)?;
    loop {
        let Ok(first) = rx.recv() else {
            return Ok(());
        };
        // Take everything waiting: a drag sends several levels and the sound server
        // answers each with events. Only the newest level is applied, and the state is
        // read once.
        let mut set = None;
        for msg in std::iter::once(first).chain(rx.try_iter()) {
            match msg {
                Msg::Stop => return Ok(()),
                Msg::Lost => anyhow::bail!("the sound server closed the connection"),
                Msg::Set(level) => set = Some(level),
                Msg::Changed => {}
            }
        }
        if let Some(level) = set {
            control.set_level(level)?;
        }
        report(control)?;
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

    /// The default output's level, 0 to 100.
    fn level(&mut self) -> Result<u8, ProtocolError> {
        let sink = self.default_sink()?;
        Ok(level_of(&raw_channels(&sink.cvolume), sink.muted))
    }

    /// Sets the default output to `level`, keeping the balance between channels.
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
            self.tell(&Command::SetSinkMute(protocol::SetDeviceMuteParams {
                device_index: Some(sink.index),
                device_name: None,
                mute: false,
            }))?;
        }
        Ok(())
    }
}

fn default_sink_name() -> CString {
    CString::new(DEFAULT_SINK).unwrap_or_default()
}

fn raw_channels(volume: &ChannelVolume) -> Vec<u32> {
    volume.channels().iter().map(Volume::as_u32).collect()
}

/// The level shown for these channel volumes: the loudest channel as a percentage of
/// 100%, capped at 100 (a sink can be pushed past 100%), and 0 while muted.
fn level_of(channels: &[u32], muted: bool) -> u8 {
    if muted {
        return 0;
    }
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

/// The entry for a level.
fn state(level: u8) -> MediaState {
    MediaState {
        player: SYSTEM_PLAYER.into(),
        name: Some(NAME.into()),
        playing: false,
        title: None,
        artist: None,
        album: None,
        duration_ms: None,
        position_ms: None,
        position_at_ms: None,
        volume: Some(level),
        can: vec![MediaAction::Volume],
    }
}

/// The "gone" marker (not playing, nothing it can do), as for a player that closed.
fn gone() -> MediaState {
    MediaState {
        volume: None,
        can: Vec::new(),
        ..state(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_level_is_the_loudest_channel_and_zero_while_muted() {
        assert_eq!(level_of(&[NORM, NORM], false), 100);
        assert_eq!(level_of(&[NORM / 2, NORM / 4], false), 50);
        assert_eq!(
            level_of(&[NORM * 3 / 2], false),
            100,
            "past 100% shows as 100"
        );
        assert_eq!(level_of(&[NORM, NORM], true), 0);
        assert_eq!(level_of(&[], false), 0);
        assert_eq!(level_of(&[0, 0], false), 0);
    }

    #[test]
    fn every_level_survives_a_round_trip() {
        for level in 0..=100u8 {
            assert_eq!(level_of(&scaled(&[NORM, NORM], level), false), level);
            assert_eq!(level_of(&scaled(&[0, 0], level), false), level);
        }
    }

    #[test]
    fn setting_a_level_keeps_the_balance() {
        // Left at 80%, right at 40%: at level 50 the right stays half of the left.
        let out = scaled(&[NORM * 8 / 10, NORM * 4 / 10], 50);
        assert_eq!(level_of(&out, false), 50);
        assert_eq!(out[1], out[0] / 2);
        assert_eq!(scaled(&[NORM, NORM / 2], 0), vec![0, 0]);
    }

    #[test]
    fn the_entry_is_a_player_that_only_has_a_volume() {
        let s = state(37);
        assert_eq!(s.player, "@system");
        assert!(!s.playing);
        assert_eq!(s.volume, Some(37));
        assert_eq!(s.can, vec![MediaAction::Volume]);
        let g = gone();
        assert!(!g.playing && g.can.is_empty(), "peers drop it");
    }

    /// Reads and moves the real volume: run by hand with
    /// `cargo test -p playdropd -- --ignored system_volume_live --nocapture`.
    #[test]
    #[ignore = "changes this computer's volume for a moment"]
    fn system_volume_live() {
        let mut conn = Conn::open("PlayDrop test").expect("a sound server");
        let before = conn.level().expect("level");
        println!("level before: {before}");
        // One point, so nobody listening notices.
        let target = if before >= 50 { before - 1 } else { before + 1 };
        conn.set_level(target).expect("set");
        assert_eq!(conn.level().expect("level"), target);
        conn.set_level(before).expect("restore");
        assert_eq!(conn.level().expect("level"), before);
    }

    /// The whole worker against the real sound server: the first state arrives, a level
    /// asked for comes back as a new state (through the event connection), and stopping
    /// ends the threads. Run by hand like `system_volume_live`.
    #[test]
    #[ignore = "changes this computer's volume for a moment"]
    fn system_volume_worker_live() {
        let (tx, mut rx) = mpsc::channel(8);
        let volume = SystemVolume::start(tx);
        let first = rx.blocking_recv().expect("first state");
        assert_eq!(first.player, SYSTEM_PLAYER);
        let before = first.volume.expect("a level");
        let target = if before >= 50 { before - 1 } else { before + 1 };
        volume.set(u64::from(target)).expect("set");
        assert_eq!(
            rx.blocking_recv().expect("changed state").volume,
            Some(target)
        );
        volume.set(u64::from(before)).expect("restore");
        assert_eq!(
            rx.blocking_recv().expect("restored state").volume,
            Some(before)
        );
        drop(volume);
        assert!(
            rx.blocking_recv().is_none(),
            "the worker ends and drops its sender"
        );
    }
}
