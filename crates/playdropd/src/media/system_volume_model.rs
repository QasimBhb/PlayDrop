//! The computer's own output volume as the reserved player [`SYSTEM_PLAYER`] (protocol.md
//! 5.9), the part every platform shares.
//!
//! A player's `volume` action changes that one player, and browsers accept the change and
//! then ignore it. What a person holding the phone wants is the volume the keyboard keys
//! change, so the daemon reports it as one more "player" that is never playing and can do
//! two things, `volume` and `mute`. Nothing in the frame format changes, so a peer that
//! knows nothing about the reserved ID still shows a working slider.
//!
//! Each backend (`system_volume.rs` on Linux, `system_volume_windows.rs` on Windows) runs
//! one worker thread and only says how to open the default output and how to read and
//! change it ([`Output`]). This file owns the rest: the handle the daemon holds, the
//! messages the worker is sent, folding a burst of them into one change (a drag sends many
//! levels and only the newest one matters), the entry peers see, and the loop that reports
//! the entry "gone" when the output goes away and keeps looking for it every
//! [`RETRY_EVERY`]. All of it is plain logic, so it is unit-tested on every platform.

use std::sync::mpsc as std_mpsc;
use std::time::{Duration, Instant};

use playdrop_core::host::MediaState;
use playdrop_core::session::types::MediaAction;
use tokio::sync::mpsc;

/// The reserved player ID. `@` cannot appear in an MPRIS bus name or a Windows app ID, so
/// no real player can have it.
pub const SYSTEM_PLAYER: &str = "@system";
/// The display name sent with the entry. Phone apps that know the ID show their own text.
const NAME: &str = "Computer volume";
/// How long to wait before looking for the output again.
pub(super) const RETRY_EVERY: Duration = Duration::from_secs(10);

/// What the default output is set to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Reading {
    /// The level, 0 to 100. Kept while muted: unmuting goes back to it.
    pub level: u8,
    /// Whether the output is muted.
    pub muted: bool,
}

/// What the worker thread is asked to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Msg {
    /// The level or the mute flag of the output changed.
    Changed,
    /// The default output may be another device now: open it again.
    #[cfg_attr(not(windows), allow(dead_code))]
    DeviceChanged,
    /// A peer asked for this level, 0 to 100.
    Set(u8),
    /// A peer asked to mute (`true`) or unmute (`false`).
    Mute(bool),
    /// The connection that brings change notices ended.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Lost,
    /// Send the current state again even if it did not change (a new host knows nothing).
    Republish,
    /// The daemon is stopping.
    Stop,
}

/// One open default output, as a backend sees it.
pub(super) trait Output {
    /// The current level and mute flag.
    fn read(&mut self) -> anyhow::Result<Reading>;
    /// Sets the level, and unmutes when `level` is above zero.
    fn set_level(&mut self, level: u8) -> anyhow::Result<()>;
    /// Mutes or unmutes, leaving the level as it is.
    fn set_muted(&mut self, muted: bool) -> anyhow::Result<()>;
}

/// The worker a backend runs on its own thread: the messages it reads, a sender for its
/// own notice threads or callbacks, and the channel to the daemon.
pub(super) type Worker =
    fn(&std_mpsc::Receiver<Msg>, &std_mpsc::Sender<Msg>, &mpsc::Sender<MediaState>);

/// Follows the default output's volume and applies peers' changes to it.
#[derive(Debug)]
pub(super) struct SystemVolume {
    tx: std_mpsc::Sender<Msg>,
}

impl SystemVolume {
    /// Starts `worker` on its own thread. Never fails: with no output there is simply no
    /// entry, and the worker keeps looking for one.
    pub(super) fn start(out: mpsc::Sender<MediaState>, worker: Worker) -> Self {
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

    /// Applies a peer's command for [`SYSTEM_PLAYER`]: `volume` with a level (clamped to
    /// 100), or `mute` with `1` or `0`. A `mute` with any other value is ignored, as the
    /// protocol says.
    pub(super) fn command(&self, action: &MediaAction, value: Option<u64>) -> anyhow::Result<()> {
        match action {
            MediaAction::Volume => {
                let level = value.ok_or_else(|| {
                    anyhow::anyhow!("a volume change needs a level from 0 to 100")
                })?;
                self.send(Msg::Set(u8::try_from(level.min(100)).unwrap_or(100)))
            }
            MediaAction::Mute => match value {
                Some(1) => self.send(Msg::Mute(true)),
                Some(0) => self.send(Msg::Mute(false)),
                other => {
                    tracing::debug!(value = ?other, "mute command without 1 or 0 ignored");
                    Ok(())
                }
            },
            _ => anyhow::bail!("the computer's volume can only be set to a level or muted"),
        }
    }

    /// Sends the current state again, also when nothing changed. Nothing is sent while no
    /// output is open.
    pub(super) fn republish(&self) {
        let _ = self.tx.send(Msg::Republish);
    }

    fn send(&self, msg: Msg) -> anyhow::Result<()> {
        self.tx
            .send(msg)
            .map_err(|_| anyhow::anyhow!("the computer's volume cannot be changed right now"))
    }
}

impl Drop for SystemVolume {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Stop);
    }
}

