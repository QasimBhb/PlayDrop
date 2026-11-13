import PlayDropCore
import PlayDropShared
import SwiftUI

// The "Now playing" section of the Device screen: one block per media player the computer
// reports (protocol.md 5.9) with the title, the artist, a progress bar extrapolated once a
// second while playing, and the controls the player allows. Commands go out through
// `CoreHost.sendMediaCommand`; the computer's next state frame updates the row.
//
// The computer's master volume arrives in the same list as the reserved player
// `SystemVolume.playerId`. It is never a player block: it gets its own "Computer volume"
// slider above the players, shown even when nothing plays.

/// The computer volume slider and one section of players; nothing when the peer reports
/// neither.
struct NowPlayingSection: View {
    /// Hex Device ID.
    let peerId: String
    /// The peer's players, the system volume entry included.
    let players: [MediaState]
    /// The peer accepts media commands (online and `media.control`).
    let controllable: Bool

    @Environment(CoreHost.self) private var core
    @Environment(MessageCenter.self) private var messages

    /// Keeps a burst of refused taps down to one message (every button answers on its own).
    @State private var throttle = RefusalThrottle()

    var body: some View {
        let split = SystemVolume.split(players) { $0.player }
        if let system = split.system {
            Section {
                SystemVolumeRow(
                    volume: system.volume,
                    controllable: controllable && system.can.contains(.volume)
                ) { value in
                    command(system.player, .volume, value)
                }
            }
            .task { await watchRefusals() }
        }
        if !split.players.isEmpty {
            // Watches refusals only when the volume section above does not.
            let watches = split.system == nil
            Section(L10n.Media.title) {
                ForEach(split.players, id: \.player) { state in
                    PlayerBlock(state: state, row: MediaRows.row(state, controllable: controllable)) { action, value in
                        command(state.player, action, value)
                    }
                }
            }
            .task(id: watches) {
                if watches { await watchRefusals() }
            }
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

/// The computer's master volume: a labelled slider with the current percentage. While
/// the finger drags, commands go out at most 5 times a second and the slider ignores
/// incoming states; after the release it keeps the released value until the computer
/// echoes it (`VolumeSliderModel`). Disabled when the computer accepts no media commands,
/// like the player controls.
private struct SystemVolumeRow: View {
    /// The reported level, 0 to 100.
    let volume: UInt8?
    /// The computer accepts the command.
    let controllable: Bool
    let send: (UInt64) -> Void

    @State private var model = VolumeSliderModel()
    @State private var flushTask: Task<Void, Never>?
    @State private var expireTask: Task<Void, Never>?

    var body: some View {
        let shown = model.shown
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Label(L10n.Media.computerVolume, systemImage: "hifispeaker")
                Spacer()
                Text(shown, format: .percent)
                    .monospacedDigit()
                    .foregroundStyle(.secondary)
            }
            .accessibilityHidden(true)
            HStack {
                Image(systemName: shown == 0 ? "speaker.slash.fill" : "speaker.fill")
                    .foregroundStyle(.secondary)
                    .accessibilityHidden(true)
                Slider(
                    value: Binding(get: { Double(shown) }, set: { changed(to: $0) }),
                    in: 0...100,
                    step: 1
                ) { editing in
                    if editing { model.begin() } else { released() }
                }
                .disabled(!controllable)
                .accessibilityLabel(Text(L10n.Media.computerVolume))
                .accessibilityValue(Text(shown, format: .percent))
                .accessibilityHint(Text(L10n.Media.computerVolumeHint))
                Image(systemName: "speaker.wave.3.fill")
                    .foregroundStyle(.secondary)
                    .accessibilityHidden(true)
            }
        }
        .padding(.vertical, 4)
        .onChange(of: volume, initial: true) { _, new in
            model.report(new.map { Int($0) })
        }
        .onDisappear {
            flushTask?.cancel()
            expireTask?.cancel()
        }
    }

    private static var now: TimeInterval { ProcessInfo.processInfo.systemUptime }

    /// A new slider value: a throttled send while dragging, else one send (an
    /// accessibility increment).
    private func changed(to value: Double) {
        if model.dragging {
            if let v = model.drag(to: value, now: Self.now) { send(UInt64(v)) }
            scheduleFlush()
        } else {
            send(UInt64(model.set(to: value, now: Self.now)))
            scheduleExpire()
        }
    }

    private func released() {
        flushTask?.cancel()
        flushTask = nil
        send(UInt64(model.release(now: Self.now)))
        scheduleExpire()
    }

    /// Sends a value the throttle held back once its turn comes, so the computer reaches
    /// the finger's level even when the finger stops moving.
    private func scheduleFlush() {
        guard let delay = model.flushDelay(now: Self.now) else { return }
        flushTask?.cancel()
        flushTask = Task {
            try? await Task.sleep(for: .seconds(delay))
            guard !Task.isCancelled else { return }
            if let v = model.flush(now: Self.now) {
                send(UInt64(v))
            } else {
                scheduleFlush()
            }
        }
    }

    /// Lets the slider follow the computer again when no echo of the released value comes.
    private func scheduleExpire() {
        guard model.awaitingEcho else { return }
        expireTask?.cancel()
        let wait = model.echoWait
        expireTask = Task {
            // A little past the wait, so the check below never lands a hair early.
            try? await Task.sleep(for: .seconds(wait + 0.05))
            guard !Task.isCancelled else { return }
            model.expire(now: Self.now)
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
