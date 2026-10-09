import Foundation
import PlayDropCore
import UIKit
import UniformTypeIdentifiers

// Puts clipboard text and images received from peers on `UIPasteboard.general`. Port of
// Android's `service/ClipboardApplier.kt`.
//
// - Only while the app is active and `AppSettings.applyIncomingClipboard` is on. The core
//   runs only in the foreground anyway; an inactive app (Control Center pulled down, app
//   switcher) skips the clip rather than writing behind the user's back.
// - A clip the sender marked sensitive (SR-24) is written local-only (never handed to
//   Universal Clipboard) and expires after 60 s.
// - Applied text is remembered in `ClipboardEchoState`, so the "send clipboard" action does
//   not bounce it straight back.
// - Clip contents are never logged or shown in a notification (SR-32).

/// Applies incoming clips; see the file comment.
@MainActor
enum ClipboardApplier {
    /// How long a sensitive clip stays on the pasteboard.
    static let sensitiveLifetime: TimeInterval = 60

    private static var started = false
    private static var eventTask: Task<Void, Never>?

    /// Subscribes to host events. Idempotent; called once by `AppServices.start()`.
    static func start() {
        guard !started else {
            return
        }
        started = true
        let events = CoreHost.shared.events()
        eventTask = Task { @MainActor in
            for await event in events {
                switch event {
                case let .clipboard(_, text, sensitive):
                    applyText(text, sensitive: sensitive)
                case let .clipboardImage(_, mime, data, sensitive):
                    applyImage(data, mime: mime, sensitive: sensitive)
                default:
                    break
                }
            }
        }
    }

    /// True when an incoming clip may be written now.
    private static var mayApply: Bool {
        AppSettings.shared.applyIncomingClipboard && UIApplication.shared.applicationState == .active
    }

    /// Writes `text` to the pasteboard and remembers it for the echo rule.
    static func applyText(_ text: String, sensitive: Bool) {
        guard mayApply else {
            Log.clipboard.info("incoming text skipped: app not active or applying is off")
            return
        }
        let pasteboard = UIPasteboard.general
        if sensitive {
            pasteboard.setItems([[UTType.utf8PlainText.identifier: text]], options: sensitiveOptions())
        } else {
            pasteboard.string = text
        }
        ClipboardEchoState.echo.remember(appliedText: text)
        Log.clipboard.info("applied incoming text (sensitive: \(sensitive, privacy: .public))")
    }

    /// Writes an image to the pasteboard when `data` decodes as one.
    static func applyImage(_ data: Data, mime: String, sensitive: Bool) {
        guard mayApply else {
            Log.clipboard.info("incoming image skipped: app not active or applying is off")
            return
        }
        guard let image = UIImage(data: data) else {
            Log.clipboard.error("incoming image skipped: not decodable (\(mime, privacy: .public))")
            return
        }
        let pasteboard = UIPasteboard.general
        if sensitive {
            // Keep the sender's encoding when iOS knows the type; otherwise hand over the
            // decoded image, which UIKit stores as PNG.
            let item: [String: Any]
            if let type = UTType(mimeType: mime), type.conforms(to: .image) {
                item = [type.identifier: data]
            } else {
                item = [UTType.png.identifier: image.pngData() ?? data]
            }
            pasteboard.setItems([item], options: sensitiveOptions())
        } else {
            pasteboard.image = image
        }
        Log.clipboard.info("applied incoming image (sensitive: \(sensitive, privacy: .public))")
    }

    /// Local-only and expiring after `sensitiveLifetime` (SR-24).
    private static func sensitiveOptions() -> [UIPasteboard.OptionsKey: Any] {
        [
            .localOnly: true,
            .expirationDate: Date().addingTimeInterval(sensitiveLifetime),
        ]
    }
}
