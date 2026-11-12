import Foundation
import Observation
import PlayDropCore
import PlayDropShared
#if canImport(FoundationNetworking)
import FoundationNetworking
#endif

// The update check on the iPhone (`docs/design/update-check.md`). The decisions (signature,
// version, once a day, redirect hosts, size limits) are the core's, reached through
// `UpdateCheckRules.live`; the order of the two requests, the switch and the remembered times
// are `UpdateCheckEngine` in PlayDropShared. This file adds the two things only the app can
// do: send the requests with `URLSession`, and expose the result to the screens.
//
// The iPhone only shows a notice with a link to the release page. Nothing is downloaded or
// installed from here, and nothing is asked unless the Settings switch is on.

extension AppSettings: UpdateCheckStorage {
    public var isEnabled: Bool { updateCheckEnabled }

    public var lastAnswerMs: UInt64? {
        get { updateLastAnswerMs }
        set { updateLastAnswerMs = newValue }
    }

    public var lastFailureMs: UInt64? {
        get { updateLastFailureMs }
        set { updateLastFailureMs = newValue }
    }
}

extension UpdateCheckRules {
    /// The rules as the Rust core states them (`playdrop-ffi` 0.6.0).
    static var live: UpdateCheckRules {
        UpdateCheckRules(
            sumsURL: updateSumsUrl(),
            sigURL: updateSigUrl(),
            userAgent: updateUserAgent(),
            maxSumsBytes: Int(clamping: updateMaxSumsLen()),
            maxSigBytes: Int(clamping: updateMaxSigLen()),
            maxRedirects: Int(clamping: updateMaxRedirects()),
            timeout: TimeInterval(updateRequestTimeoutMs()) / 1000,
            isDue: { lastAnswerMs, lastFailureMs, nowMs in
                updateCheckDue(lastAnswerMs: lastAnswerMs, lastFailureMs: lastFailureMs, nowMs: nowMs)
            },
            redirectAllowed: { updateRedirectAllowed(url: $0) },
            decide: { sums, sig, currentVersion in
                let offer = try updateOffer(sums: sums, sig: sig, currentVersion: currentVersion, kind: .ios)
                return offer.map { UpdateOfferInfo(version: $0.version, pageURL: $0.pageUrl) }
            }
        )
    }
}

/// What the screens see of the update check; the one instance is in the environment.
@MainActor
@Observable
final class UpdateChecker {
    /// The shared instance, over the shared settings and the real network.
    static let shared = UpdateChecker()

    /// The notice to offer and the line under "Check now".
    private(set) var presentation = UpdateCheckPresentation()
    /// A check is on its way.
    private(set) var checking = false

    @ObservationIgnored private let engine: UpdateCheckEngine

    /// A checker over `engine`; by default the real one.
    // The default is nil rather than a built engine: a default argument is evaluated outside
    // the main actor, where the main-actor settings instance cannot be read.
    init(engine: UpdateCheckEngine? = nil) {
        self.engine = engine ?? UpdateCheckEngine(
            rules: .live,
            transport: URLSessionUpdateTransport(),
            storage: AppSettings.shared,
            currentVersion: Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String,
            now: { UInt64(max(0, Date().timeIntervalSince1970 * 1000)) }
        )
    }

    /// The app came to the foreground: check if the switch is on and a check is due.
    func checkWhenOpened() async {
        await run(.opened)
    }

    /// "Check now": check whenever the switch is on.
    func checkNow() async {
        await run(.manual)
    }

    /// "Later": hide the notice until a check brings it back.
    func later() {
        presentation.later()
    }

    private func run(_ trigger: UpdateCheckTrigger) async {
        guard !checking else { return }
        checking = true
        defer { checking = false }
        let outcome = await engine.run(trigger)
        presentation.apply(outcome, trigger: trigger)
    }
}

// MARK: - URLSession transport

