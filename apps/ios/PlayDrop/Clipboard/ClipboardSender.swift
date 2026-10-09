import Foundation
import PlayDropCore
import UIKit
import UniformTypeIdentifiers

// Sends what is on this iPhone's pasteboard to peers: the "Send clipboard" button and the
// Shortcut action. iOS lets an app read the pasteboard only in the foreground and shows its
// paste notice when it does, so this runs only on an explicit user action, never on a
// timer (anti-pattern 18: no silent clipboard sync that iOS would block).
//
// - Text first; else an image, sent as PNG (or the pasteboard's own PNG or JPEG bytes when it
//   has them), at most 16 MiB (`TooLarge` beyond that).
// - The echo rule: text that `ClipboardApplier` put there from a peer within the echo window
//   is not sent back (the result reports `skippedEcho`).
// - With a peer: text goes to that peer through `sendClipboard`. Images have no per-peer
//   call in the core API, so an image always goes to every eligible peer through
//   `pushClipboardImage`. Without a peer: `pushClipboard` to every eligible peer.
// - Never marked sensitive: iOS gives no way to read the source app's sensitivity.
// - Content is never logged (SR-32).

/// Pushes the current pasteboard; see the file comment.
@MainActor
enum ClipboardSender {
    /// Largest image sent, in bytes; the core enforces the same limit.
    static let maxImageBytes = 16 * 1024 * 1024

    /// Sends the pasteboard to `peer` (hex Device ID), or to every eligible peer when nil.
    /// Throws `PlayDropError.NotFound` when the pasteboard holds neither text nor an image,
    /// `PlayDropError.TooLarge` for an image over 16 MiB, `CoreHostError.notRunning` when the
    /// core is down, and whatever the core reports.
    static func pushCurrent(to peer: String?) async throws -> ClipboardPush {
        let pasteboard = UIPasteboard.general
        if pasteboard.hasStrings, let text = pasteboard.string, !text.isEmpty {
            return try await pushText(text, to: peer)
        }
        if pasteboard.hasImages, let payload = imagePayload(from: pasteboard) {
            let data = payload.data
            let mime = payload.mime
            guard data.count <= maxImageBytes else {
                throw PlayDropError.TooLarge(detail: String(localized: "The copied image is larger than 16 MiB."))
            }
            return try await CoreHost.shared.withHost {
                try $0.pushClipboardImage(data: data, mime: mime, sensitive: false)
            }
        }
        throw PlayDropError.NotFound(detail: String(localized: "Nothing to send: the clipboard is empty."))
    }

    private static func pushText(_ text: String, to peer: String?) async throws -> ClipboardPush {
        if ClipboardEchoState.echo.isEcho(text) {
            return ClipboardPush(
                sentTo: [], skippedEcho: true, skippedOffline: 0, skippedNoGrant: 0, skippedUnsupported: 0
            )
        }
        guard let peer else {
            return try await CoreHost.shared.withHost { try $0.pushClipboard(text: text, sensitive: false) }
        }
        let sent = try await CoreHost.shared.withHost {
            try $0.sendClipboard(peer: peer, text: text, sensitive: false)
        }
        return ClipboardPush(
            sentTo: sent ? [peer] : [], skippedEcho: !sent, skippedOffline: 0, skippedNoGrant: 0, skippedUnsupported: 0
        )
    }

    /// The pasteboard's image bytes and MIME type: its own PNG or JPEG data when present,
    /// else the image re-encoded as PNG.
    private static func imagePayload(from pasteboard: UIPasteboard) -> (data: Data, mime: String)? {
        if let png = pasteboard.data(forPasteboardType: UTType.png.identifier) {
            return (png, "image/png")
        }
        if let jpeg = pasteboard.data(forPasteboardType: UTType.jpeg.identifier) {
            return (jpeg, "image/jpeg")
        }
        if let image = pasteboard.image, let png = image.pngData() {
            return (png, "image/png")
        }
        return nil
    }
}
