//! Pure mapping for the Windows backend (`windows.rs`), kept free of `WinRT` types so it is
//! unit-tested on every platform.
//!
//! `GlobalSystemMediaTransportControls` (GSMTC) reports times as 100 ns ticks, the sample
//! time as a `DateTime` (100 ns ticks since 1601-01-01 UTC) and controls as `Is*Enabled`
//! flags. GSMTC has no volume, so `volume` is never in `can`.

use playdrop_core::host::MediaState;
use playdrop_core::session::types::MediaAction;

/// 100 ns ticks per ms.
const TICKS_PER_MS: i64 = 10_000;
/// Ticks between 1601-01-01 (Windows `DateTime`) and 1970-01-01 (Unix).
const UNIX_EPOCH_TICKS: i64 = 116_444_736_000_000_000;
/// A position that drifted from its extrapolation by less than this is not news: apps
/// such as browsers re-send their timeline every few seconds while playing, and relaying
/// those would be the signal flood of PLAN.md anti-pattern 10.
const DRIFT_MS: u64 = 1_500;

/// `GlobalSystemMediaTransportControlsSessionPlaybackStatus`, reduced to what we report.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) enum Status {
    /// Playing.
    Playing,
    /// Paused, stopped, opening, closed or changing.
    #[default]
    NotPlaying,
}

/// `GlobalSystemMediaTransportControlsSessionPlaybackControls`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools, reason = "mirrors the GSMTC flags")]
pub(super) struct Controls {
    pub(super) play: bool,
    pub(super) pause: bool,
    pub(super) toggle: bool,
    pub(super) stop: bool,
    pub(super) next: bool,
    pub(super) previous: bool,
    pub(super) position: bool,
}

impl Controls {
    /// The actions a peer may send. `PlayPause` is offered when the app toggles itself or
    /// allows play or pause (the command then picks one from the status).
    pub(super) fn actions(self) -> Vec<MediaAction> {
        let mut can = Vec::with_capacity(7);
        if self.play {
            can.push(MediaAction::Play);
        }
        if self.pause {
            can.push(MediaAction::Pause);
        }
        if self.toggle || self.play || self.pause {
            can.push(MediaAction::PlayPause);
        }
        if self.stop {
            can.push(MediaAction::Stop);
        }
        if self.next {
            can.push(MediaAction::Next);
        }
        if self.previous {
            can.push(MediaAction::Previous);
        }
        if self.position {
            can.push(MediaAction::Seek);
        }
        can
    }
}

/// One reading of a session.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct Sample {
    pub(super) status: Status,
    pub(super) title: String,
    pub(super) artist: String,
    pub(super) album: String,
    pub(super) start_ticks: i64,
    pub(super) end_ticks: i64,
    pub(super) position_ticks: i64,
    /// `LastUpdatedTime.UniversalTime`, when `position_ticks` was true.
    pub(super) updated_ticks: i64,
    pub(super) controls: Controls,
}

impl Sample {
    /// Track length in ms, `None` when the app reports no timeline.
    fn duration_ms(&self) -> Option<u64> {
        ticks_to_ms(self.end_ticks.saturating_sub(self.start_ticks)).filter(|&ms| ms > 0)
    }

    /// Position in ms from the track start, `None` without a timeline.
    fn position_ms(&self) -> Option<u64> {
        self.duration_ms()?;
        ticks_to_ms(self.position_ticks.saturating_sub(self.start_ticks))
    }

    /// The state reported for this session. The position is stamped with the time GSMTC
    /// sampled it, or `now_ms` when the app left that empty.
    pub(super) fn to_state(&self, id: &str, name: &str, now_ms: u64) -> MediaState {
        let position_ms = self.position_ms();
        MediaState {
            player: id.to_owned(),
            name: non_empty(name),
            playing: self.status == Status::Playing,
            title: non_empty(&self.title),
            artist: non_empty(&self.artist),
            album: non_empty(&self.album),
            duration_ms: self.duration_ms(),
            position_ms,
            position_at_ms: position_ms
                .map(|_| datetime_to_unix_ms(self.updated_ticks).unwrap_or(now_ms)),
            volume: None,
            can: self.controls.actions(),
        }
    }

