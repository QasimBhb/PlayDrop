import Foundation
import Observation
import PlayDropCore
import PlayDropShared

// State and sending of the remote input screen (protocol.md 5.12). The view reports touches,
// key taps and presenter buttons; this model turns them into `InputEvent`s through the pure
// rules in PlayDropShared (`TrackpadGestures`, `KeyboardComposer`) and sends them in order
// through `InputSender`, only while the session is open.
//
// The session follows `InputSessionController`: start when the screen appears, stop when it
// goes away, when the app goes to the background (the core stops there) and when the peer
// goes offline; start again on the way back. A refusal from the computer (remote input off
// for this phone there) sets `refused`, and the computer ending the session itself (lock,
// 10 idle minutes, its Stop button) sets `endedReason`; the view then says so and closes,
// and nothing restarts until the user opens the screen again. The model
// watches the core's events for `peerOnline`, `peerOffline` and `peerRefused`, and the scene
// hooks reach the open screen through `RemoteInputModel.current`.

/// One remote input screen; see the file comment.
@MainActor
@Observable
final class RemoteInputModel {
    /// Hex Device ID of the computer.
    let peerId: String

    /// True once the session start was sent and until it stops; input is sent only then.
    private(set) var sessionReady = false
    /// The computer refused remote input from this phone; the view shows why and closes.
    private(set) var refused = false
    /// The computer ended the session itself, in its words; the view shows it and closes.
    private(set) var endedReason: String?
    /// Modifier chips latched for the next key.
    private(set) var latched: [KeyModifier] = []
    /// The presenter's laser pointer is on.
    private(set) var laserOn = false
    /// The latest failure worth showing, posted by the view to the message center.
    private(set) var failure: UiMessage?

    /// The screen on display, for the scene hooks. At most one exists at a time.
    private(set) static weak var current: RemoteInputModel?

    @ObservationIgnored private let core: CoreHost
    @ObservationIgnored private var sender: InputSender!
    @ObservationIgnored private var controller: InputSessionController!
    @ObservationIgnored private var gestures: TrackpadGestures
    @ObservationIgnored private var keyboard = KeyboardComposer()
    @ObservationIgnored private var watcher: Task<Void, Never>?
    /// The stop of the previous appearance, still on its way.
    @ObservationIgnored private var closing: Task<Void, Never>?

    /// A model for `peerId` with the trackpad at `sensitivity`, over `core` (by default the
    /// shared host).
    // Defaults are nil rather than `.shared`: a default argument is evaluated outside the
    // main actor, where the main-actor `shared` instance cannot be read.
    init(peerId: String, sensitivity: Double, core: CoreHost? = nil) {
        self.peerId = peerId
        self.core = core ?? .shared
        gestures = TrackpadGestures(sensitivity: sensitivity)
        makePipeline()
    }

    /// A fresh frame queue and session controller (each appearance gets its own session).
    private func makePipeline() {
        let core = self.core
        let sender = InputSender(
            peer: peerId,
            transport: { peer, event in try await core.sendInput(peer: peer, event) },
            onError: { [weak self] error in self?.inputFailed(error) }
        )
        self.sender = sender
        controller = InputSessionController(
            send: { start in try await sender.send(.session(start: start)) },
            isRefusal: { RemoteInputModel.isRefusal($0) }
        )
    }

    // MARK: - Session

    /// The screen appeared (again, after another tab was shown): watch events and open a
    /// session. Waits for the previous appearance's stop, so the computer never sees the new
    /// start before the old stop.
    func appear() async {
        Self.current = self
        await closing?.value
        closing = nil
        guard !refused, endedReason == nil else { return }
        if controller.state == .closed { makePipeline() }
        watchEvents()
        await opened(controller.open())
    }

    /// The screen went away: stop the session (in the background; `appear` waits for it).
    func disappear() {
        if Self.current === self { Self.current = nil }
        watcher?.cancel()
        watcher = nil
        releaseHeldButton()
        sessionReady = false
        let controller = self.controller!
        let sender = self.sender!
        let previous = closing
        closing = Task {
            await previous?.value
            await controller.close()
            await controller.settle()
            sender.finish()
        }
    }

    /// The app is going to the background: stop the open screen's session before the core
    /// stops. Called by `AppServices` before `CoreHost.stop()`.
    static func sceneDidEnterBackground() async {
        await current?.suspend()
    }

    /// The app is back in front and the core runs again: start the session again.
    static func sceneDidBecomeActive() async {
        await current?.resume()
    }

    /// The trackpad sensitivity changed in the options menu.
    func setSensitivity(_ value: Double) {
        gestures.sensitivity = TrackpadGestures.clampSensitivity(value)
    }

    private func suspend() async {
        releaseHeldButton()
        sessionReady = false
        await controller.suspend()
    }

    private func resume() async {
        await opened(controller.resume())
    }

    private func opened(_ outcome: InputSessionOutcome) {
        switch outcome {
        case .sent:
            sessionReady = controller.isActive
        case .refused:
            markRefused()
        case .failed, .skipped:
            break
        }
    }

    private func markRefused() {
        controller.refused()
        sessionReady = false
        refused = true
    }

    private func markEnded(_ reason: String) {
        controller.endedByPeer()
        sessionReady = false
        // The desktop already released held buttons; only forget the local drag.
        _ = gestures.cancelled()
        endedReason = reason
    }