/// A level and a mute flag a peer asked for that are not applied yet.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Change {
    /// The newest level asked for.
    level: Option<u8>,
    /// The mute flag the burst ends with. A level above zero unmutes, so it records
    /// `Some(false)`; a later `Mute` in the same burst still wins.
    muted: Option<bool>,
}

impl Change {
    fn set(&mut self, level: u8) {
        self.level = Some(level);
        if level > 0 {
            self.muted = Some(false);
        }
    }

    /// Sets the level, then the mute flag, on `output`: the same result as applying the
    /// burst's messages one by one.
    fn apply<O: Output>(self, output: &mut O) -> anyhow::Result<()> {
        if let Some(level) = self.level {
            output.set_level(level)?;
        }
        match self.muted {
            // `set_level` above zero already unmuted.
            Some(false) if self.level.is_some_and(|l| l > 0) => {}
            Some(muted) => output.set_muted(muted)?,
            None => {}
        }
        Ok(())
    }
}

/// Everything a burst of messages asks for, folded into one change.
#[derive(Debug, Default, PartialEq, Eq)]
struct Batch {
    /// What ends this life of the output, if anything does.
    end: Option<BatchEnd>,
    republish: bool,
    change: Change,
}

/// Why a burst ends the life of the open output, weakest first: when a burst holds
/// several, the strongest wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum BatchEnd {
    /// The default output changed: open it again.
    Reopen,
    /// The notices stopped: the output is lost.
    Lost,
    /// The daemon is stopping.
    Stop,
}

impl Batch {
    fn fold(msgs: impl IntoIterator<Item = Msg>) -> Self {
        let mut batch = Self::default();
        for msg in msgs {
            match msg {
                Msg::Stop => batch.end_with(BatchEnd::Stop),
                Msg::Lost => batch.end_with(BatchEnd::Lost),
                Msg::DeviceChanged => batch.end_with(BatchEnd::Reopen),
                Msg::Changed => {}
                Msg::Republish => batch.republish = true,
                Msg::Set(level) => batch.change.set(level),
                Msg::Mute(muted) => batch.change.muted = Some(muted),
            }
        }
        batch
    }

    fn end_with(&mut self, end: BatchEnd) {
        self.end = self.end.max(Some(end));
    }
}

/// How one life of an open output ended without an error.
#[derive(Debug, PartialEq, Eq)]
enum Ended {
    /// The daemon is stopping, or no longer reads states.
    Stop,
    /// The default output changed: open it again straight away, and apply this change,
    /// asked for in the same burst, to the new output.
    Reopen(Change),
}

/// The worker loop: open the default output, report it, serve messages; when it fails,
/// report "gone" if peers were shown a slider, wait, and try again. Returns when the
/// daemon stops. A change asked for while the default output is switched is applied to the
/// new one; one asked for while no output can be opened has nowhere to go and is dropped.
pub(super) fn run<O: Output>(
    rx: &std_mpsc::Receiver<Msg>,
    out: &mpsc::Sender<MediaState>,
    mut open: impl FnMut() -> anyhow::Result<O>,
) {
    let mut shown: Option<Reading> = None;
    let mut logged_missing = false;
    let mut pending = Change::default();
    loop {
        let carried = std::mem::take(&mut pending);
        let ended = open().and_then(|mut output| serve(&mut output, rx, out, &mut shown, carried));
        match ended {
            Ok(Ended::Stop) => return,
            Ok(Ended::Reopen(change)) => pending = change,
            Err(e) => {
                if shown.take().is_some() {
                    // Peers were shown a slider that no longer does anything: take it away.
                    logged_missing = false;
                    if out.blocking_send(gone()).is_err() {
                        return;
                    }
                }
                if logged_missing {
                    tracing::debug!(error = %e, "the computer's volume is still not available");
                } else {
                    tracing::info!(error = %e, "the computer's volume is not available; will keep looking");
                    logged_missing = true;
                }
                if !wait_to_retry(rx, RETRY_EVERY) {
                    return;
                }
            }
        }
    }
}

