import SwiftUI
import UIKit

// Hides its content from screenshots, screen recordings, AirPlay mirroring and the app
// switcher snapshot. Used for the pairing QR code.
//
// Why (threat model SR-9): the QR code carries a one-time pairing secret. Anyone who gets a
// picture of it within its two minutes can pair as this iPhone's peer, and screenshots end
// up in photo libraries, cloud backups and chat apps. Android sets FLAG_SECURE on the window;
// iOS has no public API for that. The one reliable mechanism is the layer that UIKit draws a
// secure text field's text in (`UITextField.isSecureTextEntry`): the system leaves that layer
// out of every capture. We take that canvas view out of a throwaway secure text field and
// host the SwiftUI content inside it, so the content is drawn on screen but captured as blank.
//
// The canvas is a private subview, so a future iOS could stop providing it. In that case the
// container falls back to showing the content normally (the code must stay scannable); the
// warning text on the QR screen still tells the user not to share it. Nothing here logs or
// copies the content.

/// Shows `content` on screen while keeping it out of screenshots and recordings.
struct SecureContainer<Content: View>: UIViewRepresentable {
    private let content: Content

    /// Wraps `content`.
    init(@ViewBuilder content: () -> Content) {
        self.content = content()
    }

    func makeCoordinator() -> Coordinator {
        Coordinator(hosting: UIHostingController(rootView: content))
    }

    func makeUIView(context: Context) -> UIView {
        let container = UIView()
        container.backgroundColor = .clear
        let hosted = context.coordinator.hosting.view!
        hosted.backgroundColor = .clear
        hosted.translatesAutoresizingMaskIntoConstraints = false

        if let canvas = Self.secureCanvas() {
            canvas.translatesAutoresizingMaskIntoConstraints = false
            container.addSubview(canvas)
            Self.pin(canvas, to: container)
            canvas.addSubview(hosted)
            Self.pin(hosted, to: canvas)
        } else {
            container.addSubview(hosted)
            Self.pin(hosted, to: container)
        }
        return container
    }

    func updateUIView(_ uiView: UIView, context: Context) {
        context.coordinator.hosting.rootView = content
    }

    func sizeThatFits(_ proposal: ProposedViewSize, uiView: UIView, context: Context) -> CGSize? {
        let target = CGSize(width: proposal.width ?? .infinity, height: proposal.height ?? .infinity)
        return context.coordinator.hosting.sizeThatFits(in: target)
    }

    /// Holds the hosting controller across updates.
    final class Coordinator {
        /// Hosts the SwiftUI content inside the secure canvas.
        let hosting: UIHostingController<Content>

        init(hosting: UIHostingController<Content>) {
            self.hosting = hosting
        }
    }

    /// The capture-excluded canvas of a secure text field, emptied and made interactive, or
    /// nil when this iOS does not provide one.
    private static func secureCanvas() -> UIView? {
        let field = UITextField()
        field.isSecureTextEntry = true
        guard let canvas = field.subviews.first else { return nil }
        canvas.subviews.forEach { $0.removeFromSuperview() }
        canvas.removeFromSuperview()
        canvas.isUserInteractionEnabled = true
        canvas.backgroundColor = .clear
        return canvas
    }

    private static func pin(_ view: UIView, to parent: UIView) {
        NSLayoutConstraint.activate([
            view.leadingAnchor.constraint(equalTo: parent.leadingAnchor),
            view.trailingAnchor.constraint(equalTo: parent.trailingAnchor),
            view.topAnchor.constraint(equalTo: parent.topAnchor),
            view.bottomAnchor.constraint(equalTo: parent.bottomAnchor),
        ])
    }
}
