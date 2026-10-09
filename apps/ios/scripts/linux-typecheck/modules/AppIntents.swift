// Linux type-check stub of AppIntents: the shapes PlayDrop's intents use, with the real labels.
@_exported import Foundation
public struct LocalizedStringResource: ExpressibleByStringInterpolation, Sendable {
    public init(stringLiteral value: String) {}
    public init(stringInterpolation: DefaultStringInterpolation) {}
}
public struct IntentDescription: ExpressibleByStringLiteral, Sendable {
    public init(_ descriptionText: LocalizedStringResource, categoryName: LocalizedStringResource? = nil) {}
    public init(stringLiteral value: String) {}
}
public struct IntentDialog: ExpressibleByStringInterpolation, Sendable {
    public init(stringLiteral value: String) {}
    public init(stringInterpolation: DefaultStringInterpolation) {}
}
public protocol IntentResult: Sendable {}
public protocol ProvidesDialog {}
public struct IntentResultContainer: IntentResult, ProvidesDialog {}
extension IntentResult {
    public static func result(dialog: IntentDialog) -> some IntentResult & ProvidesDialog where Self == IntentResultContainer { IntentResultContainer() }
}
public protocol AppIntent: Sendable {
    associatedtype PerformResult: IntentResult
    static var title: LocalizedStringResource { get }
    static var description: IntentDescription? { get }
    static var openAppWhenRun: Bool { get }
    init()
    func perform() async throws -> PerformResult
}
extension AppIntent {
    public static var description: IntentDescription? { nil }
    public static var openAppWhenRun: Bool { false }
}
@propertyWrapper public struct Parameter<Value>: @unchecked Sendable {
    public var wrappedValue: Value
    public init<W>(title: LocalizedStringResource, description: LocalizedStringResource? = nil) where Value == W? { wrappedValue = nil }
}
public struct TypeDisplayRepresentation: ExpressibleByStringLiteral, Sendable {
    public init(stringLiteral value: String) {}
}
public struct DisplayRepresentation: Sendable {
    public init(title: LocalizedStringResource, subtitle: LocalizedStringResource? = nil) {}
}
public protocol EntityQuery: Sendable {
    associatedtype Entity: AppEntity
    init()
    func entities(for identifiers: [Entity.ID]) async throws -> [Entity]
    func suggestedEntities() async throws -> [Entity]
}
public protocol AppEntity: Identifiable, Sendable {
    associatedtype DefaultQuery: EntityQuery
    static var typeDisplayRepresentation: TypeDisplayRepresentation { get }
    static var defaultQuery: DefaultQuery { get }
    var displayRepresentation: DisplayRepresentation { get }
}
public struct AppShortcutPhraseToken: Sendable { public static let applicationName = AppShortcutPhraseToken() }
public struct AppShortcutPhrase<Intent: AppIntent>: ExpressibleByStringInterpolation, Sendable {
    public struct StringInterpolation: StringInterpolationProtocol {
        public init(literalCapacity: Int, interpolationCount: Int) {}
        public mutating func appendLiteral(_ literal: String) {}
        public mutating func appendInterpolation(_ token: AppShortcutPhraseToken) {}
    }
    public init(stringLiteral value: String) {}
    public init(stringInterpolation: StringInterpolation) {}
}
public struct AppShortcut: Sendable {
    public init<Intent: AppIntent>(intent: Intent, phrases: [AppShortcutPhrase<Intent>], shortTitle: LocalizedStringResource, systemImageName: String) {}
}
@resultBuilder public enum AppShortcutsBuilder {
    public static func buildBlock(_ components: AppShortcut...) -> [AppShortcut] { components }
}
public protocol AppShortcutsProvider {
    @AppShortcutsBuilder static var appShortcuts: [AppShortcut] { get }
}