/// Waits `delay`, or less when the default output changes. `false` when the daemon is
/// stopping. A `Lost` left over from the life that just ended must not end the next one,
/// so everything else is dropped.
fn wait_to_retry(rx: &std_mpsc::Receiver<Msg>, delay: Duration) -> bool {
    let deadline = Instant::now() + delay;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(Msg::Stop) | Err(std_mpsc::RecvTimeoutError::Disconnected) => return false,
            Ok(Msg::DeviceChanged) | Err(std_mpsc::RecvTimeoutError::Timeout) => return true,
            Ok(_) => {}
        }
    }
}

/// One life of an open output. `carried` is a change left over from the output before.
fn serve<O: Output>(
    output: &mut O,
    rx: &std_mpsc::Receiver<Msg>,
    out: &mpsc::Sender<MediaState>,
    shown: &mut Option<Reading>,
    carried: Change,
) -> anyhow::Result<Ended> {
    apply(carried, output);
    if !publish(output.read()?, out, shown) {
        return Ok(Ended::Stop);
    }
    loop {
        let Ok(first) = rx.recv() else {
            return Ok(Ended::Stop);
        };
        // Take everything waiting: a drag sends several levels and the output answers
        // each with a notice. Only the newest level is applied, and the state is read once.
        let batch = Batch::fold(std::iter::once(first).chain(rx.try_iter()));
        match batch.end {
            Some(BatchEnd::Stop) => return Ok(Ended::Stop),
            Some(BatchEnd::Lost) => anyhow::bail!("the sound system stopped sending changes"),
            // The output this life opened may not be the default any more: the change
            // goes to the one opened next.
            Some(BatchEnd::Reopen) => return Ok(Ended::Reopen(batch.change)),
            None => {}
        }
        apply(batch.change, output);
        if batch.republish {
            *shown = None;
        }
        if !publish(output.read()?, out, shown) {
            return Ok(Ended::Stop);
        }
    }
}

/// Applies a change. A change that fails (a drag while the default output is switched,
/// say) does not mean the output is gone: the read after it decides that.
fn apply<O: Output>(change: Change, output: &mut O) {
    if let Err(e) = change.apply(output) {
        tracing::debug!(error = %e, "changing the computer's volume failed");
    }
}

/// Sends the entry when the level or the mute flag changed since the last one shown.
/// `false` when the daemon no longer reads states.
fn publish(reading: Reading, out: &mpsc::Sender<MediaState>, shown: &mut Option<Reading>) -> bool {
    if *shown == Some(reading) {
        return true;
    }
    *shown = Some(reading);
    tracing::debug!(
        level = reading.level,
        muted = reading.muted,
        "computer volume"
    );
    out.blocking_send(state(reading)).is_ok()
}

/// The entry for a reading.
pub(super) fn state(reading: Reading) -> MediaState {
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
        volume: Some(reading.level),
        muted: Some(reading.muted),
        can: vec![MediaAction::Volume, MediaAction::Mute],
    }
}

/// The "gone" marker (not playing, nothing it can do), as for a player that closed.
pub(super) fn gone() -> MediaState {
    MediaState {
        volume: None,
        muted: None,
        can: Vec::new(),
        ..state(Reading {
            level: 0,
            muted: false,
        })
    }
}

/// The level shown for a Windows endpoint scalar (0.0 to 1.0): what the Windows volume
/// slider shows, the scalar times 100, rounded.
#[cfg(any(windows, test))]
pub(super) fn percent_from_scalar(scalar: f32) -> u8 {
    if scalar.is_nan() {
        return 0;
    }
    let percent = (scalar.clamp(0.0, 1.0) * 100.0).round();
    // Clamped to 0.0..=100.0 and rounded above, so it fits a u8 exactly.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let percent = percent as u8;
    percent
}