    /// Whether `next` says anything `self` did not: any field other than the position, or
    /// a position more than [`DRIFT_MS`] away from where `self` extrapolates to.
    pub(super) fn differs_from(&self, next: &Self) -> bool {
        let same_rest = self.status == next.status
            && self.title == next.title
            && self.artist == next.artist
            && self.album == next.album
            && self.start_ticks == next.start_ticks
            && self.end_ticks == next.end_ticks
            && self.controls == next.controls;
        if !same_rest {
            return true;
        }
        let (Some(was), Some(now)) = (self.position_ms(), next.position_ms()) else {
            return self.position_ms() != next.position_ms();
        };
        let expected = if self.status == Status::Playing {
            let elapsed = ticks_to_ms(next.updated_ticks.saturating_sub(self.updated_ticks));
            was.saturating_add(elapsed.unwrap_or(0))
        } else {
            was
        };
        now.abs_diff(expected) > DRIFT_MS
    }
}

fn non_empty(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_owned())
}

/// 100 ns ticks to ms; negative values are dropped.
pub(super) fn ticks_to_ms(ticks: i64) -> Option<u64> {
    u64::try_from(ticks / TICKS_PER_MS).ok()
}

/// ms to 100 ns ticks, saturating.
pub(super) fn ms_to_ticks(ms: u64) -> i64 {
    i64::try_from(ms)
        .unwrap_or(i64::MAX)
        .saturating_mul(TICKS_PER_MS)
}

/// A Windows `DateTime` to Unix ms; `None` for an empty (zero) or pre-1970 time.
pub(super) fn datetime_to_unix_ms(universal_ticks: i64) -> Option<u64> {
    if universal_ticks <= 0 {
        return None;
    }
    ticks_to_ms(universal_ticks.checked_sub(UNIX_EPOCH_TICKS)?)
}

