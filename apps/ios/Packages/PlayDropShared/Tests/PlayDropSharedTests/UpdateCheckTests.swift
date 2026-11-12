// The update check: its request, the redirect rule, when it runs and what the screens keep.
import XCTest
@testable import PlayDropShared

/// Answers by address and remembers every request.
actor ScriptedTransport: UpdateTransport {
    private var answers: [String: [Result<UpdateResponse, UpdateCheckError>]]
    private(set) var requests: [UpdateRequest] = []
    private(set) var limits: [Int] = []

    init(_ answers: [String: [Result<UpdateResponse, UpdateCheckError>]]) {
        self.answers = answers
    }

    func send(_ request: UpdateRequest, maxBodyBytes: Int) async throws -> UpdateResponse {
        requests.append(request)
        limits.append(maxBodyBytes)
        guard var queue = answers[request.url.absoluteString], !queue.isEmpty else {
            throw UpdateCheckError.transport
        }
        let next = queue.removeFirst()
        answers[request.url.absoluteString] = queue
        return try next.get()
    }
}

@MainActor
final class MemoryStorage: UpdateCheckStorage {
    var isEnabled = true
    var lastAnswerMs: UInt64?
    var lastFailureMs: UInt64?
}

private let sumsURL = "https://example.test/latest/SHA256SUMS"
private let sigURL = "https://example.test/latest/SHA256SUMS.sig"
private let store1 = "https://objects.example.test/sums"
private let store2 = "https://objects.example.test/sig"
private let pageURL = "https://example.test/releases/tag/v0.11.0"

private func rules(
    due: Bool = true,
    maxRedirects: Int = 2,
    decide: @escaping @Sendable (Data, Data, String) throws -> UpdateOfferInfo? = { _, _, _ in nil }
) -> UpdateCheckRules {
    UpdateCheckRules(
        sumsURL: sumsURL,
        sigURL: sigURL,
        userAgent: "PlayDrop",
        maxSumsBytes: 100,
        maxSigBytes: 10,
        maxRedirects: maxRedirects,
        timeout: 15,
        isDue: { _, _, _ in due },
        redirectAllowed: { $0.hasPrefix("https://objects.example.test/") },
        decide: decide
    )
}

private func redirect(to location: String) -> Result<UpdateResponse, UpdateCheckError> {
    .success(UpdateResponse(status: 302, location: location))
}

private func ok(_ text: String) -> Result<UpdateResponse, UpdateCheckError> {
    .success(UpdateResponse(status: 200, body: Data(text.utf8)))
}

/// The two files behind one redirect each.
private func happyScript() -> [String: [Result<UpdateResponse, UpdateCheckError>]] {
    [
        sumsURL: [redirect(to: store1)], store1: [ok("sums")],
        sigURL: [redirect(to: store2)], store2: [ok("sig")],
    ]
}

private let offerInfo = UpdateOfferInfo(version: "0.11.0", pageURL: pageURL)

final class UpdateRequestPlanTests: XCTestCase {
    func testHeadersAreExactlyTheFixedSet() {
        let headers = UpdateRequestPlan.headers(userAgent: "PlayDrop")
        XCTAssertEqual(headers, [
            "User-Agent": "PlayDrop",
            "Accept": "*/*",
            "Accept-Language": "en",
            "Accept-Encoding": "identity",
        ])
    }

    func testNoHeaderCarriesAnIdentifier() {
        let names = Set(UpdateRequestPlan.headers(userAgent: "PlayDrop").keys.map { $0.lowercased() })
        for forbidden in ["cookie", "referer", "authorization", "if-none-match", "if-modified-since"] {
            XCTAssertFalse(names.contains(forbidden), forbidden)
        }
    }
}

final class UpdateFetcherTests: XCTestCase {
    func testFollowsARedirectAndBuildsEveryHopTheSame() async throws {
        let transport = ScriptedTransport([sumsURL: [redirect(to: store1)], store1: [ok("body")]])
        let fetcher = UpdateFetcher(rules: rules(), transport: transport)
        let body = try await fetcher.fetch(sumsURL, maxBytes: 100)
        XCTAssertEqual(body, Data("body".utf8))
        let requests = await transport.requests
        XCTAssertEqual(requests.map(\.url.absoluteString), [sumsURL, store1])
        XCTAssertEqual(Set(requests.map(\.headers)), [UpdateRequestPlan.headers(userAgent: "PlayDrop")])
        XCTAssertEqual(requests.map(\.timeout), [15, 15])
        let limits = await transport.limits
        XCTAssertEqual(limits, [100, 100])
    }