/// Sends one update request with `URLSession` and reports what came back, without following
/// redirects (`UpdateFetcher` does that, hop by hop, under the core's rules).
///
/// The session is ephemeral and holds nothing: no cookie storage, no cache, no credential
/// storage. Every header is set by `UpdateRequestPlan`. What the system still adds on its
/// own is the HTTP framing (`Host`, `Connection`) and whatever the TLS and HTTP/2 or HTTP/3
/// layer sends for every client.
struct URLSessionUpdateTransport: UpdateTransport {
    func send(_ request: UpdateRequest, maxBodyBytes: Int) async throws -> UpdateResponse {
        let exchange = UpdateExchange(maxBodyBytes: maxBodyBytes)
        let configuration = URLSessionConfiguration.ephemeral
        configuration.urlCache = nil
        configuration.httpCookieStorage = nil
        configuration.urlCredentialStorage = nil
        configuration.httpShouldSetCookies = false
        configuration.requestCachePolicy = .reloadIgnoringLocalCacheData
        configuration.timeoutIntervalForRequest = request.timeout
        configuration.timeoutIntervalForResource = request.timeout
        configuration.httpAdditionalHeaders = request.headers
        let queue = OperationQueue()
        queue.maxConcurrentOperationCount = 1
        let session = URLSession(configuration: configuration, delegate: exchange, delegateQueue: queue)
        defer { session.finishTasksAndInvalidate() }

        var urlRequest = URLRequest(
            url: request.url, cachePolicy: .reloadIgnoringLocalCacheData, timeoutInterval: request.timeout
        )
        urlRequest.httpMethod = "GET"
        urlRequest.httpShouldHandleCookies = false
        for (name, value) in request.headers {
            urlRequest.setValue(value, forHTTPHeaderField: name)
        }
        return try await withTaskCancellationHandler {
            try await exchange.run(session.dataTask(with: urlRequest))
        } onCancel: {
            exchange.cancel()
        }
    }
}

/// One request's delegate: collects the answer, refuses to follow a redirect, and stops
/// reading a body that passes its limit. Callbacks arrive one at a time on the session's
/// serial queue; the lock covers the calls from `run` and `cancel`.
private final class UpdateExchange: NSObject, URLSessionDataDelegate, @unchecked Sendable {
    private let maxBodyBytes: Int
    private let lock = NSLock()
    private var continuation: CheckedContinuation<UpdateResponse, Error>?
    private var task: URLSessionDataTask?
    private var status: Int?
    private var location: String?
    private var body = Data()
    private var tooLarge = false
    private var cancelled = false

    init(maxBodyBytes: Int) {
        self.maxBodyBytes = maxBodyBytes
    }

    func run(_ task: URLSessionDataTask) async throws -> UpdateResponse {
        try await withCheckedThrowingContinuation { continuation in
            lock.lock()
            let alreadyCancelled = cancelled
            if !alreadyCancelled {
                self.continuation = continuation
                self.task = task
            }
            lock.unlock()
            if alreadyCancelled {
                continuation.resume(throwing: CancellationError())
            } else {
                task.resume()
            }
        }
    }

    func cancel() {
        lock.lock()
        cancelled = true
        let task = self.task
        lock.unlock()
        task?.cancel()
    }

    // A redirect is reported, not followed: answering nil makes the 3xx the final response.
    func urlSession(
        _ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse,
        newRequest request: URLRequest, completionHandler: @escaping (URLRequest?) -> Void
    ) {
        completionHandler(nil)
    }

    func urlSession(
        _ session: URLSession, dataTask: URLSessionDataTask, didReceive response: URLResponse,
        completionHandler: @escaping (URLSession.ResponseDisposition) -> Void
    ) {
        guard let http = response as? HTTPURLResponse else {
            completionHandler(.cancel)
            return
        }
        lock.lock()
        status = http.statusCode
        location = http.value(forHTTPHeaderField: "Location")
        let declared = http.expectedContentLength
        let oversize = http.statusCode == 200 && declared > Int64(maxBodyBytes)
        if oversize { tooLarge = true }
        lock.unlock()
        // Only a 200 body is wanted: a redirect or an error page is dropped unread.
        completionHandler(http.statusCode == 200 && !oversize ? .allow : .cancel)
    }

    func urlSession(_ session: URLSession, dataTask: URLSessionDataTask, didReceive data: Data) {
        lock.lock()
        body.append(data)
        let over = body.count > maxBodyBytes
        if over { tooLarge = true }
        lock.unlock()
        if over { dataTask.cancel() }
    }

    func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: Error?) {
        lock.lock()
        let continuation = self.continuation
        self.continuation = nil
        let result: Result<UpdateResponse, Error>
        if tooLarge {
            result = .failure(UpdateCheckError.tooLarge)
        } else if cancelled {
            result = .failure(CancellationError())
        } else if let status, status != 200 {
            // The body was cancelled on purpose; the status is the answer.
            result = .success(UpdateResponse(status: status, location: location))
        } else if error != nil || status == nil {
            result = .failure(UpdateCheckError.transport)
        } else {
            result = .success(UpdateResponse(status: 200, location: location, body: body))
        }
        lock.unlock()
        continuation?.resume(with: result)
    }
}
