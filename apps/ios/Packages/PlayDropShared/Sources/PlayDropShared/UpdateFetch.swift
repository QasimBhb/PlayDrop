// The two requests of an update check and the redirect rule, over an injected transport.
//
// What leaves the phone is decided here and nowhere else: a fixed set of headers (the
// `User-Agent` the core names and three constant defaults that stop the system from filling
// them in with something that varies), no cookie, no cache validator, no credentials. The
// transport sends one request and does not follow redirects; `UpdateFetcher` follows them
// itself, so every hop is checked against the rules and every hop is built the same way.
import Foundation

/// One request to send. Headers are complete: the transport adds none of its own that it can
/// avoid.
public struct UpdateRequest: Equatable, Sendable {
    /// The address.
    public let url: URL
    /// Every header to send.
    public let headers: [String: String]
    /// Seconds before the request is given up.
    public let timeout: TimeInterval

    /// Creates a request.
    public init(url: URL, headers: [String: String], timeout: TimeInterval) {
        self.url = url
        self.headers = headers
        self.timeout = timeout
    }
}

/// An answer. A redirect carries its `Location` and no body.
public struct UpdateResponse: Equatable, Sendable {
    /// HTTP status.
    public let status: Int
    /// The `Location` header, if any.
    public let location: String?
    /// The body of a 200 answer, at most the limit the transport was given.
    public let body: Data

    /// Creates a response.
    public init(status: Int, location: String? = nil, body: Data = Data()) {
        self.status = status
        self.location = location
        self.body = body
    }
}

/// Sends one request. An implementation must not follow redirects and must stop reading and
/// throw `UpdateCheckError.tooLarge` when a 200 body would pass `maxBodyBytes`.
public protocol UpdateTransport: Sendable {
    /// Sends `request` and returns the answer.
    func send(_ request: UpdateRequest, maxBodyBytes: Int) async throws -> UpdateResponse
}

/// How a request is built.
public enum UpdateRequestPlan {
    /// A constant `Accept-Language`. Left alone, the system sends the phone's languages, which
    /// say where its owner is from.
    public static let acceptLanguage = "en"
    /// A constant `Accept`, the value the system would send anyway.
    public static let accept = "*/*"
    /// No compression: the answers are tiny, the size limit then counts the bytes on the wire,
    /// and the signature is over exactly the bytes received.
    public static let acceptEncoding = "identity"

    /// The headers of every request.
    public static func headers(userAgent: String) -> [String: String] {
        [
            "User-Agent": userAgent,
            "Accept": accept,
            "Accept-Language": acceptLanguage,
            "Accept-Encoding": acceptEncoding,
        ]
    }

    /// The request for `url` under `rules`.
    public static func request(for url: URL, rules: UpdateCheckRules) -> UpdateRequest {
        UpdateRequest(url: url, headers: headers(userAgent: rules.userAgent), timeout: rules.timeout)
    }
}

/// Fetches one file, following redirects under the rules.
public struct UpdateFetcher: Sendable {
    private let rules: UpdateCheckRules
    private let transport: any UpdateTransport

    /// Creates a fetcher over `transport`.
    public init(rules: UpdateCheckRules, transport: any UpdateTransport) {
        self.rules = rules
        self.transport = transport
    }

    /// The body of `address` (an https URL), at most `maxBytes` long.
    public func fetch(_ address: String, maxBytes: Int) async throws -> Data {
        guard let first = URL(string: address), first.scheme == "https" else {
            throw UpdateCheckError.badURL
        }
        var url = first
        var redirects = 0
        while true {
            let response = try await transport.send(
                UpdateRequestPlan.request(for: url, rules: rules), maxBodyBytes: maxBytes
            )
            switch response.status {
            case 200:
                guard response.body.count <= maxBytes else { throw UpdateCheckError.tooLarge }
                return response.body
            case 301, 302, 303, 307, 308:
                redirects += 1
                guard redirects <= rules.maxRedirects else { throw UpdateCheckError.tooManyRedirects }
                guard let location = response.location,
                      let next = URL(string: location, relativeTo: url)?.absoluteURL else {
                    throw UpdateCheckError.missingLocation
                }
                guard next.scheme == "https", rules.redirectAllowed(next.absoluteString) else {
                    throw UpdateCheckError.redirectNotAllowed
                }
                url = next
            default:
                throw UpdateCheckError.badStatus(response.status)
            }
        }
    }
}
