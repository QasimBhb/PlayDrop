// Linux type-check stub of UserNotifications: what LocalNotifications uses, with the real labels.
import Foundation
public enum UNAuthorizationStatus: Int, Sendable { case notDetermined, denied, authorized, provisional, ephemeral }
public struct UNAuthorizationOptions: OptionSet, Sendable { public let rawValue: UInt; public init(rawValue: UInt) { self.rawValue = rawValue }
    public static let badge = UNAuthorizationOptions(rawValue: 1); public static let sound = UNAuthorizationOptions(rawValue: 2); public static let alert = UNAuthorizationOptions(rawValue: 4) }
public struct UNNotificationPresentationOptions: OptionSet, Sendable { public let rawValue: UInt; public init(rawValue: UInt) { self.rawValue = rawValue }
    public static let sound = UNNotificationPresentationOptions(rawValue: 2); public static let list = UNNotificationPresentationOptions(rawValue: 8); public static let banner = UNNotificationPresentationOptions(rawValue: 16) }
public struct UNNotificationActionOptions: OptionSet, Sendable { public let rawValue: UInt; public init(rawValue: UInt) { self.rawValue = rawValue }
    public static let destructive = UNNotificationActionOptions(rawValue: 2); public static let foreground = UNNotificationActionOptions(rawValue: 4) }
public struct UNNotificationCategoryOptions: OptionSet, Sendable { public let rawValue: UInt; public init(rawValue: UInt) { self.rawValue = rawValue } }
open class UNNotificationSettings { open var authorizationStatus: UNAuthorizationStatus { .denied } }
open class UNNotificationSound { public static var `default`: UNNotificationSound { UNNotificationSound() } }
open class UNNotificationContent { open var userInfo: [AnyHashable: Any] { [:] } }
open class UNMutableNotificationContent: UNNotificationContent {
    public override init() {}
    open var title: String = ""
    open var body: String = ""
    open var sound: UNNotificationSound?
    open override var userInfo: [AnyHashable: Any] { get { [:] } set {} }
    open var categoryIdentifier: String = ""
}
open class UNNotificationTrigger {}
open class UNNotificationRequest { public init(identifier: String, content: UNNotificationContent, trigger: UNNotificationTrigger?) { self.content = content }
    open var content: UNNotificationContent }
open class UNNotification { open var request: UNNotificationRequest { fatalError() } }
open class UNNotificationResponse { open var actionIdentifier: String { "" }; open var notification: UNNotification { fatalError() } }
open class UNNotificationAction: Hashable { public init(identifier: String, title: String, options: UNNotificationActionOptions = []) {}
    public static func == (a: UNNotificationAction, b: UNNotificationAction) -> Bool { a === b }; public func hash(into h: inout Hasher) {} }
open class UNNotificationCategory: Hashable { public init(identifier: String, actions: [UNNotificationAction], intentIdentifiers: [String], options: UNNotificationCategoryOptions = []) {}
    public static func == (a: UNNotificationCategory, b: UNNotificationCategory) -> Bool { a === b }; public func hash(into h: inout Hasher) {} }
public protocol UNUserNotificationCenterDelegate: NSObjectProtocol {
    func userNotificationCenter(_ center: UNUserNotificationCenter, willPresent notification: UNNotification, withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void)
    func userNotificationCenter(_ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse, withCompletionHandler completionHandler: @escaping () -> Void)
}
open class UNUserNotificationCenter: @unchecked Sendable {
    public static func current() -> UNUserNotificationCenter { UNUserNotificationCenter() }
    weak open var delegate: UNUserNotificationCenterDelegate?
    open func setNotificationCategories(_ categories: Set<UNNotificationCategory>) {}
    open func notificationSettings() async -> UNNotificationSettings { UNNotificationSettings() }
    open func requestAuthorization(options: UNAuthorizationOptions = []) async throws -> Bool { false }
    open func add(_ request: UNNotificationRequest) async throws {}
    open func removeDeliveredNotifications(withIdentifiers identifiers: [String]) {}
    open func removePendingNotificationRequests(withIdentifiers identifiers: [String]) {}
}
