//! Linux backend: MPRIS2 players on the session bus (protocol.md 5.9, see `mod.rs`).
//!
//! One manager task follows `NameOwnerChanged` for the `org.mpris.MediaPlayer2` namespace
//! only (an `arg0namespace` match, so the bus never wakes us for other names), after one
//! `ListNames` for the players already running. Each player gets one task that reads
//! everything once with `GetAll`, then lives on the `PropertiesChanged` payloads and the
//! `Seeked` signal. `Position` never emits a change signal (MPRIS spec), so it is read on
//! demand when the status, track or rate changes, and taken from `Seeked` otherwise.
//!
//! PLAN.md anti-pattern 10: KDE Connect relayed the raw MPRIS signal flood (about 4 signals
//! a second) and drained phone batteries. Here a signal that changes nothing we report is
//! dropped, a burst collapses to the newest state per player in [`Latest`], and the core
//! coalesces the wire to 2 frames per second per player.
//!
//! `playerctld` is skipped: it re-exports whichever player is active under its own name,
//! so listing it would show every track twice.
//!
//! Titles and artists are the user's listening history and are logged at debug only
//! (SR-32 spirit).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use futures_util::StreamExt as _;
use playdrop_core::host::MediaState;
use playdrop_core::session::types::MediaAction;
use tokio::sync::{mpsc, OnceCell};
use tokio::task::JoinHandle;
use zbus::fdo::{DBusProxy, PropertiesProxy};
use zbus::names::InterfaceName;
use zbus::proxy::CacheProperties;
use zbus::zvariant::{ObjectPath, Value};

use super::latest::Latest;
use super::unsupported::{error, unsupported};
use super::MediaSupport;

/// Backend name for diagnostics.
const BACKEND: &str = "mpris";
/// Every MPRIS player owns a bus name with this prefix.
const PREFIX: &str = "org.mpris.MediaPlayer2.";
/// The namespace the `NameOwnerChanged` match is limited to.
const NAMESPACE: &str = "org.mpris.MediaPlayer2";
/// Object path every MPRIS player serves.
const PATH: &str = "/org/mpris/MediaPlayer2";
/// Root interface (`Identity`).
const ROOT_IFACE: &str = "org.mpris.MediaPlayer2";
/// Player interface (status, metadata, controls).
const PLAYER_IFACE: &str = "org.mpris.MediaPlayer2.Player";
/// The track ID MPRIS uses for "no track"; `SetPosition` cannot target it.
const NO_TRACK: &str = "/org/mpris/MediaPlayer2/TrackList/NoTrack";
/// Player IDs that mirror other players.
const IGNORED: &[&str] = &["playerctld"];
/// Shown when a command names a player that has gone.
const GONE: &str = "that player is no longer running";

