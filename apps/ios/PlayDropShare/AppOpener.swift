import UIKit

// Opens the PlayDrop app from the share extension. iOS gives share extensions no supported
// way to open their containing app: `NSExtensionContext.open(_:completionHandler:)` is only
// honoured for Today widgets and reports `false` here. So after trying it, this falls back to
// the widely used workaround of walking the responder chain to the `UIApplication` object and
// sending it an open-URL message through the Objective-C runtime (`UIApplication.shared` and
// the open methods are unavailable to extension code at compile time). Apple has changed this
// behaviour before (iOS 18 made the old `openURL:` a no-op), so it may stop working in a
// later release. The share sheet always also offers "Done": the batch is already committed,
// and opening PlayDrop by hand drains it. The URL carries no parameters (SR-30).

/// Opens `playdrop://` from the extension; see the file comment.
@MainActor
enum AppOpener {
    /// Tries the supported call, then the responder-chain workaround. `completion` receives
    /// whether some route accepted the URL (not a guarantee that the app came forward).
    static func open(
        _ url: URL, from responder: UIResponder, context: NSExtensionContext?,
        completion: @escaping @MainActor @Sendable (Bool) -> Void
    ) {
        // Both closures are main-actor isolated and marked Sendable (Swift 5 mode does not
        // infer that), so they may cross into the system's callback and back without the
        // responder ever leaving the main actor.
        let fallback: @MainActor @Sendable () -> Bool = { [weak responder] in
            guard let responder else { return false }
            return openThroughResponderChain(url, from: responder)
        }
        guard let context else {
            completion(fallback())
            return
        }
        context.open(url) { @Sendable opened in
            Task { @MainActor in
                completion(opened || fallback())
            }
        }
    }

    /// The responder-chain workaround. Prefers the current three-argument open message and
    /// falls back to the old one-argument `openURL:` on systems that still honour it.
    private static func openThroughResponderChain(_ url: URL, from start: UIResponder) -> Bool {
        let modern = NSSelectorFromString("openURL:options:completionHandler:")
        let legacy = NSSelectorFromString("openURL:")
        var responder: UIResponder? = start
        while let current = responder {
            if current is UIApplication {
                if current.responds(to: modern), let method = current.method(for: modern) {
                    typealias OpenFunction = @convention(c) (
                        AnyObject, Selector, NSURL, NSDictionary, (@convention(block) (Bool) -> Void)?
                    ) -> Void
                    let open = unsafeBitCast(method, to: OpenFunction.self)
                    open(current, modern, url as NSURL, NSDictionary(), nil)
                    return true
                }
                if current.responds(to: legacy) {
                    _ = current.perform(legacy, with: url)
                    return true
                }
                return false
            }
            responder = current.next
        }
        return false
    }
}
