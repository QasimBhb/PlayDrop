import PlayDropShared
import SwiftUI
import UIKit

// The trackpad tab: a full-width touch surface and Left, Middle, Right buttons under it.
// The surface is a UIKit view because SwiftUI gestures do not report how many fingers are
// down; it passes the finger count, their centre and the touch time to
// `RemoteInputModel`, which applies `TrackpadGestures`. A timer started on the first finger
// drives the long-press drag.

/// The trackpad tab.
struct TrackpadPane: View {
    let model: RemoteInputModel

    var body: some View {
        VStack(spacing: 12) {
            TouchSurface(model: model)
                .background(RoundedRectangle(cornerRadius: 16).fill(Color(uiColor: .secondarySystemBackground)))
                .overlay(RoundedRectangle(cornerRadius: 16).stroke(Color.secondary.opacity(0.3)))
                .clipShape(RoundedRectangle(cornerRadius: 16))
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .accessibilityLabel(Text(L10n.Input.trackpad))
            Text(L10n.Input.trackpadHint)
                .font(.footnote)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
            HStack(spacing: 12) {
                button(L10n.Input.left, .left)
                button(L10n.Input.middle, .middle)
                button(L10n.Input.right, .right)
            }
            .frame(height: 56)
        }
    }

    private func button(_ title: String, _ which: TrackpadButton) -> some View {
        Button {
            model.click(which)
            InputHaptics.click()
        } label: {
            Text(title).frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        .buttonStyle(.bordered)
    }
}

/// Hosts `TouchSurfaceView`.
private struct TouchSurface: UIViewRepresentable {
    let model: RemoteInputModel

    func makeUIView(context: Context) -> TouchSurfaceView {
        let view = TouchSurfaceView()
        view.model = model
        return view
    }

    func updateUIView(_ view: TouchSurfaceView, context: Context) {
        view.model = model
    }
}

/// Reports finger count, centre and time on every touch change.
final class TouchSurfaceView: UIView {
    weak var model: RemoteInputModel?

    private var down: Set<UITouch> = []
    private var longPressTimer: Timer?

    override init(frame: CGRect) {
        super.init(frame: frame)
        isMultipleTouchEnabled = true
        backgroundColor = .clear
    }

    required init?(coder: NSCoder) {
        super.init(coder: coder)
        isMultipleTouchEnabled = true
    }

    override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent?) {
        let wasIdle = down.isEmpty
        down.formUnion(touches)
        report(time: event?.timestamp)
        if wasIdle && down.count == 1 { startLongPressTimer() } else { stopLongPressTimer() }
    }

    override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent?) {
        report(time: event?.timestamp)
    }

    override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent?) {
        down.subtract(touches)
        if down.isEmpty {
            stopLongPressTimer()
            let outputs = model?.touchesEnded(time: event?.timestamp ?? ProcessInfo.processInfo.systemUptime) ?? []
            InputHaptics.play(for: outputs)
        } else {
            report(time: event?.timestamp)
        }
    }

    override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent?) {
        down.subtract(touches)
        if down.isEmpty {
            stopLongPressTimer()
            model?.touchesCancelled()
        }
    }

    private func report(time: TimeInterval?) {
        guard !down.isEmpty else { return }
        var x = 0.0, y = 0.0
        for touch in down {
            let p = touch.location(in: self)
            x += p.x
            y += p.y
        }
        let n = Double(down.count)
        let outputs = model?.touches(
            down.count, at: TrackpadPoint(x: x / n, y: y / n),
            time: time ?? ProcessInfo.processInfo.systemUptime
        ) ?? []
        InputHaptics.play(for: outputs)
    }

    private func startLongPressTimer() {
        stopLongPressTimer()
        // `UIEvent.timestamp` and `systemUptime` share one clock, so the tick compares with
        // the touch times the model saw.
        longPressTimer = Timer.scheduledTimer(withTimeInterval: 0.52, repeats: false) { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self else { return }
                let outputs = self.model?.tick(time: ProcessInfo.processInfo.systemUptime) ?? []
                InputHaptics.play(for: outputs)
            }
        }
    }

    private func stopLongPressTimer() {
        longPressTimer?.invalidate()
        longPressTimer = nil
    }
}