    func testRelativeLocationIsResolvedThenChecked() async throws {
        let transport = ScriptedTransport([
            "https://objects.example.test/a/b": [redirect(to: "/c/d")],
            "https://objects.example.test/c/d": [ok("x")],
        ])
        let fetcher = UpdateFetcher(rules: rules(), transport: transport)
        let body = try await fetcher.fetch("https://objects.example.test/a/b", maxBytes: 10)
        XCTAssertEqual(body, Data("x".utf8))
    }

    func testRedirectToAnotherHostIsRefused() async {
        let transport = ScriptedTransport([sumsURL: [redirect(to: "https://evil.test/x")]])
        let fetcher = UpdateFetcher(rules: rules(), transport: transport)
        await assertFetchFails(fetcher, .redirectNotAllowed)
        let count = await transport.requests.count
        XCTAssertEqual(count, 1, "the refused hop is never requested")
    }

    func testRedirectToPlainHTTPIsRefusedEvenOnAnAllowedHost() async {
        let transport = ScriptedTransport([sumsURL: [redirect(to: "http://objects.example.test/x")]])
        await assertFetchFails(UpdateFetcher(rules: rules(), transport: transport), .redirectNotAllowed)
    }

    func testTooManyRedirectsEndTheCheck() async {
        let transport = ScriptedTransport([
            sumsURL: [redirect(to: store1)],
            store1: [redirect(to: store1 + "2")],
            store1 + "2": [redirect(to: store1 + "3")],
        ])
        await assertFetchFails(
            UpdateFetcher(rules: rules(maxRedirects: 2), transport: transport), .tooManyRedirects
        )
    }

    func testExactlyTheMaximumNumberOfRedirectsIsAllowed() async throws {
        let transport = ScriptedTransport([
            sumsURL: [redirect(to: store1)],
            store1: [redirect(to: store1 + "2")],
            store1 + "2": [ok("fine")],
        ])
        let body = try await UpdateFetcher(rules: rules(maxRedirects: 2), transport: transport)
            .fetch(sumsURL, maxBytes: 100)
        XCTAssertEqual(body, Data("fine".utf8))
    }

    func testRedirectWithoutLocationFails() async {
        let transport = ScriptedTransport([sumsURL: [.success(UpdateResponse(status: 302))]])
        await assertFetchFails(UpdateFetcher(rules: rules(), transport: transport), .missingLocation)
    }

    func testOtherStatusesFail() async {
        let transport = ScriptedTransport([sumsURL: [.success(UpdateResponse(status: 404))]])
        await assertFetchFails(UpdateFetcher(rules: rules(), transport: transport), .badStatus(404))
    }

    func testBodyOverTheLimitFailsEvenIfTheTransportLetItThrough() async {
        let transport = ScriptedTransport([sumsURL: [ok(String(repeating: "a", count: 101))]])
        await assertFetchFails(UpdateFetcher(rules: rules(), transport: transport), .tooLarge)
    }

    func testStartAddressMustBeHTTPS() async {
        let transport = ScriptedTransport([:])
        let fetcher = UpdateFetcher(rules: rules(), transport: transport)
        do {
            _ = try await fetcher.fetch("http://example.test/x", maxBytes: 10)
            XCTFail("expected a failure")
        } catch {
            XCTAssertEqual(error as? UpdateCheckError, .badURL)
        }
        let count = await transport.requests.count
        XCTAssertEqual(count, 0)
    }

    private func assertFetchFails(
        _ fetcher: UpdateFetcher, _ expected: UpdateCheckError, line: UInt = #line
    ) async {
        do {
            _ = try await fetcher.fetch(sumsURL, maxBytes: 100)
            XCTFail("expected a failure", line: line)
        } catch {
            XCTAssertEqual(error as? UpdateCheckError, expected, line: line)
        }
    }
}

