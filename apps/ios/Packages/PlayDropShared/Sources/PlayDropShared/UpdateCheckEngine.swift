// When the update check runs and what it concludes: the switch, the once-a-day rule, the two
// requests in order, the core's decision, and the times it remembers. The iPhone only shows a
// notice; there is no download here.
//
// A failed check records its time (the core then waits an hour) and returns `.failed`; whether
// the user hears of it is the caller's choice (only after "Check now").
import Foundation

/// What the engine remembers between launches and reads from the settings.
@MainActor
public protocol UpdateCheckStorage: AnyObject {
    /// The "Check for updates" switch.
    var isEnabled: Bool { get }
    /// Unix milliseconds of the last check that got an answer.
    var lastAnswerMs: UInt64? { get set }
    /// Unix milliseconds of the last check that failed.
    var lastFailureMs: UInt64? { get set }
}

/// Why a check starts.
public enum UpdateCheckTrigger: Equatable, Sendable {
    /// The app came to the foreground: runs only when the rules say a check is due.
    case opened
    /// The user pressed "Check now": runs whenever the switch is on.
    case manual
}

/// How a check ended.
public enum UpdateCheckOutcome: Equatable, Sendable {
    /// Nothing was asked: the switch is off, no check is due, or one is already running.
    case skipped
    /// Asked, and this version is the newest.
    case upToDate
    /// Asked, and a newer release exists.
    case offer(UpdateNotice)
    /// Could not ask or could not verify the answer.
    case failed
}

/// Runs update checks; see the file comment.
@MainActor
public final class UpdateCheckEngine {
    private let rules: UpdateCheckRules
    private let fetcher: UpdateFetcher
    private let storage: any UpdateCheckStorage
    private let currentVersion: String?
    private let now: @Sendable () -> UInt64
    private var running = false

    /// True while a check is on its way.
    public var isRunning: Bool { running }

    /// Creates an engine. `currentVersion` is `CFBundleShortVersionString`; `now` is Unix
    /// milliseconds.
    public init(
        rules: UpdateCheckRules,
        transport: any UpdateTransport,
        storage: any UpdateCheckStorage,
        currentVersion: String?,
        now: @escaping @Sendable () -> UInt64
    ) {
        self.rules = rules
        fetcher = UpdateFetcher(rules: rules, transport: transport)
        self.storage = storage
        self.currentVersion = currentVersion
        self.now = now
    }

    /// Runs a check for `trigger`; see `UpdateCheckOutcome`.
    public func run(_ trigger: UpdateCheckTrigger) async -> UpdateCheckOutcome {
        guard storage.isEnabled, !running else { return .skipped }
        if trigger == .opened,
           !rules.isDue(storage.lastAnswerMs, storage.lastFailureMs, now()) {
            return .skipped
        }
        running = true
        defer { running = false }
        do {
            let offer = try await ask()
            storage.lastAnswerMs = now()
            guard let offer else { return .upToDate }
            return .offer(offer)
        } catch {
            storage.lastFailureMs = now()
            return .failed
        }
    }

    /// The two requests, then the decision.
    private func ask() async throws -> UpdateNotice? {
        guard let currentVersion, !currentVersion.isEmpty else { throw UpdateCheckError.noCurrentVersion }
        let sums = try await fetcher.fetch(rules.sumsURL, maxBytes: rules.maxSumsBytes)
        let sig = try await fetcher.fetch(rules.sigURL, maxBytes: rules.maxSigBytes)
        let offer: UpdateOfferInfo?
        do {
            offer = try rules.decide(sums, sig, currentVersion)
        } catch {
            throw UpdateCheckError.notVerified
        }
        guard let offer else { return nil }
        // The page opens in Safari: only an https address is accepted.
        guard let page = URL(string: offer.pageURL), page.scheme == "https", page.host != nil else {
            throw UpdateCheckError.notVerified
        }
        return UpdateNotice(version: offer.version, pageURL: page)
    }
}

/// What the screens show of the checks: the notice to offer, and the line under "Check now".
public struct UpdateCheckPresentation: Equatable, Sendable {
    /// What the line under "Check now" says.
    public enum ManualStatus: Equatable, Sendable {
        /// Nothing was asked by hand yet.
        case none
        /// The newest version is installed.
        case upToDate
        /// A newer version exists.
        case updateAvailable(String)
        /// The check did not work.
        case failed
    }

    /// The notice to show, if any. "Later" and a closed alert clear it.
    public private(set) var notice: UpdateNotice?
    /// The result of the last "Check now".
    public private(set) var manualStatus: ManualStatus = .none

    /// Creates an empty presentation.
    public init() {}

    /// Takes in the result of a check. A failed or skipped automatic check changes nothing, so
    /// it never shows an error nobody asked for. A completed check always replaces the notice:
    /// "Later" lasts only until the next check that gets an answer.
    public mutating func apply(_ outcome: UpdateCheckOutcome, trigger: UpdateCheckTrigger) {
        switch outcome {
        case .skipped:
            return
        case .upToDate:
            notice = nil
            if trigger == .manual { manualStatus = .upToDate }
        case let .offer(offer):
            notice = offer
            if trigger == .manual { manualStatus = .updateAvailable(offer.version) }
        case .failed:
            if trigger == .manual { manualStatus = .failed }
        }
    }

    /// "Later": hide the notice until a check brings it back.
    public mutating func later() {
        notice = nil
    }
}
