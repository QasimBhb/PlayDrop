import Foundation
import PlayDropCore
import PlayDropShared

// What one "Now playing" row shows and which buttons it offers, derived from the player's
// `MediaState` (protocol.md 5.9). Only actions the player lists in `can` get a control, and
// none at all when the computer does not accept media commands from us (anti-pattern 18).

/// One player row.
struct MediaRow: Identifiable, Equatable {
    /// Player ID.
    let id: String
    /// Display name of the player ("Spotify"), else its ID.
    let name: String
    /// Track title, if any.
    let title: String?
    /// Artist, if any.
    let artist: String?
    /// Playing now.
    let playing: Bool
    /// Track length in ms, if known.
    let durationMs: UInt64?
    /// Volume 0 to 100, if reported.
    let volume: UInt8?
    /// The command for the play/pause button, nil when the player allows neither.
    let toggleAction: MediaAction?
    /// Previous track allowed.
    let canPrevious: Bool
    /// Next track allowed.
    let canNext: Bool
    /// Seeking allowed (and the duration known).
    let canSeek: Bool
    /// Volume changes allowed.
    let canVolume: Bool
}

/// Builds `MediaRow`s; see the file comment.
enum MediaRows {
    /// The row for `state`; `controllable` is false when the computer accepts no commands.
    static func row(_ state: MediaState, controllable: Bool) -> MediaRow {
        let can = controllable ? Set(state.can) : []
        let name = state.name?.trimmingCharacters(in: .whitespacesAndNewlines)
        return MediaRow(
            id: state.player,
            name: (name?.isEmpty ?? true) ? state.player : name!,
            title: nonEmpty(state.title),
            artist: nonEmpty(state.artist),
            playing: state.playing,
            durationMs: state.durationMs,
            volume: state.volume,
            toggleAction: toggleAction(playing: state.playing, can: can),
            canPrevious: can.contains(.previous),
            canNext: can.contains(.next),
            canSeek: can.contains(.seek) && (state.durationMs ?? 0) > 0,
            canVolume: can.contains(.volume)
        )
    }

    /// The play/pause command: the player's own toggle when it has one, else pause while
    /// playing and play while paused.
    static func toggleAction(playing: Bool, can: Set<MediaAction>) -> MediaAction? {
        if can.contains(.playPause) { return .playPause }
        if playing { return can.contains(.pause) ? .pause : nil }
        return can.contains(.play) ? .play : nil
    }

    /// The position of `state` at `now`, extrapolated while playing.
    static func position(_ state: MediaState, now: Date) -> UInt64? {
        MediaPlayback.position(
            positionMs: state.positionMs, positionAtMs: state.positionAtMs,
            durationMs: state.durationMs, playing: state.playing,
            nowMs: UInt64(max(0, now.timeIntervalSince1970 * 1000))
        )
    }

    private static func nonEmpty(_ s: String?) -> String? {
        guard let t = s?.trimmingCharacters(in: .whitespacesAndNewlines), !t.isEmpty else { return nil }
        return t
    }
}
