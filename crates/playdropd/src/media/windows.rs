//! Windows backend: `GlobalSystemMediaTransportControlsSessionManager` (GSMTC, `WinRT`;
//! protocol.md 5.9, see `mod.rs`). Type-checked on Linux, never run there.
//!
//! One session per app that shows the Windows media overlay; its player ID is the
//! `SourceAppUserModelId`. `WinRT` raises events on its own threads, which must not wait,
//! so the handlers only mark a session dirty (a set plus a [`Notify`]); one tokio task
//! reads the dirty sessions and turns them into [`MediaState`]s. A burst of events for one
//! session is read once. Timeline refreshes that only confirm the extrapolated position are
//! dropped (`Sample::differs_from`): browsers re-send their timeline every few seconds,
//! and relaying that is the flood of PLAN.md anti-pattern 10. The core then coalesces to 2
//! frames per second per player.
//!
//! GSMTC has no volume control, so `volume` is never offered in `can` and a `volume`
//! command is refused. GSMTC gives no app display name either; `display_name` derives one
//! from the app ID.
//!
//! Titles and artists are logged at debug only (SR-32 spirit).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use ::windows::Foundation::TypedEventHandler;
use ::windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as PlaybackStatus,
};
use playdrop_core::host::MediaState;
use playdrop_core::session::types::MediaAction;
use tokio::sync::{mpsc, Notify, OnceCell};
use tokio::task::JoinHandle;

use super::gsmtc_model::{display_name, ms_to_ticks, Controls, Sample, Status};
use super::latest::Latest;
use super::unsupported::{error, unsupported};
use super::MediaSupport;

/// Backend name for diagnostics.
const BACKEND: &str = "gsmtc";
/// Shown when a command names a player that has gone.
const GONE: &str = "that player is no longer running";

/// Asks for the session manager once and caches the answer.
pub(super) async fn support() -> MediaSupport {
    static SUPPORT: OnceCell<MediaSupport> = OnceCell::const_new();
    SUPPORT
        .get_or_init(|| async {
            match request_manager().await {
                Ok(_) => MediaSupport {
                    supported: true,
                    backend: BACKEND,
                    reason: None,
                },
                Err(e) => {
                    tracing::info!(error = %e, "media session manager unavailable");
                    unsupported(
                        "media control needs the Windows media controls (Windows 10 1809 or later), which are not available here",
                    )
                }
            }
        })
        .await
        .clone()
}

async fn request_manager() -> ::windows::core::Result<Manager> {
    Manager::RequestAsync()?.await
}

/// What the `WinRT` handlers touch: which sessions changed. Kept apart from the session
/// table so a handler (owned by a `WinRT` object) never holds a reference back to it.
#[derive(Debug, Default)]
struct Dirty {
    marks: Mutex<Marks>,
    notify: Notify,
}

#[derive(Debug, Default)]
struct Marks {
    sessions: bool,
    players: HashSet<String>,
}

impl Dirty {
    fn mark_sessions(&self) {
        self.marks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .sessions = true;
        self.notify.notify_one();
    }

    fn mark_player(&self, id: &str) {
        self.marks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .players
            .insert(id.to_owned());
        self.notify.notify_one();
    }

