// Linux type-check stub of UIKit: what the app and extension use, with the real labels.
@_exported import Foundation
public enum UIUserInterfaceIdiom: Int, Sendable { case unspecified, phone, pad }
@MainActor open class UIDevice {
    public static var current: UIDevice { UIDevice() }
    nonisolated init() {}
    open var name: String { "" }
    open var isBatteryMonitoringEnabled: Bool = false
    open var batteryLevel: Float { 0 }
    public enum BatteryState: Int, Sendable { case unknown, unplugged, charging, full }
    open var batteryState: BatteryState { .unknown }
    open var userInterfaceIdiom: UIUserInterfaceIdiom { .phone }
    nonisolated public static let batteryLevelDidChangeNotification = Notification.Name("l")
    nonisolated public static let batteryStateDidChangeNotification = Notification.Name("s")
}
public struct UIBackgroundTaskIdentifier: Hashable, Sendable {
    public static let invalid = UIBackgroundTaskIdentifier()
}
@MainActor open class UIApplication: UIResponder {
    public static var shared: UIApplication { UIApplication() }
    public enum State: Int, Sendable { case active, inactive, background }
    open var applicationState: State { .active }
    open func beginBackgroundTask(withName taskName: String?, expirationHandler handler: (@MainActor @Sendable () -> Void)? = nil) -> UIBackgroundTaskIdentifier { .invalid }
    open func endBackgroundTask(_ identifier: UIBackgroundTaskIdentifier) {}
}
open class UIImage: @unchecked Sendable {
    public init?(data: Data) { return nil }
    open func pngData() -> Data? { nil }
}
open class UIPasteboard {
    open class var general: UIPasteboard { UIPasteboard() }
    init() {}
    open var string: String?
    open var image: UIImage?
    open var hasStrings: Bool { false }
    open var hasImages: Bool { false }
    public struct OptionsKey: Hashable, Sendable {
        public static let localOnly = OptionsKey()
        public static let expirationDate = OptionsKey()
    }
    open func setItems(_ items: [[String: Any]], options: [OptionsKey: Any] = [:]) {}
    open func data(forPasteboardType pasteboardType: String) -> Data? { nil }
}

@MainActor open class UIResponder {
    nonisolated public init() {}
    open var next: UIResponder? { nil }
}
@MainActor open class UIViewController: UIResponder {
    open var extensionContext: NSExtensionContext? { nil }
    open func viewDidLoad() {}
    open func addChild(_ childController: UIViewController) {}
    open func didMove(toParent parent: UIViewController?) {}
    open var view: UIView! { UIView() }
}
@MainActor open class UIView {
    nonisolated public init() {}
    open var translatesAutoresizingMaskIntoConstraints: Bool = true
    open func addSubview(_ view: UIView) {}
}
open class NSExtensionContext: @unchecked Sendable {
    open var inputItems: [Any] { [] }
    open func completeRequest(returningItems items: [Any]?, completionHandler: (@Sendable (Bool) -> Void)? = nil) {}
    open func cancelRequest(withError error: Error) {}
    open func open(_ URL: URL, completionHandler: (@Sendable (Bool) -> Void)? = nil) {}
}
@MainActor open class UIFeedbackGenerator {
    nonisolated public init() {}
    open func prepare() {}
}
@MainActor open class UINotificationFeedbackGenerator: UIFeedbackGenerator {
    public enum FeedbackType: Int, Sendable { case success, warning, error }
    open func notificationOccurred(_ notificationType: FeedbackType) {}
}
