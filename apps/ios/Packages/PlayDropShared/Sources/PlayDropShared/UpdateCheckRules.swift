// What the update check needs from the Rust core, as plain values and closures, so this package
// (which cannot import the generated bindings) can run the whole check and the tests can run it
// without the core. The app fills `UpdateCheckRules` from the core's `update_*` functions
// (`docs/design/update-check.md`, `docs/core-api.md` section 16).
//
// The iPhone only ever shows a notice: nothing is downloaded or installed from here.
import Foundation

/// A newer release the user may open in Safari.
public struct UpdateNotice: Equatable, Sendable {
    /// The newer version, `major.minor.patch`.
    public let version: String
    /// The release page (https).
    public let pageURL: URL

    /// Creates a notice.
    public init(version: String, pageURL: URL) {
        self.version = version
        self.pageURL = pageURL
    }
}

/// What the core's decision says about a newer release, before the page address is checked.
public struct UpdateOfferInfo: Equatable, Sendable {
    /// The newer version.
    public let version: String
    /// The release page, as the core gave it.
    public let pageURL: String

    /// Creates the value.
    public init(version: String, pageURL: String) {
        self.version = version
        self.pageURL = pageURL
    }
}

/// The fixed parts of an update check; see the file comment.
public struct UpdateCheckRules: Sendable {
    /// First request: the checksum file.
    public var sumsURL: String
    /// Second request: its signature.
    public var sigURL: String
    /// The `User-Agent` of every request.
    public var userAgent: String
    /// Largest accepted checksum file, in bytes.
    public var maxSumsBytes: Int
    /// Largest accepted signature file, in bytes.
    public var maxSigBytes: Int
    /// Most redirects followed for one file.
    public var maxRedirects: Int
    /// Seconds one request may take.
    public var timeout: TimeInterval
    /// The once-a-day rule: last answer, last failure and now, all Unix milliseconds.
    public var isDue: @Sendable (_ lastAnswerMs: UInt64?, _ lastFailureMs: UInt64?, _ nowMs: UInt64) -> Bool
    /// Whether a redirect may go to this address.
    public var redirectAllowed: @Sendable (_ url: String) -> Bool
    /// Verifies the two files and says whether `currentVersion` is older; throws when they do
    /// not verify.
    public var decide: @Sendable (_ sums: Data, _ sig: Data, _ currentVersion: String) throws -> UpdateOfferInfo?

    /// Creates the rules.
    public init(
        sumsURL: String,
        sigURL: String,
        userAgent: String,
        maxSumsBytes: Int,
        maxSigBytes: Int,
        maxRedirects: Int,
        timeout: TimeInterval,
        isDue: @escaping @Sendable (UInt64?, UInt64?, UInt64) -> Bool,
        redirectAllowed: @escaping @Sendable (String) -> Bool,
        decide: @escaping @Sendable (Data, Data, String) throws -> UpdateOfferInfo?
    ) {
        self.sumsURL = sumsURL
        self.sigURL = sigURL
        self.userAgent = userAgent
        self.maxSumsBytes = maxSumsBytes
        self.maxSigBytes = maxSigBytes
        self.maxRedirects = maxRedirects
        self.timeout = timeout
        self.isDue = isDue
        self.redirectAllowed = redirectAllowed
        self.decide = decide
    }
}

/// Why a check failed. Never shown with its detail: the user only hears "could not check".
public enum UpdateCheckError: Error, Equatable, Sendable {
    /// An address that is not an https URL.
    case badURL
    /// A redirect to a host the rules do not allow, or to a non-https address.
    case redirectNotAllowed
    /// More redirects than the rules allow.
    case tooManyRedirects
    /// A redirect without a usable `Location`.
    case missingLocation
    /// An answer other than 200 or a redirect.
    case badStatus(Int)
    /// A file larger than its limit.
    case tooLarge
    /// The core did not accept the files, or the page address it gave is not https.
    case notVerified
    /// The app has no version to compare with.
    case noCurrentVersion
    /// The network failed (no connection, timeout, TLS).
    case transport
}