    private func watchEvents() {
        guard watcher == nil else { return }
        let stream = core.events()
        let peer = peerId
        watcher = Task { [weak self] in
            for await event in stream {
                guard let self else { return }
                switch event {
                case let .peerOnline(p, _, _, _) where p == peer:
                    await self.resume()
                case let .peerOffline(p, _) where p == peer:
                    await self.suspend()
                case let .peerRefused(p, frameType, code, _)
                    where p == peer && Self.isInputRefusal(frameType: frameType, code: code):
                    self.markRefused()
                case let .inputSessionEnded(p, reason) where p == peer:
                    self.markEnded(reason)
                default:
                    break
                }
            }
        }
    }

    /// True for a `peerRefused` event about our input frames.
    nonisolated static func isInputRefusal(frameType: String?, code: String) -> Bool {
        frameType == "input" && code == "forbidden"
    }

    /// True when a `sendInput` failure means the peer refuses this kind of input.
    nonisolated static func isRefusal(_ error: Error) -> Bool {
        if let e = error as? PlayDropError, case .Forbidden = e { return true }
        return false
    }

    private func inputFailed(_ error: Error) {
        if Self.isRefusal(error) {
            markRefused()
            return
        }
        if let e = error as? PlayDropError, case .NotFound = e {
            // The session to the peer dropped; `peerOffline` follows and suspends.
            sessionReady = false
            return
        }
        failure = UiMessage.forError(error)
    }

    /// Clears `failure` once the view showed it.
    func failureShown() { failure = nil }

    // MARK: - Trackpad

    /// Fingers are down: `fingers` of them, centred on `point` (in points), at `time`
    /// seconds. Returns what was sent, so the view can play haptics for clicks.
    @discardableResult
    func touches(_ fingers: Int, at point: TrackpadPoint, time: TimeInterval) -> [TrackpadOutput] {
        send(gestures.touches(fingers, at: point, time: time))
    }

    /// Every finger lifted.
    @discardableResult
    func touchesEnded(time: TimeInterval) -> [TrackpadOutput] {
        send(gestures.ended(time: time))
    }

    /// The system cancelled the touches.
    @discardableResult
    func touchesCancelled() -> [TrackpadOutput] {
        send(gestures.cancelled())
    }

    /// The long-press timer fired.
    @discardableResult
    func tick(time: TimeInterval) -> [TrackpadOutput] {
        send(gestures.tick(time: time))
    }

    /// A button under the pad was tapped.
    func click(_ button: TrackpadButton) {
        _ = send([.button(button, .click)])
    }

    private func releaseHeldButton() {
        let outputs = gestures.cancelled()
        if sessionReady { _ = send(outputs) }
    }

    private func send(_ outputs: [TrackpadOutput]) -> [TrackpadOutput] {
        guard sessionReady else { return [] }
        for output in outputs { sender.post(InputMapping.event(for: output)) }
        return outputs
    }

    // MARK: - Keyboard

    /// The typing field changed to `text`; the view resets it to `KeyboardComposer.fieldReset`.
    func fieldChanged(_ text: String) {
        let outputs = keyboard.fieldChanged(to: text)
        latched = keyboard.latched
        post(outputs)
    }

    /// A special key was tapped (with the latched modifiers).
    func press(_ key: String) {
        let output = keyboard.press(key)
        latched = keyboard.latched
        post([output])
    }

    /// A media key was tapped; modifiers stay latched.
    func mediaKey(_ key: String) {
        post([.key(key, mods: [])])
    }

    /// A modifier chip was tapped.
    func toggle(_ mod: KeyModifier) {
        keyboard.toggle(mod)
        latched = keyboard.latched
    }

    private func post(_ outputs: [KeyboardOutput]) {
        guard sessionReady else { return }
        for output in outputs { sender.post(InputMapping.event(for: output)) }
    }

    // MARK: - Presenter

    /// A presenter button was tapped.
    func presenter(_ action: PresenterAction) {
        guard sessionReady else { return }
        sender.post(.presenter(action: action))
    }

    /// Turns the laser pointer on or off.
    func toggleLaser() {
        guard sessionReady else { return }
        laserOn.toggle()
        sender.post(.presenter(action: laserOn ? .pointerOn : .pointerOff))
    }
}

/// Maps the PlayDropShared outputs to core frames.
enum InputMapping {
    /// The frame for a trackpad output. Scrolls are in lines.
    static func event(for output: TrackpadOutput) -> InputEvent {
        switch output {
        case let .move(dx, dy):
            return .pointerMove(dx: dx, dy: dy)
        case let .button(button, action):
            return .pointerButton(button: pointerButton(button), action: buttonAction(action))
        case let .scroll(dx, dy):
            return .scroll(dx: dx, dy: dy, pixels: false)
        }
    }

    /// The frame for a keyboard output: text, or a key press with modifiers.
    static func event(for output: KeyboardOutput) -> InputEvent {
        switch output {
        case let .text(text):
            return .text(text: text)
        case let .key(key, mods):
            return .key(key: key, mods: mods.map(modifier), action: .press)
        }
    }

    static func pointerButton(_ b: TrackpadButton) -> PointerButton {
        switch b {
        case .left: return .left
        case .middle: return .middle
        case .right: return .right
        }
    }

    static func buttonAction(_ a: TrackpadButtonAction) -> ButtonAction {
        switch a {
        case .down: return .down
        case .up: return .up
        case .click: return .click
        }
    }

    static func modifier(_ m: KeyModifier) -> Modifier {
        switch m {
        case .ctrl: return .ctrl
        case .alt: return .alt
        case .shift: return .shift
        case .meta: return .meta
        }
    }
}