#[zbus::proxy(
    interface = "org.mpris.MediaPlayer2.Player",
    default_path = "/org/mpris/MediaPlayer2",
    gen_blocking = false
)]
trait Player {
    fn play(&self) -> zbus::Result<()>;
    fn pause(&self) -> zbus::Result<()>;
    fn play_pause(&self) -> zbus::Result<()>;
    fn stop(&self) -> zbus::Result<()>;
    fn next(&self) -> zbus::Result<()>;
    fn previous(&self) -> zbus::Result<()>;
    fn seek(&self, offset: i64) -> zbus::Result<()>;
    fn set_position(&self, track_id: &ObjectPath<'_>, position: i64) -> zbus::Result<()>;
    #[zbus(property(emits_changed_signal = "false"))]
    fn position(&self) -> zbus::Result<i64>;
    #[zbus(property)]
    fn set_volume(&self, volume: f64) -> zbus::Result<()>;
    #[zbus(signal)]
    fn seeked(&self, position: i64) -> zbus::Result<()>;
}

/// Probes for a session bus once and caches the answer.
pub(super) async fn support() -> MediaSupport {
    static SUPPORT: OnceCell<MediaSupport> = OnceCell::const_new();
    SUPPORT
        .get_or_init(|| async {
            match zbus::Connection::session().await {
                Ok(_) => MediaSupport {
                    supported: true,
                    backend: BACKEND,
                    reason: None,
                },
                Err(e) => {
                    tracing::info!(error = %e, "no session bus; media control is unavailable");
                    unsupported(
                        "media control needs the desktop session bus (D-Bus), and none was found",
                    )
                }
            }
        })
        .await
        .clone()
}

/// What the daemon keeps per player for commands and the "gone" marker.
#[derive(Debug)]
struct Slot {
    /// Full bus name, `org.mpris.MediaPlayer2.<id>`.
    bus: String,
    /// Increments per (re)start, so a task that outlived its slot cannot overwrite it.
    generation: u64,
    task: Option<JoinHandle<()>>,
    name: Option<String>,
    track_id: Option<String>,
    can: Vec<MediaAction>,
}

/// State shared by the manager, the player tasks and [`Watcher::command`].
#[derive(Debug, Default)]
struct Shared {
    players: Mutex<HashMap<String, Slot>>,
    next_generation: Mutex<u64>,
    out: Latest,
}

impl Shared {
    fn players(&self) -> std::sync::MutexGuard<'_, HashMap<String, Slot>> {
        self.players.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Records and queues a player's state, unless its slot was replaced or removed.
    fn publish(&self, id: &str, generation: u64, snap: &Snapshot) {
        let state = snap.to_state(id, unix_ms());
        let mut players = self.players();
        let Some(slot) = players.get_mut(id).filter(|s| s.generation == generation) else {
            return;
        };
        slot.name.clone_from(&state.name);
        slot.track_id.clone_from(&snap.track_id);
        slot.can.clone_from(&state.can);
        tracing::debug!(
            player = id,
            playing = state.playing,
            title = ?state.title,
            artist = ?state.artist,
            can = state.can.len(),
            "media state"
        );
        self.out.push(state);
    }
}

/// Watches every MPRIS player on the session bus.
pub(super) struct Watcher {
    conn: zbus::Connection,
    shared: Arc<Shared>,
    manager: JoinHandle<()>,
    forwarder: JoinHandle<()>,
}

impl Watcher {
    pub(super) async fn start(tx: mpsc::Sender<MediaState>) -> anyhow::Result<Self> {
        let support = support().await;
        if !support.supported {
            return Err(error(&support));
        }
        let conn = zbus::Connection::session()
            .await
            .context("the desktop session bus is not reachable")?;
        let shared = Arc::new(Shared::default());

        // Subscribe before listing, so a player that starts in between is not missed;
        // adding a player twice is a no-op.
        let rule = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender("org.freedesktop.DBus")?
            .interface("org.freedesktop.DBus")?
            .member("NameOwnerChanged")?
            .arg0ns(NAMESPACE)?
            .build();
        let owners = zbus::MessageStream::for_match_rule(rule, &conn, None).await?;
        let names = DBusProxy::new(&conn).await?.list_names().await?;

        for name in &names {
            add_player(&conn, &shared, name.as_str());
        }

        let manager = tokio::spawn(follow_owners(conn.clone(), Arc::clone(&shared), owners));
        let forwarder = tokio::spawn({
            let shared = Arc::clone(&shared);
            async move { shared.out.forward(tx).await }
        });
        Ok(Self {
            conn,
            shared,
            manager,
            forwarder,
        })
    }

    pub(super) async fn command(
        &self,
        player: &str,
        action: MediaAction,
        value: Option<u64>,
    ) -> anyhow::Result<()> {
        let (bus, track_id, can) = {
            let players = self.shared.players();
            let slot = players.get(player).ok_or_else(|| anyhow::anyhow!(GONE))?;
            (slot.bus.clone(), slot.track_id.clone(), slot.can.clone())
        };
        if let MediaAction::Unknown(a) = &action {
            anyhow::bail!("PlayDrop does not know the media action \"{a}\"");
        }
        if !can.contains(&action) {
            anyhow::bail!("this player does not allow that right now");
        }
        let proxy = PlayerProxy::builder(&self.conn)
            .destination(bus)?
            .cache_properties(CacheProperties::No)
            .build()
            .await?;
        let result = match action {
            MediaAction::Play => proxy.play().await,
            MediaAction::Pause => proxy.pause().await,
            MediaAction::PlayPause => proxy.play_pause().await,
            MediaAction::Stop => proxy.stop().await,
            MediaAction::Next => proxy.next().await,
            MediaAction::Previous => proxy.previous().await,
            MediaAction::Seek => {
                let ms = value.context("a seek needs a position")?;
                seek(&proxy, track_id.as_deref(), ms_to_us(ms)).await
            }
            MediaAction::Volume => {
                let v = value.context("a volume change needs a level from 0 to 100")?;
                proxy.set_volume(volume_to_mpris(v)).await
            }
            MediaAction::Unknown(_) => unreachable!("refused above"),
        };
        result.map_err(|e| {
            tracing::debug!(player, error = %e, "media command failed");
            match e {
                zbus::Error::MethodError(name, _, _)
                    if name.as_str() == "org.freedesktop.DBus.Error.ServiceUnknown" =>
                {
                    anyhow::anyhow!(GONE)
                }
                _ => anyhow::anyhow!("the player did not accept the command"),
            }
        })
    }