@MainActor
final class UpdateCheckEngineTests: XCTestCase {
    private func engine(
        rules: UpdateCheckRules,
        transport: ScriptedTransport,
        storage: MemoryStorage,
        version: String? = "0.10.0",
        now: UInt64 = 5_000
    ) -> UpdateCheckEngine {
        UpdateCheckEngine(
            rules: rules, transport: transport, storage: storage, currentVersion: version, now: { now }
        )
    }

    func testAnOfferIsReturnedAndTheAnswerTimeRemembered() async {
        let storage = MemoryStorage()
        let transport = ScriptedTransport(happyScript())
        let seen = Seen()
        let e = engine(
            rules: rules(decide: { sums, sig, version in
                seen.record(sums, sig, version)
                return offerInfo
            }),
            transport: transport, storage: storage
        )
        let outcome = await e.run(.opened)
        XCTAssertEqual(outcome, .offer(UpdateNotice(version: "0.11.0", pageURL: URL(string: pageURL)!)))
        XCTAssertEqual(storage.lastAnswerMs, 5_000)
        XCTAssertNil(storage.lastFailureMs)
        XCTAssertEqual(seen.values, ["sums|sig|0.10.0"])
        let requests = await transport.requests
        let urls = requests.map(\.url.absoluteString)
        XCTAssertEqual(urls, [sumsURL, store1, sigURL, store2], "sums first, then the signature, nothing else")
    }

    func testNothingNewIsUpToDate() async {
        let storage = MemoryStorage()
        let e = engine(rules: rules(), transport: ScriptedTransport(happyScript()), storage: storage)
        let outcome = await e.run(.manual)
        XCTAssertEqual(outcome, .upToDate)
        XCTAssertEqual(storage.lastAnswerMs, 5_000)
    }

    func testSwitchOffSendsNothingForEitherTrigger() async {
        let storage = MemoryStorage()
        storage.isEnabled = false
        let transport = ScriptedTransport(happyScript())
        let e = engine(rules: rules(), transport: transport, storage: storage)
        let opened = await e.run(.opened)
        let manual = await e.run(.manual)
        XCTAssertEqual(opened, .skipped)
        XCTAssertEqual(manual, .skipped)
        let count = await transport.requests.count
        XCTAssertEqual(count, 0)
    }

    func testOpenedWaitsForTheDueRuleButManualDoesNot() async {
        let storage = MemoryStorage()
        let transport = ScriptedTransport(happyScript())
        let e = engine(rules: rules(due: false), transport: transport, storage: storage)
        let opened = await e.run(.opened)
        XCTAssertEqual(opened, .skipped)
        var count = await transport.requests.count
        XCTAssertEqual(count, 0)
        let manual = await e.run(.manual)
        XCTAssertEqual(manual, .upToDate)
        count = await transport.requests.count
        XCTAssertEqual(count, 4)
    }

    func testTheDueRuleGetsTheStoredTimes() async {
        let storage = MemoryStorage()
        storage.lastAnswerMs = 11
        storage.lastFailureMs = 22
        let args = Seen()
        let r = UpdateCheckRules(
            sumsURL: sumsURL, sigURL: sigURL, userAgent: "PlayDrop", maxSumsBytes: 1, maxSigBytes: 1,
            maxRedirects: 1, timeout: 1,
            isDue: { a, f, n in
                args.note("\(String(describing: a))|\(String(describing: f))|\(n)")
                return false
            },
            redirectAllowed: { _ in false }, decide: { _, _, _ in nil }
        )
        let e = engine(rules: r, transport: ScriptedTransport([:]), storage: storage)
        _ = await e.run(.opened)
        XCTAssertEqual(args.values, ["Optional(11)|Optional(22)|5000"])
    }

    func testAFailureIsRecordedAndReported() async {
        let storage = MemoryStorage()
        let e = engine(rules: rules(), transport: ScriptedTransport([:]), storage: storage)
        let outcome = await e.run(.opened)
        XCTAssertEqual(outcome, .failed)
        XCTAssertEqual(storage.lastFailureMs, 5_000)
        XCTAssertNil(storage.lastAnswerMs)
    }

