import Foundation
import Observation
import PlayDropCore
import PlayDropShared
import UserNotifications

// Local notifications for what the core reports: received files, failed transfers, pings,
// pairing results and offers waiting for an answer (with Accept and Decline actions). Port
// of the event half of Android's `service/Notifications.kt`, same wording, except that files
// land in "Files > PlayDrop" instead of "Download/PlayDrop".
//
// Foreground only, by construction: the core runs only while the app is open (PLAN.md
// anti-pattern 11), so every notification is posted while the app is in front and shown as
// a banner through the delegate's `willPresent`. They mainly matter when the user is on
// another tab or the transfer list is scrolled away. When the app goes to the background the
// offer notifications are removed: the core stops, so their buttons could no longer work.
//
// - One identifier per transfer and kind (`offer.<id>`, `received.<id>`, `failed.<id>`) and
//   per peer for pings and pairings, so a repeated event replaces its notification instead
//   of stacking. Progress is never posted.
// - Clipboard content, SAS and QR URIs never appear in a notification (SR-32). Names are
//   the local label, else the Short ID (SR-8).

/// Identifiers of PlayDrop's notification category, actions and payload keys. Not actor
/// isolated, so the notification delegate can read them on any thread.
enum NotificationKeys {
    /// Category of offer notifications.
    static let offerCategory = "org.playdrop.offer"
    /// Action that accepts an offer.
    static let acceptAction = "org.playdrop.offer.accept"
    /// Action that declines an offer.
    static let declineAction = "org.playdrop.offer.decline"
    /// `userInfo` key holding the transfer ID of an offer.
    static let transferIdKey = "transferId"
}

/// Notification permission, categories and the event router; see the file comment.
@MainActor
@Observable
final class LocalNotifications {
    /// The shared instance; `AppServices.start()` starts it.
    static let shared = LocalNotifications()

    /// The last known permission state; refreshed by `refreshAuthorizationStatus()`,
    /// `requestPermission()` and before each post.
    private(set) var authorizationStatus: UNAuthorizationStatus = .notDetermined

    @ObservationIgnored private var started = false
    @ObservationIgnored private var eventTask: Task<Void, Never>?
    @ObservationIgnored private let delegate = NotificationDelegate()
    @ObservationIgnored private var postedOffers: Set<String> = []
    @ObservationIgnored private var warnedDenied = false

    private init() {}

    /// Sets the delegate and categories and subscribes to host events. Idempotent.
    func start() {
        guard !started else {
            return
        }
        started = true
        let center = UNUserNotificationCenter.current()
        center.delegate = delegate
        center.setNotificationCategories([Self.makeOfferCategory()])
        let events = CoreHost.shared.events()
        eventTask = Task { @MainActor [weak self] in
            for await event in events {
                await self?.route(event)
            }
        }
        Task { await refreshAuthorizationStatus() }
    }

    /// Asks for alert, sound and badge permission (once; iOS remembers the answer). True when
    /// notifications may be shown.
    func requestPermission() async -> Bool {
        let granted = await Self.requestAuthorization()
        await refreshAuthorizationStatus()
        return granted
    }

    /// Re-reads the permission state into `authorizationStatus`.
    func refreshAuthorizationStatus() async {
        authorizationStatus = await Self.currentStatus()
    }

    /// Removes offer notifications; called when the app goes to the background and the core
    /// stops, which makes their Accept and Decline buttons useless.
    func appDidEnterBackground() {
        guard !postedOffers.isEmpty else {
            return
        }
        let ids = postedOffers.map(Self.offerIdentifier)
        postedOffers.removeAll()
        UNUserNotificationCenter.current().removeDeliveredNotifications(withIdentifiers: ids)
    }

    /// Answers an offer from a notification action.
    func handleOfferAction(_ actionIdentifier: String, transferId: String) async {
        let accept: Bool
        switch actionIdentifier {
        case NotificationKeys.acceptAction: accept = true
        case NotificationKeys.declineAction: accept = false
        default: return
        }
        postedOffers.remove(transferId)
        do {
            try await CoreHost.shared.respondOffer(transferId: transferId, accept: accept)
        } catch {
            Log.notify.error("answering an offer failed: \(Log.describe(error), privacy: .public)")
            await post(
                id: "offer-answer.\(transferId)",
                title: String(localized: "PlayDrop"),
                body: String(localized: "Could not answer the offer: \(ErrorText.message(for: error))")
            )
        }
    }

    // MARK: - Routing