    pub(super) fn stop(self) {
        // `Drop` aborts every task.
        drop(self);
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.manager.abort();
        self.forwarder.abort();
        for slot in self.shared.players().values_mut() {
            if let Some(t) = slot.task.take() {
                t.abort();
            }
        }
    }
}

/// Seeks to `target_us`: `SetPosition` with the current track ID when there is one, else a
/// relative `Seek` from the current `Position` (players without track IDs, or between
/// tracks).
async fn seek(proxy: &PlayerProxy<'_>, track_id: Option<&str>, target_us: i64) -> zbus::Result<()> {
    if let Some(path) = track_id
        .filter(|t| *t != NO_TRACK)
        .and_then(|t| ObjectPath::try_from(t).ok())
    {
        return proxy.set_position(&path, target_us).await;
    }
    let now = proxy.position().await?;
    proxy.seek(target_us.saturating_sub(now)).await
}

/// Follows players appearing, vanishing and changing owner.
async fn follow_owners(
    conn: zbus::Connection,
    shared: Arc<Shared>,
    mut owners: zbus::MessageStream,
) {
    while let Some(msg) = owners.next().await {
        let Ok(msg) = msg else { continue };
        let Ok((name, old, new)) = msg.body().deserialize::<(String, String, String)>() else {
            continue;
        };
        if !old.is_empty() {
            remove_player(&shared, &name);
        }
        if !new.is_empty() {
            add_player(&conn, &shared, &name);
        }
    }
    tracing::info!("the session bus stopped reporting media players");
}

/// Starts a task for `bus` if it is an MPRIS player not already watched.
fn add_player(conn: &zbus::Connection, shared: &Arc<Shared>, bus: &str) {
    let Some(id) = player_id(bus) else { return };
    let generation = {
        let mut players = shared.players();
        if players.contains_key(id) {
            return;
        }
        let generation = {
            let mut g = shared
                .next_generation
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            *g += 1;
            *g
        };
        players.insert(
            id.to_owned(),
            Slot {
                bus: bus.to_owned(),
                generation,
                task: None,
                name: None,
                track_id: None,
                can: Vec::new(),
            },
        );
        generation
    };
    tracing::debug!(player = id, "media player appeared");
    let task = tokio::spawn({
        let conn = conn.clone();
        let shared = Arc::clone(shared);
        let bus = bus.to_owned();
        let id = id.to_owned();
        async move {
            if let Err(e) = watch_player(&conn, &shared, &bus, &id, generation).await {
                tracing::debug!(player = %id, error = %e, "media player not readable");
            }
        }
    });
    let mut players = shared.players();
    match players.get_mut(id).filter(|s| s.generation == generation) {
        Some(slot) => slot.task = Some(task),
        None => task.abort(),
    }
}

/// Forgets a player and sends the "gone" marker (protocol.md 11.1).
fn remove_player(shared: &Shared, bus: &str) {
    let Some(id) = player_id(bus) else { return };
    let Some(mut slot) = shared.players().remove(id) else {
        return;
    };
    if let Some(t) = slot.task.take() {
        t.abort();
    }
    tracing::debug!(player = id, "media player went away");
    shared.out.push(gone(id, slot.name));
}

/// Reads one player once, then follows its signals until it goes away.
async fn watch_player(
    conn: &zbus::Connection,
    shared: &Shared,
    bus: &str,
    id: &str,
    generation: u64,
) -> zbus::Result<()> {
    let props = PropertiesProxy::builder(conn)
        .destination(bus.to_owned())?
        .path(PATH)?
        .cache_properties(CacheProperties::No)
        .build()
        .await?;
    let player = PlayerProxy::builder(conn)
        .destination(bus.to_owned())?
        .cache_properties(CacheProperties::No)
        .build()
        .await?;
    let player_iface = InterfaceName::from_static_str_unchecked(PLAYER_IFACE);
    let mut changes = props.receive_properties_changed().await?;
    let mut seeks = player.receive_seeked().await?;

    let mut snap = Snapshot {
        identity: props
            .get(
                InterfaceName::from_static_str_unchecked(ROOT_IFACE),
                "Identity",
            )
            .await
            .ok()
            .and_then(|v| value_string(&v)),
        ..Snapshot::default()
    };
    let all = props.get_all(player_iface.clone()).await?;
    snap.apply(all.iter().map(|(k, v)| (k.as_str(), &**v)));
    shared.publish(id, generation, &snap);

    loop {
        let before = snap.clone();
        tokio::select! {
            change = changes.next() => {
                let Some(change) = change else { break };
                let Ok(args) = change.args() else { continue };
                if args.interface_name != player_iface {
                    continue;
                }
                let effect = snap.apply(args.changed_properties.iter().map(|(k, v)| (*k, v)));
                if !args.invalidated_properties.is_empty() {
                    // Rare: the player says "ask me". Read everything again.
                    if let Ok(all) = props.get_all(player_iface.clone()).await {
                        snap.apply(all.iter().map(|(k, v)| (k.as_str(), &**v)));
                    }
                } else if effect.position_stale {
                    snap.position_us = player.position().await.ok();
                }
            }
            seek = seeks.next() => {
                let Some(seek) = seek else { break };
                if let Ok(args) = seek.args() {
                    snap.position_us = Some(args.position);
                }
            }
        }
        // A signal that changed nothing we report is dropped here (anti-pattern 10).
        if snap != before {
            shared.publish(id, generation, &snap);
        }
    }
    Ok(())
}

/// Unix time in ms.
fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

// ---- Pure mapping, testable without a bus ------------------------------------------------

/// The player ID for an MPRIS bus name: the name without its prefix, or `None` for a name
/// that is not a player (or one that mirrors other players).
fn player_id(bus: &str) -> Option<&str> {
    bus.strip_prefix(PREFIX)
        .filter(|id| !id.is_empty() && !IGNORED.contains(id))
}

/// The "gone" marker for a player (protocol.md 11.1): not playing, nothing allowed.
fn gone(id: &str, name: Option<String>) -> MediaState {
    MediaState {
        player: id.to_owned(),
        name,
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

/// The `Can*` properties of the Player interface.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools, reason = "mirrors the six MPRIS flags")]
struct Flags {
    play: bool,
    pause: bool,
    next: bool,
    previous: bool,
    seek: bool,
    control: bool,
}

impl Flags {
    /// The actions a peer may send. MPRIS says `CanControl = false` means every other
    /// `Can*` is false too and nothing may be called, so it gates everything.
    fn actions(self, has_volume: bool) -> Vec<MediaAction> {
        if !self.control {
            return Vec::new();
        }
        let mut can = Vec::with_capacity(8);
        if self.play {
            can.push(MediaAction::Play);
        }
        if self.pause {
            can.push(MediaAction::Pause);
        }
        if self.play || self.pause {
            can.push(MediaAction::PlayPause);
        }
        can.push(MediaAction::Stop);
        if self.next {
            can.push(MediaAction::Next);
        }
        if self.previous {
            can.push(MediaAction::Previous);
        }
        if self.seek {
            can.push(MediaAction::Seek);
        }
        if has_volume {
            can.push(MediaAction::Volume);
        }
        can
    }
}

/// Everything known about one player, kept current from signal payloads.
#[derive(Debug, Default, Clone, PartialEq)]
struct Snapshot {
    identity: Option<String>,
    status: Option<String>,
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    length_us: Option<i64>,
    track_id: Option<String>,
    position_us: Option<i64>,
    volume: Option<f64>,
    flags: Flags,
}

/// What applying a batch of properties means for the position sample.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Effect {
    /// The status, track or rate changed, so the last `Position` no longer extrapolates.
    position_stale: bool,
}

impl Snapshot {
    /// Applies Player interface properties (from `GetAll` or a `PropertiesChanged`
    /// payload). Unknown names are ignored.
    fn apply<'a, 'v: 'a>(
        &mut self,
        props: impl Iterator<Item = (&'a str, &'a Value<'v>)>,
    ) -> Effect {
        let mut effect = Effect::default();
        for (key, value) in props {
            let value = unwrap_variant(value);
            match key {
                "PlaybackStatus" => {
                    self.status = value_string(value);
                    effect.position_stale = true;
                }
                "Metadata" => {
                    self.apply_metadata(value);
                    effect.position_stale = true;
                }
                "Rate" => effect.position_stale = true,
                "Position" => self.position_us = value_i64(value),
                "Volume" => self.volume = value_f64(value),
                "CanPlay" => self.flags.play = value_bool(value),
                "CanPause" => self.flags.pause = value_bool(value),
                "CanGoNext" => self.flags.next = value_bool(value),
                "CanGoPrevious" => self.flags.previous = value_bool(value),
                "CanSeek" => self.flags.seek = value_bool(value),
                "CanControl" => self.flags.control = value_bool(value),
                _ => {}
            }
        }
        effect
    }

    /// Replaces the track fields with a `Metadata` dictionary (`a{sv}`). A key that is
    /// missing clears its field: `Metadata` always carries the whole track.
    fn apply_metadata(&mut self, value: &Value<'_>) {
        self.title = None;
        self.artist = None;
        self.album = None;
        self.length_us = None;
        self.track_id = None;
        let Value::Dict(dict) = value else { return };
        for (k, v) in dict.iter() {
            let Value::Str(k) = k else { continue };
            let v = unwrap_variant(v);
            match k.as_str() {
                "xesam:title" => self.title = value_string(v),
                "xesam:artist" => self.artist = join_artists(v),
                "xesam:album" => self.album = value_string(v),
                "mpris:length" => self.length_us = value_i64(v),
                "mpris:trackid" => self.track_id = value_string(v),
                _ => {}
            }
        }
    }

    /// The state reported for this player, with the position sampled at `now_ms`.
    fn to_state(&self, id: &str, now_ms: u64) -> MediaState {
        let position_ms = self.position_us.and_then(us_to_ms);
        MediaState {
            player: id.to_owned(),
            name: self.identity.clone(),
            playing: self.status.as_deref() == Some("Playing"),
            title: self.title.clone(),
            artist: self.artist.clone(),
            album: self.album.clone(),
            duration_ms: self.length_us.and_then(us_to_ms).filter(|&ms| ms > 0),
            position_ms,
            position_at_ms: position_ms.map(|_| now_ms),
            volume: self.volume.map(volume_to_percent),
            can: self.flags.actions(self.volume.is_some()),
        }
    }
}

/// Strips the `v` wrappers MPRIS puts around dictionary values.
fn unwrap_variant<'a, 'v>(mut value: &'a Value<'v>) -> &'a Value<'v> {
    while let Value::Value(inner) = value {
        value = inner;
    }
    value
}

/// A non-empty string, also from an object path (`mpris:trackid` is `o`, some players send
/// `s`).
fn value_string(value: &Value<'_>) -> Option<String> {
    let s = match unwrap_variant(value) {
        Value::Str(s) => s.as_str(),
        Value::ObjectPath(p) => p.as_str(),
        _ => return None,
    };
    (!s.is_empty()).then(|| s.to_owned())
}

/// `xesam:artist` is a string array; some players send one string. Joined with ", ".
fn join_artists(value: &Value<'_>) -> Option<String> {
    match unwrap_variant(value) {
        Value::Array(a) => {
            let names: Vec<String> = a.iter().filter_map(value_string).collect();
            (!names.is_empty()).then(|| names.join(", "))
        }
        other => value_string(other),
    }
}

/// An integer in any width. `mpris:length` is `x` by the spec, but players send `t`, `i`
/// and even `d`.
#[allow(
    clippy::cast_possible_truncation,
    reason = "a microsecond count from a double fits an i64 for any real track"
)]
fn value_i64(value: &Value<'_>) -> Option<i64> {
    match unwrap_variant(value) {
        Value::I64(v) => Some(*v),
        Value::U64(v) => i64::try_from(*v).ok(),
        Value::I32(v) => Some(i64::from(*v)),
        Value::U32(v) => Some(i64::from(*v)),
        Value::I16(v) => Some(i64::from(*v)),
        Value::U16(v) => Some(i64::from(*v)),
        Value::U8(v) => Some(i64::from(*v)),
        Value::F64(v) if v.is_finite() => Some(v.round() as i64),
        _ => None,
    }
}