    func testFilesThatDoNotVerifyFail() async {
        let storage = MemoryStorage()
        let e = engine(
            rules: rules(decide: { _, _, _ in throw UpdateCheckError.notVerified }),
            transport: ScriptedTransport(happyScript()), storage: storage
        )
        let outcome = await e.run(.manual)
        XCTAssertEqual(outcome, .failed)
        XCTAssertNil(storage.lastAnswerMs)
        XCTAssertEqual(storage.lastFailureMs, 5_000)
    }

    func testAPageAddressThatIsNotHTTPSFails() async {
        let storage = MemoryStorage()
        let e = engine(
            rules: rules(decide: { _, _, _ in UpdateOfferInfo(version: "0.11.0", pageURL: "javascript:alert(1)") }),
            transport: ScriptedTransport(happyScript()), storage: storage
        )
        let outcome = await e.run(.manual)
        XCTAssertEqual(outcome, .failed)
    }

    func testMissingAppVersionFailsWithoutAskingTheCoreToGuess() async {
        let storage = MemoryStorage()
        let transport = ScriptedTransport(happyScript())
        let e = engine(rules: rules(), transport: transport, storage: storage, version: nil)
        let outcome = await e.run(.manual)
        XCTAssertEqual(outcome, .failed)
        let count = await transport.requests.count
        XCTAssertEqual(count, 0)
    }
}

/// Collects what the core closures were called with.
final class Seen: @unchecked Sendable {
    private let lock = NSLock()
    private var items: [String] = []

    var values: [String] {
        lock.lock()
        defer { lock.unlock() }
        return items
    }

    func record(_ a: Data, _ b: Data, _ c: String) {
        note("\(String(decoding: a, as: UTF8.self))|\(String(decoding: b, as: UTF8.self))|\(c)")
    }

    func note(_ text: String) {
        lock.lock()
        defer { lock.unlock() }
        items.append(text)
    }
}

final class UpdateCheckPresentationTests: XCTestCase {
    private let notice = UpdateNotice(version: "0.11.0", pageURL: URL(string: pageURL)!)

    func testAnOfferShowsAndLaterHidesIt() {
        var p = UpdateCheckPresentation()
        p.apply(.offer(notice), trigger: .opened)
        XCTAssertEqual(p.notice, notice)
        XCTAssertEqual(p.manualStatus, .none, "an automatic check says nothing in Settings")
        p.later()
        XCTAssertNil(p.notice)
    }

    func testLaterHoldsThroughSkippedAndFailedChecksAndEndsWithTheNextAnswer() {
        var p = UpdateCheckPresentation()
        p.apply(.offer(notice), trigger: .opened)
        p.later()
        p.apply(.skipped, trigger: .opened)
        p.apply(.failed, trigger: .opened)
        XCTAssertNil(p.notice)
        p.apply(.offer(notice), trigger: .opened)
        XCTAssertEqual(p.notice, notice, "the next check that gets an answer offers it again")
    }

    func testFailedAutomaticCheckShowsNothingButFailedManualCheckSaysSo() {
        var p = UpdateCheckPresentation()
        p.apply(.failed, trigger: .opened)
        XCTAssertEqual(p.manualStatus, .none)
        p.apply(.failed, trigger: .manual)
        XCTAssertEqual(p.manualStatus, .failed)
    }

    func testManualStatusFollowsManualAnswers() {
        var p = UpdateCheckPresentation()
        p.apply(.upToDate, trigger: .manual)
        XCTAssertEqual(p.manualStatus, .upToDate)
        p.apply(.offer(notice), trigger: .manual)
        XCTAssertEqual(p.manualStatus, .updateAvailable("0.11.0"))
        p.apply(.skipped, trigger: .manual)
        XCTAssertEqual(p.manualStatus, .updateAvailable("0.11.0"), "a skipped check changes nothing")
    }

    func testUpToDateClearsAStaleNotice() {
        var p = UpdateCheckPresentation()
        p.apply(.offer(notice), trigger: .opened)
        p.apply(.upToDate, trigger: .opened)
        XCTAssertNil(p.notice)
    }
}
