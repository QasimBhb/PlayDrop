// SR-30 / T35: the playdrop:// scheme takes no parameters; only the scheme is compared.
import XCTest
@testable import PlayDropShared

final class URLSchemeRuleTests: XCTestCase {
    private func url(_ s: String, file: StaticString = #filePath, line: UInt = #line) -> URL {
        guard let u = URL(string: s) else {
            XCTFail("\(s) is not a URL", file: file, line: line)
            return URL(fileURLWithPath: "/")
        }
        return u
    }

    func testOurSchemeIsHandledWithAndWithoutParameters() {
        XCTAssertTrue(URLSchemeRule.shouldHandle(url("playdrop://")))
        XCTAssertTrue(URLSchemeRule.shouldHandle(url("playdrop://open")))
        XCTAssertTrue(URLSchemeRule.shouldHandle(url("playdrop://open?x=1")))
    }

    func testSchemeIsCaseInsensitive() {
        XCTAssertTrue(URLSchemeRule.shouldHandle(url("PLAYDROP://")))
        XCTAssertTrue(URLSchemeRule.shouldHandle(url("PlayDrop://open")))
    }

    func testOtherSchemesAreRefused() {
        XCTAssertFalse(URLSchemeRule.shouldHandle(url("https://playdrop.example/")))
        XCTAssertFalse(URLSchemeRule.shouldHandle(url("playdrops://")))
        XCTAssertFalse(URLSchemeRule.shouldHandle(url("playdrop-x://")))
        XCTAssertFalse(URLSchemeRule.shouldHandle(url("file:///playdrop://")))
        XCTAssertFalse(URLSchemeRule.shouldHandle(url("mailto:playdrop@example.com")))
    }

    func testParametersChangeNothing() {
        let bare = URLSchemeRule.shouldHandle(url("playdrop://"))
        XCTAssertTrue(bare)
        let variants = [
            "playdrop://pair?id=abc123&secret=s3cret",
            "playdrop://anything/else#frag",
            "playdrop://host:1234/path/to?a=b&c=d#e",
            "playdrop:///?send=file",
        ]
        for v in variants {
            XCTAssertEqual(URLSchemeRule.shouldHandle(url(v)), bare, v)
        }
        // A pairing-looking link on another scheme is still refused.
        XCTAssertFalse(URLSchemeRule.shouldHandle(url("https://pair?id=abc&secret=s")))
    }
}