    private func route(_ event: HostEvent) async {
        let label = { (peer: String) in CoreHost.shared.labelOf(peer: peer) }
        switch event {
        case let .transferOffer(id, peer, _, _, fileCount, totalBytes, _):
            postedOffers.insert(id)
            let size = ByteFormat.bytes(totalBytes)
            let body = fileCount == 1
                ? String(localized: "1 file, \(size)")
                : String(localized: "\(fileCount) files, \(size)")
            await post(
                id: Self.offerIdentifier(id),
                title: String(localized: "\(label(peer)) wants to send files"),
                body: body,
                category: NotificationKeys.offerCategory,
                userInfo: [NotificationKeys.transferIdKey: id]
            )

        case let .transferComplete(id, peer, direction, _, _, _, _, delivered):
            removeOffer(id)
            guard direction == .receive, !delivered.isEmpty else {
                return
            }
            let title: String
            if delivered.count == 1, let first = delivered.first {
                title = String(localized: "Received \(Self.fileName(of: first.path))")
            } else {
                title = String(localized: "\(delivered.count) files received")
            }
            await post(
                id: "received.\(id)",
                title: title,
                body: String(localized: "From \(label(peer)), saved in Files > PlayDrop")
            )

        case let .transferFailed(id, peer, direction, reason):
            removeOffer(id)
            let title = direction == .send
                ? String(localized: "Sending to \(label(peer)) failed")
                : String(localized: "Receiving from \(label(peer)) failed")
            await post(id: "failed.\(id)", title: title, body: reason)

        case let .ping(peer, message):
            let text = message?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
            await post(
                id: "ping.\(peer)",
                title: String(localized: "Ping from \(label(peer))"),
                body: text.isEmpty ? String(localized: "Ping") : text
            )

        case let .paired(peer, shortId, name):
            await post(
                id: "paired.\(peer)",
                title: String(localized: "New device paired"),
                body: String(localized: "New device paired: \(name) (\(shortId))")
            )

        case let .pairingFailed(reason):
            await post(id: "pairing-failed", title: String(localized: "Pairing failed"), body: reason)

        default:
            break
        }
    }

    private func removeOffer(_ transferId: String) {
        guard postedOffers.remove(transferId) != nil else {
            return
        }
        let id = Self.offerIdentifier(transferId)
        let center = UNUserNotificationCenter.current()
        center.removeDeliveredNotifications(withIdentifiers: [id])
        center.removePendingNotificationRequests(withIdentifiers: [id])
    }

    /// Posts one notification when permission allows it; logs once when it does not.
    private func post(
        id: String, title: String, body: String, category: String? = nil, userInfo: [String: String] = [:]
    ) async {
        await refreshAuthorizationStatus()
        switch authorizationStatus {
        case .authorized, .provisional, .ephemeral:
            break
        default:
            if !warnedDenied {
                warnedDenied = true
                Log.notify.info("notifications are off; received files and offers show only in the app")
            }
            return
        }
        do {
            try await Self.deliver(id: id, title: title, body: body, category: category, userInfo: userInfo)
        } catch {
            Log.notify.error("posting a notification failed: \(Log.describe(error), privacy: .public)")
        }
    }

    // MARK: - Helpers

    private static func offerIdentifier(_ transferId: String) -> String {
        "offer.\(transferId)"
    }

    /// The display name of a delivered file from its manifest path (`a/b/c.txt` -> `c.txt`).
    private static func fileName(of manifestPath: String) -> String {
        let last = manifestPath.split(separator: "/").last.map(String.init) ?? manifestPath
        return FileNames.sanitized(last)
    }

    private static func makeOfferCategory() -> UNNotificationCategory {
        let accept = UNNotificationAction(
            identifier: NotificationKeys.acceptAction, title: String(localized: "Accept"), options: [.foreground]
        )
        let decline = UNNotificationAction(
            identifier: NotificationKeys.declineAction, title: String(localized: "Decline"), options: [.destructive]
        )
        return UNNotificationCategory(
            identifier: NotificationKeys.offerCategory, actions: [accept, decline], intentIdentifiers: [], options: []
        )
    }

    // The UserNotifications calls below take and return non-Sendable objects, so they are
    // made from nonisolated helpers that only exchange Sendable values with the main actor.

    nonisolated private static func currentStatus() async -> UNAuthorizationStatus {
        await UNUserNotificationCenter.current().notificationSettings().authorizationStatus
    }

    nonisolated private static func requestAuthorization() async -> Bool {
        do {
            return try await UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound, .badge])
        } catch {
            Log.notify.error("permission request failed: \(Log.describe(error), privacy: .public)")
            return false
        }
    }

    nonisolated private static func deliver(
        id: String, title: String, body: String, category: String?, userInfo: [String: String]
    ) async throws {
        let content = UNMutableNotificationContent()
        content.title = title
        content.body = body
        content.sound = .default
        content.userInfo = userInfo
        if let category {
            content.categoryIdentifier = category
        }
        let request = UNNotificationRequest(identifier: id, content: content, trigger: nil)
        try await UNUserNotificationCenter.current().add(request)
    }
}

/// The notification center's delegate: shows banners while the app is in front and passes
/// offer actions to `LocalNotifications`. Stateless; the center holds it weakly, so
/// `LocalNotifications` keeps it alive.
final class NotificationDelegate: NSObject, UNUserNotificationCenterDelegate {
    /// Shows every PlayDrop notification as a banner, in the list and with sound, even
    /// though the app is in the foreground (it always is when the core posts).
    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification,
        withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void
    ) {
        completionHandler([.banner, .list, .sound])
    }

    /// Routes Accept and Decline to the core; a plain tap only opens the app.
    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse,
        withCompletionHandler completionHandler: @escaping () -> Void
    ) {
        let action = response.actionIdentifier
        let transferId = response.notification.request.content.userInfo[NotificationKeys.transferIdKey] as? String
        if let transferId, action == NotificationKeys.acceptAction || action == NotificationKeys.declineAction {
            Task { @MainActor in
                await LocalNotifications.shared.handleOfferAction(action, transferId: transferId)
            }
        }
        completionHandler()
    }
}
