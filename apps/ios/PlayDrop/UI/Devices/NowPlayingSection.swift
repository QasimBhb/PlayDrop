import PlayDropCore
import PlayDropShared
import SwiftUI

// The "Now playing" section of the Device screen: one block per media player the computer
// reports (protocol.md 5.9) with the title, the artist, a progress bar extrapolated once a
// second while playing, and the controls the player allows. Commands go out through
// `CoreHost.sendMediaCommand`; the computer's next state frame updates the row.

/// One section per peer with players; nothing when it reports none.
struct NowPlayingSection: View {
    /// Hex Device ID.
    let peerId: String
    /// The peer's players.
    let players: [MediaState]
    /// The peer accepts media commands (online and `media.control`).
    let controllable: Bool

    @Environment(CoreHost.self) private var core
    @Environment(MessageCenter.self) private var messages

    /// Keeps a burst of refused taps down to one message (every button answers on its own).
    @State private var throttle = RefusalThrottle()

    var body: some View {
        if !players.isEmpty {
            Section(L10n.Media.title) {
                ForEach(players, id: \.player) { state in
                    PlayerBlock(state: state, row: MediaRows.row(state, controllable: controllable)) { action, value in
                        command(state.player, action, value)
                    }
                }
            }
            .task { await watchRefusals() }
        }
    }

    /// Shows the "Media is off" message when the computer refuses a media command. The
    /// refusal arrives as a `peerRefused` event for a `media` frame (protocol.md 5.3).
    private func watchRefusals() async {
        for await event in core.events() {
            if case let .peerRefused(p, frameType, code, _) = event, p == peerId,
               PeerRefusals.feature(frameType: frameType, code: code) == .media {
                showRefused()
            }
        }
    }

    private func showRefused() {
        let now = ProcessInfo.processInfo.systemUptime
        guard throttle.shouldShow(peer: peerId, feature: .media, now: now) else { return }
        messages.show(.error(L10n.mediaRefused(core.labelOf(peer: peerId))))
    }

    private func command(_ player: String, _ action: MediaAction, _ value: UInt64?) {
        let peer = peerId
        Task {
            do {
                try await core.sendMediaCommand(peer: peer, player: player, action: action, value: value)
            } catch let e as PlayDropError {
                if case .Forbidden = e { showRefused() } else { messages.show(error: e) }
            } catch {
                messages.show(error: error)
            }
        }
    }
}

/// One player.
private struct PlayerBlock: View {
    let state: MediaState
    let row: MediaRow
    let send: (MediaAction, UInt64?) -> Void

    @State private var volume: Double = 0
    @State private var editingVolume = false

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(verbatim: row.name).font(.caption).foregroundStyle(.secondary)
            Text(verbatim: row.title ?? L10n.Media.unknownTitle).font(.headline).lineLimit(2)
            if let artist = row.artist {
                Text(verbatim: artist).font(.subheadline).foregroundStyle(.secondary).lineLimit(1)
            }
            if let duration = row.durationMs, duration > 0 {
                TimelineView(.periodic(from: .now, by: 1)) { context in
                    progress(duration: duration, now: context.date)
                }
            }
            controls
            if row.canVolume {
                HStack {
                    Image(systemName: "speaker.fill").foregroundStyle(.secondary)
                    Slider(value: $volume, in: 0...100, step: 1) { editing in
                        editingVolume = editing
                        if !editing { send(.volume, UInt64(volume.rounded())) }
                    }
                    .accessibilityLabel(Text(L10n.Media.volume))
                    Image(systemName: "speaker.wave.3.fill").foregroundStyle(.secondary)
                }
            }
        }
        .padding(.vertical, 4)
        .onAppear { volume = Double(row.volume ?? 0) }
        .onChange(of: row.volume) { _, new in
            if !editingVolume { volume = Double(new ?? 0) }
        }
    }

    private func progress(duration: UInt64, now: Date) -> some View {
        let position = MediaRows.position(state, now: now) ?? 0
        let fraction = MediaPlayback.fraction(positionMs: position, durationMs: duration) ?? 0
        return VStack(spacing: 2) {
            GeometryReader { geo in
                ZStack(alignment: .leading) {
                    Capsule().fill(Color.secondary.opacity(0.25))
                    Capsule().fill(Color.accentColor).frame(width: geo.size.width * fraction)
                }
                .contentShape(Rectangle())
                .gesture(
                    DragGesture(minimumDistance: 0).onEnded { value in
                        let at = value.location.x / max(geo.size.width, 1)
                        send(.seek, MediaPlayback.seekTarget(fraction: at, durationMs: duration))
                    },
                    including: row.canSeek ? .all : .subviews
                )
            }
            .frame(height: 6)
            .accessibilityElement()
            .accessibilityLabel(Text(L10n.Media.position))
            .accessibilityValue(Text(verbatim: MediaPlayback.clock(position)))
            HStack {
                Text(verbatim: MediaPlayback.clock(position))
                Spacer()
                Text(verbatim: MediaPlayback.clock(duration))
            }
            .font(.caption2.monospacedDigit())
            .foregroundStyle(.secondary)
        }
    }

    @ViewBuilder
    private var controls: some View {
        if row.canPrevious || row.canNext || row.toggleAction != nil {
            HStack(spacing: 32) {
                Spacer()
                if row.canPrevious {
                    Button { send(.previous, nil) } label: { Image(systemName: "backward.fill") }
                        .accessibilityLabel(Text(L10n.Media.previous))
                }
                if let toggle = row.toggleAction {
                    Button { send(toggle, nil) } label: {
                        Image(systemName: row.playing ? "pause.fill" : "play.fill").font(.title2)
                    }
                    .accessibilityLabel(Text(row.playing ? L10n.Media.pause : L10n.Media.play))
                }
                if row.canNext {
                    Button { send(.next, nil) } label: { Image(systemName: "forward.fill") }
                        .accessibilityLabel(Text(L10n.Media.next))
                }
                Spacer()
            }
            // Several buttons in one Form row: without this, a tap anywhere fires them all.
            .buttonStyle(.borderless)
        }
    }
}