/// A display name for a `SourceAppUserModelId`. GSMTC gives no app name, and the
/// `ApplicationModel` lookup is not worth a dependency for a label, so: `Spotify.exe` ->
/// `Spotify`, `SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify` -> `Spotify`,
/// `Microsoft.ZuneMusic_8wekyb3d8bbwe!Microsoft.ZuneMusic` -> `ZuneMusic`, and a package
/// whose entry point is the generic `App` is named after the package.
pub(super) fn display_name(aumid: &str) -> String {
    let candidate = match aumid.split_once('!') {
        Some((_, app)) if !app.is_empty() && !app.eq_ignore_ascii_case("app") => app,
        Some((package, _)) => package.split('_').next().unwrap_or(package),
        None => {
            let file = aumid.rsplit(['\\', '/']).next().unwrap_or(aumid);
            let len = file.len();
            if len > 4
                && file.is_char_boundary(len - 4)
                && file[len - 4..].eq_ignore_ascii_case(".exe")
            {
                return file[..len - 4].to_owned();
            }
            return file.to_owned();
        }
    };
    candidate
        .rsplit('.')
        .find(|s| !s.is_empty())
        .unwrap_or(candidate)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEC: i64 = 10_000_000;

    fn playing_at(position_s: i64, updated_s: i64) -> Sample {
        Sample {
            status: Status::Playing,
            title: "Song".into(),
            artist: "Artist".into(),
            album: String::new(),
            start_ticks: 0,
            end_ticks: 215 * SEC,
            position_ticks: position_s * SEC,
            updated_ticks: UNIX_EPOCH_TICKS + updated_s * SEC,
            controls: Controls {
                play: true,
                pause: true,
                toggle: true,
                stop: false,
                next: true,
                previous: true,
                position: true,
            },
        }
    }

    #[test]
    fn ticks_convert() {
        assert_eq!(ticks_to_ms(215 * SEC), Some(215_000));
        assert_eq!(ticks_to_ms(-1), Some(0), "rounds toward zero first");
        assert_eq!(ticks_to_ms(-SEC), None);
        assert_eq!(ms_to_ticks(90_000), 90 * SEC);
        assert_eq!(ms_to_ticks(u64::MAX), i64::MAX);
        assert_eq!(datetime_to_unix_ms(UNIX_EPOCH_TICKS + SEC), Some(1_000));
        assert_eq!(datetime_to_unix_ms(0), None);
        assert_eq!(datetime_to_unix_ms(SEC), None);
    }

    #[test]
    fn sample_maps_to_state() {
        let s = playing_at(61, 1_791_460_800).to_state("Spotify.exe", "Spotify", 5);
        assert_eq!(s.player, "Spotify.exe");
        assert_eq!(s.name.as_deref(), Some("Spotify"));
        assert!(s.playing);
        assert_eq!(s.title.as_deref(), Some("Song"));
        assert_eq!(s.album, None);
        assert_eq!(s.duration_ms, Some(215_000));
        assert_eq!(s.position_ms, Some(61_000));
        assert_eq!(s.position_at_ms, Some(1_791_460_800_000));
        assert_eq!(s.volume, None, "GSMTC has no volume");
        assert_eq!(
            s.can,
            vec![
                MediaAction::Play,
                MediaAction::Pause,
                MediaAction::PlayPause,
                MediaAction::Next,
                MediaAction::Previous,
                MediaAction::Seek,
            ]
        );
    }

    #[test]
    fn no_timeline_means_no_position() {
        let mut sample = playing_at(0, 0);
        sample.end_ticks = 0;
        let s = sample.to_state("x", "", 5);
        assert_eq!(s.duration_ms, None);
        assert_eq!(s.position_ms, None);
        assert_eq!(s.position_at_ms, None);
        assert_eq!(s.name, None);
    }

    #[test]
    fn missing_sample_time_uses_now() {
        let mut sample = playing_at(10, 0);
        sample.updated_ticks = 0;
        assert_eq!(sample.to_state("x", "X", 42).position_at_ms, Some(42));
    }

    #[test]
    fn timeline_refresh_on_track_is_not_news() {
        let a = playing_at(10, 100);
        assert!(!a.differs_from(&playing_at(15, 105)), "on schedule");
        assert!(!a.differs_from(&playing_at(16, 105)), "within drift");
        assert!(a.differs_from(&playing_at(60, 105)), "a seek");
        let mut paused = a.clone();
        paused.status = Status::NotPlaying;
        assert!(a.differs_from(&paused));
        let mut later = paused.clone();
        later.updated_ticks += 30 * SEC;
        assert!(
            !paused.differs_from(&later),
            "a paused position does not move"
        );
        let mut renamed = a.clone();
        renamed.title = "Other".into();
        assert!(a.differs_from(&renamed));
    }

    #[test]
    fn controls_map_to_can() {
        let toggle_only = Controls {
            toggle: true,
            ..Controls::default()
        };
        assert_eq!(toggle_only.actions(), vec![MediaAction::PlayPause]);
        assert_eq!(Controls::default().actions(), Vec::<MediaAction>::new());
        let stop = Controls {
            stop: true,
            ..Controls::default()
        };
        assert_eq!(stop.actions(), vec![MediaAction::Stop]);
    }

    #[test]
    fn display_names() {
        assert_eq!(display_name("Spotify.exe"), "Spotify");
        assert_eq!(display_name("C:\\Apps\\vlc.EXE"), "vlc");
        assert_eq!(
            display_name("SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify"),
            "Spotify"
        );
        assert_eq!(
            display_name("Microsoft.ZuneMusic_8wekyb3d8bbwe!Microsoft.ZuneMusic"),
            "ZuneMusic"
        );
        assert_eq!(display_name("Contoso.Player_abc123!App"), "Player");
        assert_eq!(display_name("308046B0AF4A39CB"), "308046B0AF4A39CB");
        assert_eq!(display_name("Chrome"), "Chrome");
    }
}