    fn take(&self) -> Marks {
        std::mem::take(&mut *self.marks.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

/// One watched session and the event registrations to undo.
struct Entry {
    session: Session,
    media_token: i64,
    playback_token: i64,
    timeline_token: i64,
    name: String,
    last: Option<Sample>,
}

impl Entry {
    fn unregister(&self) {
        let _ = self.session.RemoveMediaPropertiesChanged(self.media_token);
        let _ = self.session.RemovePlaybackInfoChanged(self.playback_token);
        let _ = self
            .session
            .RemoveTimelinePropertiesChanged(self.timeline_token);
    }
}

/// State shared by the worker task and [`Watcher::command`].
struct Shared {
    manager: Manager,
    sessions: Mutex<HashMap<String, Entry>>,
    dirty: Arc<Dirty>,
    out: Latest,
}

impl Shared {
    fn sessions(&self) -> std::sync::MutexGuard<'_, HashMap<String, Entry>> {
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn session(&self, id: &str) -> Option<Session> {
        self.sessions().get(id).map(|e| e.session.clone())
    }
}

/// Watches every GSMTC session.
pub(super) struct Watcher {
    shared: Arc<Shared>,
    sessions_token: i64,
    worker: JoinHandle<()>,
    forwarder: JoinHandle<()>,
}

impl Watcher {
    pub(super) async fn start(tx: mpsc::Sender<MediaState>) -> anyhow::Result<Self> {
        let support = support().await;
        if !support.supported {
            return Err(error(&support));
        }
        let manager = request_manager().await?;
        let dirty = Arc::new(Dirty::default());
        let sessions_token = manager.SessionsChanged(&TypedEventHandler::new({
            let dirty = Arc::clone(&dirty);
            move |_, _| {
                dirty.mark_sessions();
                Ok(())
            }
        }))?;
        let shared = Arc::new(Shared {
            manager,
            sessions: Mutex::new(HashMap::new()),
            dirty: Arc::clone(&dirty),
            out: Latest::default(),
        });
        // The first pass reads every session that exists now.
        dirty.mark_sessions();
        let worker = tokio::spawn(run(Arc::clone(&shared)));
        let forwarder = tokio::spawn({
            let shared = Arc::clone(&shared);
            async move { shared.out.forward(tx).await }
        });
        Ok(Self {
            shared,
            sessions_token,
            worker,
            forwarder,
        })
    }

    pub(super) async fn command(
        &self,
        player: &str,
        action: MediaAction,
        value: Option<u64>,
    ) -> anyhow::Result<()> {
        let (session, last) = {
            let sessions = self.shared.sessions();
            let entry = sessions.get(player).ok_or_else(|| anyhow::anyhow!(GONE))?;
            (
                entry.session.clone(),
                entry.last.clone().unwrap_or_default(),
            )
        };
        match &action {
            MediaAction::Unknown(a) => {
                anyhow::bail!("PlayDrop does not know the media action \"{a}\"")
            }
            MediaAction::Volume => {
                anyhow::bail!("Windows does not let other apps change a player's volume")
            }
            a if !last.controls.actions().contains(a) => {
                anyhow::bail!("this player does not allow that right now")
            }
            _ => {}
        }
        let accepted = send(&session, &action, value, &last).await.map_err(|e| {
            tracing::debug!(player, error = %e, "media command failed");
            anyhow::anyhow!("the player did not accept the command")
        })?;
        if !accepted {
            anyhow::bail!("the player did not accept the command");
        }
        Ok(())
    }

    pub(super) fn stop(self) {
        // `Drop` aborts the tasks and removes every event registration.
        drop(self);
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.worker.abort();
        self.forwarder.abort();
        let _ = self
            .shared
            .manager
            .RemoveSessionsChanged(self.sessions_token);
        for entry in self.shared.sessions().values() {
            entry.unregister();
        }
    }
}

/// Sends one command. `Ok(false)` is the app saying no.
async fn send(
    session: &Session,
    action: &MediaAction,
    value: Option<u64>,
    last: &Sample,
) -> anyhow::Result<bool> {
    let op = match action {
        // Apps that cannot toggle get play or pause picked from the last status.
        MediaAction::PlayPause if last.controls.toggle => session.TryTogglePlayPauseAsync()?,
        MediaAction::PlayPause if last.status == Status::Playing => session.TryPauseAsync()?,
        MediaAction::Play | MediaAction::PlayPause => session.TryPlayAsync()?,
        MediaAction::Pause => session.TryPauseAsync()?,
        MediaAction::Stop => session.TryStopAsync()?,
        MediaAction::Next => session.TrySkipNextAsync()?,
        MediaAction::Previous => session.TrySkipPreviousAsync()?,
        MediaAction::Seek => {
            let ms = value.ok_or_else(|| anyhow::anyhow!("a seek needs a position"))?;
            // GSMTC positions count from the timeline start, in 100 ns ticks.
            let ticks = last.start_ticks.saturating_add(ms_to_ticks(ms));
            session.TryChangePlaybackPositionAsync(ticks)?
        }
        MediaAction::Volume | MediaAction::Unknown(_) => unreachable!("refused by the caller"),
    };
    Ok(op.await?)
}

/// The worker: waits for marks, then resyncs the session list and reads dirty sessions.
async fn run(shared: Arc<Shared>) {
    loop {
        let marks = shared.dirty.take();
        if marks.sessions {
            resync(&shared);
        }
        for id in marks.players {
            refresh(&shared, &id).await;
        }
        shared.dirty.notify.notified().await;
    }
}

/// Matches the table to `GetSessions`: new sessions are registered and marked dirty,
/// vanished ones unregistered and reported once as gone (protocol.md 11.1).
fn resync(shared: &Shared) {
    let current = match shared.manager.GetSessions() {
        Ok(list) => list,
        Err(e) => {
            tracing::debug!(error = %e, "media sessions not readable");
            return;
        }
    };
    let mut seen = HashSet::new();
    for session in &current {
        let Ok(id) = session.SourceAppUserModelId().map(|s| s.to_string_lossy()) else {
            continue;
        };
        if id.is_empty() || !seen.insert(id.clone()) {
            // Two sessions of one app share an ID; the first one wins.
            continue;
        }
        // An app that restarted between two passes keeps its ID but gets a new session
        // object; the old registrations point at the dead one.
        let stale = {
            let mut sessions = shared.sessions();
            match sessions.get(&id) {
                Some(entry) if entry.session == session => continue,
                Some(_) => sessions.remove(&id),
                None => None,
            }
        };
        if let Some(old) = stale {
            old.unregister();
        }
        match register(&shared.dirty, session, &id) {
            Ok(entry) => {
                tracing::debug!(player = %id, "media session appeared");
                shared.sessions().insert(id.clone(), entry);
                shared.dirty.mark_player(&id);
            }
            Err(e) => tracing::debug!(player = %id, error = %e, "media session not watched"),
        }
    }
    let gone: Vec<(String, Entry)> = {
        let mut sessions = shared.sessions();
        let ids: Vec<String> = sessions
            .keys()
            .filter(|id| !seen.contains(*id))
            .cloned()
            .collect();
        ids.into_iter()
            .filter_map(|id| sessions.remove(&id).map(|e| (id, e)))
            .collect()
    };
    for (id, entry) in gone {
        entry.unregister();
        tracing::debug!(player = %id, "media session went away");
        shared.out.push(gone_state(&id, &entry.name));
    }
}

/// Subscribes to one session's three change events.
fn register(dirty: &Arc<Dirty>, session: Session, id: &str) -> ::windows::core::Result<Entry> {
    let handler = |dirty: &Arc<Dirty>| {
        let dirty = Arc::clone(dirty);
        let id = id.to_owned();
        move || dirty.mark_player(&id)
    };
    let on_media = handler(dirty);
    let media_token = session.MediaPropertiesChanged(&TypedEventHandler::new(move |_, _| {
        on_media();
        Ok(())
    }))?;
    let on_playback = handler(dirty);
    let playback_token = session.PlaybackInfoChanged(&TypedEventHandler::new(move |_, _| {
        on_playback();
        Ok(())
    }))?;
    let on_timeline = handler(dirty);
    let timeline_token =
        session.TimelinePropertiesChanged(&TypedEventHandler::new(move |_, _| {
            on_timeline();
            Ok(())
        }))?;
    Ok(Entry {
        session,
        media_token,
        playback_token,
        timeline_token,
        name: display_name(id),
        last: None,
    })
}

/// Reads one session and queues its state when it says something new.
async fn refresh(shared: &Shared, id: &str) {
    let Some(session) = shared.session(id) else {
        return;
    };
    let sample = match read(&session).await {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!(player = id, error = %e, "media session not readable");
            return;
        }
    };
    let mut sessions = shared.sessions();
    let Some(entry) = sessions.get_mut(id) else {
        return;
    };
    if entry
        .last
        .as_ref()
        .is_some_and(|last| !last.differs_from(&sample))
    {
        return;
    }
    let state = sample.to_state(id, &entry.name, unix_ms());
    entry.last = Some(sample);
    tracing::debug!(
        player = id,
        playing = state.playing,
        title = ?state.title,
        artist = ?state.artist,
        can = state.can.len(),
        "media state"
    );
    shared.out.push(state);
}

/// One reading of a session: playback info and timeline are synchronous, the media
/// properties are an async call.
async fn read(session: &Session) -> ::windows::core::Result<Sample> {
    let info = session.GetPlaybackInfo()?;
    let status = if info.PlaybackStatus()? == PlaybackStatus::Playing {
        Status::Playing
    } else {
        Status::NotPlaying
    };
    let c = info.Controls()?;
    let controls = Controls {
        play: c.IsPlayEnabled()?,
        pause: c.IsPauseEnabled()?,
        toggle: c.IsPlayPauseToggleEnabled()?,
        stop: c.IsStopEnabled()?,
        next: c.IsNextEnabled()?,
        previous: c.IsPreviousEnabled()?,
        position: c.IsPlaybackPositionEnabled()?,
    };
    let timeline = session.GetTimelineProperties()?;
    let props = session.TryGetMediaPropertiesAsync()?.await?;
    Ok(Sample {
        status,
        title: props.Title()?.to_string_lossy(),
        artist: props.Artist()?.to_string_lossy(),
        album: props.AlbumTitle()?.to_string_lossy(),
        start_ticks: timeline.StartTime()?.Duration,
        end_ticks: timeline.EndTime()?.Duration,
        position_ticks: timeline.Position()?.Duration,
        updated_ticks: timeline.LastUpdatedTime()?.UniversalTime,
        controls,
    })
}

/// The "gone" marker (protocol.md 11.1): not playing, nothing allowed.
fn gone_state(id: &str, name: &str) -> MediaState {
    MediaState {
        player: id.to_owned(),
        name: Some(name.to_owned()).filter(|n| !n.is_empty()),
        playing: false,
        title: None,
        artist: None,
        album: None,
        duration_ms: None,
        position_ms: None,
        position_at_ms: None,
        volume: None,
        can: Vec::new(),
    }
}

/// Unix time in ms.
fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}