/// The Windows endpoint scalar for a level, 0 to 100 (clamped).
#[cfg(any(windows, test))]
pub(super) fn scalar_from_percent(level: u8) -> f32 {
    f32::from(level.min(100)) / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    #[test]
    fn the_entry_is_a_player_with_a_volume_and_a_mute() {
        let s = state(Reading {
            level: 37,
            muted: true,
        });
        assert_eq!(s.player, "@system");
        assert!(!s.playing);
        assert_eq!(s.volume, Some(37), "the level stays while muted");
        assert_eq!(s.muted, Some(true));
        assert_eq!(s.can, vec![MediaAction::Volume, MediaAction::Mute]);
        let g = gone();
        assert!(!g.playing && g.can.is_empty(), "peers drop it");
        assert_eq!((g.volume, g.muted), (None, None));
    }

    #[test]
    fn a_burst_folds_to_the_newest_level_and_mute() {
        use Msg::{Changed, Mute, Set};
        assert_eq!(Batch::fold([Changed, Changed]), Batch::default());
        let b = Batch::fold([Set(10), Changed, Set(20), Set(30)]);
        assert_eq!((b.change.level, b.change.muted), (Some(30), Some(false)));
        // Mute after a level: the level is set, then the output muted.
        let b = Batch::fold([Set(40), Mute(true)]);
        assert_eq!((b.change.level, b.change.muted), (Some(40), Some(true)));
        // A level above zero after a mute unmutes.
        let b = Batch::fold([Mute(true), Set(40)]);
        assert_eq!((b.change.level, b.change.muted), (Some(40), Some(false)));
        // ... and the unmute it implies stays when a later level is zero.
        let b = Batch::fold([Set(40), Set(0)]);
        assert_eq!((b.change.level, b.change.muted), (Some(0), Some(false)));
        let b = Batch::fold([Mute(true), Set(40), Set(0)]);
        assert_eq!((b.change.level, b.change.muted), (Some(0), Some(false)));
        // Level zero does not unmute.
        let b = Batch::fold([Mute(true), Set(0)]);
        assert_eq!((b.change.level, b.change.muted), (Some(0), Some(true)));
        let b = Batch::fold([Mute(true), Mute(false)]);
        assert_eq!(b.change.muted, Some(false));
        let b = Batch::fold([Set(5), Msg::Stop, Msg::Lost, Msg::DeviceChanged]);
        assert_eq!(b.end, Some(BatchEnd::Stop), "stopping beats everything");
        let b = Batch::fold([Msg::DeviceChanged, Msg::Lost, Msg::DeviceChanged]);
        assert_eq!(b.end, Some(BatchEnd::Lost), "a loss beats a reopen");
        assert!(Batch::fold([Msg::Republish]).end.is_none());
        assert!(Batch::fold([Msg::Republish]).republish);
    }

    #[test]
    fn scalars_and_levels_round_trip() {
        for level in 0..=100u8 {
            assert_eq!(percent_from_scalar(scalar_from_percent(level)), level);
        }
        assert_eq!(scalar_from_percent(150), 1.0);
        assert_eq!(percent_from_scalar(0.404), 40);
        assert_eq!(percent_from_scalar(0.405), 41);
        assert_eq!(percent_from_scalar(-0.5), 0);
        assert_eq!(percent_from_scalar(1.7), 100);
        assert_eq!(percent_from_scalar(f32::NAN), 0);
    }

    fn handle() -> (SystemVolume, std_mpsc::Receiver<Msg>) {
        let (tx, rx) = std_mpsc::channel();
        (SystemVolume { tx }, rx)
    }

    #[test]
    fn commands_become_messages() {
        let (volume, rx) = handle();
        volume.command(&MediaAction::Volume, Some(42)).unwrap();
        volume.command(&MediaAction::Volume, Some(400)).unwrap();
        volume.command(&MediaAction::Mute, Some(1)).unwrap();
        volume.command(&MediaAction::Mute, Some(0)).unwrap();
        // Ignored, as protocol.md 5.9 says.
        volume.command(&MediaAction::Mute, None).unwrap();
        volume.command(&MediaAction::Mute, Some(2)).unwrap();
        assert!(volume.command(&MediaAction::Volume, None).is_err());
        assert!(volume.command(&MediaAction::Play, None).is_err());
        let got: Vec<Msg> = rx.try_iter().collect();
        assert_eq!(
            got,
            vec![
                Msg::Set(42),
                Msg::Set(100),
                Msg::Mute(true),
                Msg::Mute(false)
            ]
        );
        drop(volume);
        assert_eq!(rx.try_recv(), Ok(Msg::Stop));
    }

    /// An output that keeps its state in memory and records every call.
    #[derive(Clone, Default)]
    struct Fake {
        state: Arc<Mutex<FakeState>>,
    }

    #[derive(Default)]
    struct FakeState {
        reading: Option<Reading>,
        calls: Vec<String>,
        /// Results for the next `open` calls; `true` opens, `false` fails. Empty opens.
        opens: VecDeque<bool>,
        /// How many of the next `set_level` and `set_muted` calls fail.
        failing_sets: u32,
    }

    impl Fake {
        fn with(level: u8, muted: bool) -> Self {
            let fake = Self::default();
            fake.state.lock().unwrap().reading = Some(Reading { level, muted });
            fake
        }

        fn open(&self) -> anyhow::Result<Self> {
            let mut st = self.state.lock().unwrap();
            st.calls.push("open".into());
            if st.opens.pop_front().unwrap_or(true) && st.reading.is_some() {
                Ok(self.clone())
            } else {
                anyhow::bail!("no output")
            }
        }

        fn calls(&self) -> Vec<String> {
            self.state.lock().unwrap().calls.clone()
        }
    }

    impl Output for Fake {
        fn read(&mut self) -> anyhow::Result<Reading> {
            self.state
                .lock()
                .unwrap()
                .reading
                .ok_or_else(|| anyhow::anyhow!("gone"))
        }

        fn set_level(&mut self, level: u8) -> anyhow::Result<()> {
            let mut st = self.state.lock().unwrap();
            st.calls.push(format!("level {level}"));
            if st.failing_sets > 0 {
                st.failing_sets -= 1;
                anyhow::bail!("the sink moved away");
            }
            let r = st.reading.as_mut().ok_or_else(|| anyhow::anyhow!("gone"))?;
            r.level = level;
            if level > 0 {
                r.muted = false;
            }
            Ok(())
        }

        fn set_muted(&mut self, muted: bool) -> anyhow::Result<()> {
            let mut st = self.state.lock().unwrap();
            st.calls.push(format!("mute {muted}"));
            if st.failing_sets > 0 {
                st.failing_sets -= 1;
                anyhow::bail!("the sink moved away");
            }
            st.reading
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("gone"))?
                .muted = muted;
            Ok(())
        }
    }

    fn reading_of(s: &MediaState) -> (Option<u8>, Option<bool>) {
        (s.volume, s.muted)
    }

    #[test]
    fn the_loop_reports_changes_once_and_applies_the_newest_level() {
        let fake = Fake::with(30, false);
        let (tx, rx) = std_mpsc::channel();
        let (out, mut states) = mpsc::channel(16);
        // Everything is queued before the loop starts, so it reads it as one burst.
        for msg in [Msg::Set(10), Msg::Set(20), Msg::Set(50), Msg::Mute(true)] {
            tx.send(msg).unwrap();
        }
        let f = fake.clone();
        let worker = std::thread::spawn(move || run(&rx, &out, || f.open()));
        let first = states.blocking_recv().unwrap();
        assert_eq!(reading_of(&first), (Some(30), Some(false)));
        let next = states.blocking_recv().unwrap();
        assert_eq!(reading_of(&next), (Some(50), Some(true)));
        // A notice that changed nothing sends nothing; a level above zero unmutes.
        tx.send(Msg::Changed).unwrap();
        tx.send(Msg::Set(60)).unwrap();
        let next = states.blocking_recv().unwrap();
        assert_eq!(reading_of(&next), (Some(60), Some(false)));
        tx.send(Msg::Stop).unwrap();
        worker.join().unwrap();
        assert!(
            states.blocking_recv().is_none(),
            "the loop dropped its sender"
        );
        assert_eq!(
            fake.calls(),
            vec!["open", "level 50", "mute true", "level 60"],
            "only the newest level of a burst is applied"
        );
    }

    #[test]
    fn a_lost_output_is_reported_gone_and_found_again() {
        let fake = Fake::with(30, false);
        // First open works, the next one (after the loss) fails, then it is back.
        fake.state.lock().unwrap().opens = VecDeque::from([true, false, true]);
        let (tx, rx) = std_mpsc::channel();
        let (out, mut states) = mpsc::channel(16);
        let f = fake.clone();
        let worker = std::thread::spawn(move || run(&rx, &out, || f.open()));
        assert_eq!(
            reading_of(&states.blocking_recv().unwrap()),
            (Some(30), Some(false))
        );
        tx.send(Msg::Lost).unwrap();
        let g = states.blocking_recv().unwrap();
        assert!(g.can.is_empty(), "gone after the loss");
        // A changed default output ends the wait at once; the open fails again, nothing
        // more is reported, and the next change finds it.
        tx.send(Msg::DeviceChanged).unwrap();
        tx.send(Msg::DeviceChanged).unwrap();
        let back = states.blocking_recv().unwrap();
        assert_eq!(reading_of(&back), (Some(30), Some(false)));
        tx.send(Msg::Stop).unwrap();
        worker.join().unwrap();
        assert!(states.blocking_recv().is_none());
        assert_eq!(fake.calls(), vec!["open", "open", "open"]);
    }

    #[test]
    fn a_new_default_output_is_opened_and_only_a_different_reading_is_sent() {
        let fake = Fake::with(30, false);
        let (tx, rx) = std_mpsc::channel();
        let (out, mut states) = mpsc::channel(16);
        let f = fake.clone();
        let worker = std::thread::spawn(move || run(&rx, &out, || f.open()));
        states.blocking_recv().unwrap();
        // Same level on the new device: nothing to tell peers.
        tx.send(Msg::DeviceChanged).unwrap();
        // Another level on the next one.
        fake.state.lock().unwrap().reading = Some(Reading {
            level: 70,
            muted: true,
        });
        tx.send(Msg::DeviceChanged).unwrap();
        let next = states.blocking_recv().unwrap();
        assert_eq!(reading_of(&next), (Some(70), Some(true)));
        tx.send(Msg::Stop).unwrap();
        worker.join().unwrap();
        assert!(states.blocking_recv().is_none());
        assert!(fake.calls().iter().all(|c| c == "open"));
    }

    #[test]
    fn a_change_asked_for_during_a_device_switch_goes_to_the_new_output() {
        let fake = Fake::with(30, false);
        let (tx, rx) = std_mpsc::channel();
        let (out, mut states) = mpsc::channel(16);
        let f = fake.clone();
        let worker = std::thread::spawn(move || run(&rx, &out, || f.open()));
        states.blocking_recv().unwrap();
        // One burst: a drag, a mute and the switch, in any order. Sent while the loop
        // waits on the first message, so it may also see them in two bursts; either way
        // nothing is applied to the old output after the switch was heard.
        for msg in [Msg::Set(55), Msg::DeviceChanged, Msg::Mute(true)] {
            tx.send(msg).unwrap();
        }
        let next = states.blocking_recv().unwrap();
        let next = if next.muted == Some(true) {
            next
        } else {
            // The level went out before the switch was read; the mute follows.
            states.blocking_recv().unwrap()
        };
        assert_eq!(reading_of(&next), (Some(55), Some(true)));
        tx.send(Msg::Stop).unwrap();
        worker.join().unwrap();
        assert!(states.blocking_recv().is_none());
        let calls = fake.calls();
        assert!(calls.contains(&"level 55".to_owned()), "{calls:?}");
        assert_eq!(
            calls.last().map(String::as_str),
            Some("mute true"),
            "{calls:?}"
        );
        assert_eq!(
            calls.iter().filter(|c| *c == "open").count(),
            2,
            "{calls:?}"
        );
    }

    #[test]
    fn a_change_folded_with_a_switch_is_carried_not_applied() {
        let b = Batch::fold([Msg::Set(20), Msg::DeviceChanged, Msg::Mute(true)]);
        assert_eq!(b.end, Some(BatchEnd::Reopen));
        assert_eq!(
            b.change,
            Change {
                level: Some(20),
                muted: Some(true)
            }
        );
    }

    /// Every sequence of up to four levels and mutes, from a muted and an unmuted output:
    /// the folded change leaves the output exactly where applying the messages one by one
    /// does.
    #[test]
    fn folding_equals_applying_one_by_one() {
        let alphabet = [
            Msg::Set(0),
            Msg::Set(40),
            Msg::Set(70),
            Msg::Mute(true),
            Msg::Mute(false),
        ];
        let mut sequences: Vec<Vec<Msg>> = vec![Vec::new()];
        let mut last: Vec<Vec<Msg>> = vec![Vec::new()];
        for _ in 0..4 {
            let next: Vec<Vec<Msg>> = last
                .iter()
                .flat_map(|seq| {
                    alphabet.iter().map(move |m| {
                        let mut s = seq.clone();
                        s.push(*m);
                        s
                    })
                })
                .collect();
            sequences.extend(next.iter().cloned());
            last = next;
        }
        for start_muted in [false, true] {
            for seq in &sequences {
                let mut one_by_one = Fake::with(30, start_muted);
                for msg in seq {
                    match *msg {
                        Msg::Set(l) => one_by_one.set_level(l).unwrap(),
                        Msg::Mute(m) => one_by_one.set_muted(m).unwrap(),
                        _ => unreachable!(),
                    }
                }
                let mut folded = Fake::with(30, start_muted);
                Batch::fold(seq.iter().copied())
                    .change
                    .apply(&mut folded)
                    .unwrap();
                assert_eq!(
                    folded.read().unwrap(),
                    one_by_one.read().unwrap(),
                    "{seq:?} from muted {start_muted}"
                );
            }
        }
    }

    #[test]
    fn a_failed_change_keeps_the_slider() {
        let fake = Fake::with(30, false);
        let (tx, rx) = std_mpsc::channel();
        let (out, mut states) = mpsc::channel(16);
        let f = fake.clone();
        let worker = std::thread::spawn(move || run(&rx, &out, || f.open()));
        assert_eq!(
            reading_of(&states.blocking_recv().unwrap()),
            (Some(30), Some(false))
        );
        // The set fails, the read after it works: no "gone", and the next change applies.
        fake.state.lock().unwrap().failing_sets = 1;
        tx.send(Msg::Set(80)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !fake.calls().contains(&"level 80".to_owned()) {
            assert!(Instant::now() < deadline, "the failing set was never tried");
            std::thread::sleep(Duration::from_millis(5));
        }
        tx.send(Msg::Set(60)).unwrap();
        let next = states.blocking_recv().unwrap();
        assert!(!next.can.is_empty(), "the slider stays");
        assert_eq!(reading_of(&next), (Some(60), Some(false)));
        tx.send(Msg::Stop).unwrap();
        worker.join().unwrap();
        assert!(states.blocking_recv().is_none());
        assert_eq!(
            fake.calls().iter().filter(|c| *c == "open").count(),
            1,
            "never reopened"
        );
    }

    #[test]
    fn republish_sends_the_same_state_again() {
        let fake = Fake::with(30, true);
        let (tx, rx) = std_mpsc::channel();
        let (out, mut states) = mpsc::channel(16);
        let f = fake.clone();
        let worker = std::thread::spawn(move || run(&rx, &out, || f.open()));
        let first = states.blocking_recv().unwrap();
        let volume = SystemVolume { tx: tx.clone() };
        volume.republish();
        let again = states.blocking_recv().unwrap();
        assert_eq!(again, first);
        drop(volume);
        worker.join().unwrap();
        assert!(states.blocking_recv().is_none());
    }

    #[test]
    fn with_no_output_nothing_is_reported_and_stop_ends_the_wait() {
        let (tx, rx) = std_mpsc::channel();
        let (out, mut states) = mpsc::channel(16);
        let worker = std::thread::spawn(move || {
            run::<Fake>(&rx, &out, || anyhow::bail!("no sound device"));
        });
        tx.send(Msg::Set(10)).unwrap();
        tx.send(Msg::Stop).unwrap();
        worker.join().unwrap();
        assert!(states.blocking_recv().is_none(), "no state, not even gone");
    }

    #[test]
    fn the_wait_ends_early_only_for_a_changed_device() {
        let (tx, rx) = std_mpsc::channel();
        tx.send(Msg::Lost).unwrap();
        tx.send(Msg::Set(3)).unwrap();
        tx.send(Msg::DeviceChanged).unwrap();
        let started = Instant::now();
        assert!(wait_to_retry(&rx, Duration::from_secs(30)));
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(
            wait_to_retry(&rx, Duration::from_millis(20)),
            "timeout retries"
        );
        tx.send(Msg::Stop).unwrap();
        assert!(!wait_to_retry(&rx, Duration::from_secs(30)));
    }
}