#[allow(
    clippy::cast_precision_loss,
    reason = "volumes are small integers when sent as integers"
)]
fn value_f64(value: &Value<'_>) -> Option<f64> {
    match unwrap_variant(value) {
        Value::F64(v) if v.is_finite() => Some(*v),
        other => value_i64(other).map(|v| v as f64),
    }
}

fn value_bool(value: &Value<'_>) -> bool {
    matches!(unwrap_variant(value), Value::Bool(true))
}

/// Microseconds to ms; negative values (unknown) are dropped.
fn us_to_ms(us: i64) -> Option<u64> {
    u64::try_from(us).ok().map(|us| us / 1000)
}

/// ms to microseconds, saturating.
fn ms_to_us(ms: u64) -> i64 {
    i64::try_from(ms.saturating_mul(1000)).unwrap_or(i64::MAX)
}

/// MPRIS volume (0.0 to 1.0, players may exceed 1.0) to 0 to 100.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to 0..=100 first"
)]
fn volume_to_percent(v: f64) -> u8 {
    (v.clamp(0.0, 1.0) * 100.0).round() as u8
}

/// 0 to 100 (clamped) to an MPRIS volume.
fn volume_to_mpris(v: u64) -> f64 {
    f64::from(u8::try_from(v.min(100)).unwrap_or(100)) / 100.0
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn metadata(entries: Vec<(&'static str, Value<'static>)>) -> Value<'static> {
        let map: HashMap<&str, Value<'static>> = entries.into_iter().collect();
        Value::from(map)
    }

    fn all_flags() -> Flags {
        Flags {
            play: true,
            pause: true,
            next: true,
            previous: true,
            seek: true,
            control: true,
        }
    }

    #[test]
    fn player_id_strips_prefix_and_skips_non_players() {
        assert_eq!(player_id("org.mpris.MediaPlayer2.spotify"), Some("spotify"));
        assert_eq!(
            player_id("org.mpris.MediaPlayer2.firefox.instance_1_42"),
            Some("firefox.instance_1_42")
        );
        assert_eq!(player_id("org.mpris.MediaPlayer2."), None);
        assert_eq!(player_id("org.mpris.MediaPlayer2"), None);
        assert_eq!(player_id("org.freedesktop.DBus"), None);
        assert_eq!(player_id("org.mpris.MediaPlayer2.playerctld"), None);
    }

    #[test]
    fn artists_are_joined() {
        let v = Value::from(vec!["A", "B"]);
        assert_eq!(join_artists(&v).as_deref(), Some("A, B"));
        assert_eq!(join_artists(&Value::from("Solo")).as_deref(), Some("Solo"));
        assert_eq!(join_artists(&Value::from(Vec::<&str>::new())), None);
        assert_eq!(
            join_artists(&Value::from(vec!["", "C"])).as_deref(),
            Some("C")
        );
    }

    #[test]
    fn lengths_convert_from_any_width() {
        assert_eq!(
            value_i64(&Value::I64(215_000_000)).and_then(us_to_ms),
            Some(215_000)
        );
        assert_eq!(value_i64(&Value::U64(1_500)).and_then(us_to_ms), Some(1));
        assert_eq!(value_i64(&Value::F64(2_000_000.4)), Some(2_000_000));
        assert_eq!(value_i64(&Value::new(Value::I32(7))), Some(7));
        assert_eq!(us_to_ms(-1), None);
        assert_eq!(ms_to_us(90_000), 90_000_000);
        assert_eq!(ms_to_us(u64::MAX), i64::MAX);
    }

    #[test]
    fn volume_maps_both_ways() {
        assert_eq!(volume_to_percent(0.7), 70);
        assert_eq!(volume_to_percent(0.005), 1);
        assert_eq!(volume_to_percent(1.5), 100);
        assert_eq!(volume_to_percent(-0.2), 0);
        assert!((volume_to_mpris(70) - 0.7).abs() < f64::EPSILON);
        assert!((volume_to_mpris(250) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn can_follows_flags() {
        let all = all_flags().actions(true);
        assert_eq!(
            all,
            vec![
                MediaAction::Play,
                MediaAction::Pause,
                MediaAction::PlayPause,
                MediaAction::Stop,
                MediaAction::Next,
                MediaAction::Previous,
                MediaAction::Seek,
                MediaAction::Volume,
            ]
        );
        let pause_only = Flags {
            pause: true,
            control: true,
            ..Flags::default()
        };
        assert_eq!(
            pause_only.actions(false),
            vec![
                MediaAction::Pause,
                MediaAction::PlayPause,
                MediaAction::Stop
            ]
        );
        let no_control = Flags {
            control: false,
            ..all_flags()
        };
        assert_eq!(
            no_control.actions(true),
            Vec::<MediaAction>::new(),
            "CanControl gates everything"
        );
    }

    #[test]
    fn snapshot_maps_to_state() {
        let mut snap = Snapshot {
            identity: Some("Spotify".into()),
            ..Snapshot::default()
        };
        let meta = metadata(vec![
            ("xesam:title", Value::from("Song")),
            ("xesam:artist", Value::from(vec!["A", "B"])),
            ("xesam:album", Value::from("Album")),
            ("mpris:length", Value::I64(215_000_000)),
            (
                "mpris:trackid",
                Value::from(ObjectPath::try_from("/com/spotify/track/1").unwrap()),
            ),
        ]);
        let status = Value::from("Playing");
        let pos = Value::I64(61_200_000);
        let vol = Value::F64(0.7);
        let t = Value::Bool(true);
        let props: Vec<(&str, &Value<'_>)> = vec![
            ("PlaybackStatus", &status),
            ("Metadata", &meta),
            ("Position", &pos),
            ("Volume", &vol),
            ("CanPlay", &t),
            ("CanPause", &t),
            ("CanGoNext", &t),
            ("CanGoPrevious", &t),
            ("CanSeek", &t),
            ("CanControl", &t),
            ("SomethingNew", &t),
        ];
        let effect = snap.apply(props.into_iter());
        assert!(effect.position_stale);
        assert_eq!(snap.track_id.as_deref(), Some("/com/spotify/track/1"));

        let s = snap.to_state("spotify", 1_791_460_800_123);
        assert_eq!(s.player, "spotify");
        assert_eq!(s.name.as_deref(), Some("Spotify"));
        assert!(s.playing);
        assert_eq!(s.title.as_deref(), Some("Song"));
        assert_eq!(s.artist.as_deref(), Some("A, B"));
        assert_eq!(s.album.as_deref(), Some("Album"));
        assert_eq!(s.duration_ms, Some(215_000));
        assert_eq!(s.position_ms, Some(61_200));
        assert_eq!(s.position_at_ms, Some(1_791_460_800_123));
        assert_eq!(s.volume, Some(70));
        assert_eq!(s.can, all_flags().actions(true));
    }

    #[test]
    fn changed_payload_updates_only_what_it_carries() {
        let mut snap = Snapshot::default();
        let meta = metadata(vec![("xesam:title", Value::from("One"))]);
        snap.apply(std::iter::once(("Metadata", &meta)));
        let vol = Value::new(Value::F64(0.25));
        let effect = snap.apply(std::iter::once(("Volume", &vol)));
        assert!(
            !effect.position_stale,
            "a volume change keeps the position sample"
        );
        assert_eq!(snap.title.as_deref(), Some("One"));
        assert_eq!(snap.volume, Some(0.25));

        // New metadata replaces the whole track, so missing keys clear.
        let meta = metadata(vec![("xesam:album", Value::from("Two"))]);
        snap.apply(std::iter::once(("Metadata", &meta)));
        assert_eq!(snap.title, None);
        assert_eq!(snap.album.as_deref(), Some("Two"));

        let paused = Value::from("Paused");
        snap.apply(std::iter::once(("PlaybackStatus", &paused)));
        let s = snap.to_state("x", 5);
        assert!(!s.playing);
        assert_eq!(s.position_ms, None, "no sample, no timestamp");
        assert_eq!(s.position_at_ms, None);
        assert_eq!(s.duration_ms, None);
    }

    #[test]
    fn gone_marker_is_not_playing_with_empty_can() {
        let g = gone("vlc", Some("VLC".into()));
        assert_eq!(g.player, "vlc");
        assert_eq!(g.name.as_deref(), Some("VLC"));
        assert!(!g.playing);
        assert_eq!(g.can, Vec::<MediaAction>::new());
    }

    /// Run with a player open:
    /// `cargo test -p playdropd -- --ignored media_mpris_live --nocapture`.
    #[tokio::test]
    #[ignore = "needs a session bus and, to see anything, a running MPRIS player"]
    async fn media_mpris_live() {
        let (tx, mut rx) = mpsc::channel(16);
        let watcher = Watcher::start(tx).await.expect("watcher starts");
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
        let mut seen = 0;
        while let Ok(Some(state)) = tokio::time::timeout_at(deadline, rx.recv()).await {
            seen += 1;
            println!("{state:?}");
        }
        println!("{seen} states in 3 s");
        watcher.stop();
    }
}
