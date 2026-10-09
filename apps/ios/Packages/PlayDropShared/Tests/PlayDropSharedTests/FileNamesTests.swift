// Name cleaning (SR-17), unique naming and the MIME table.
import XCTest
@testable import PlayDropShared

final class FileNamesTests: XCTestCase {
    func testPlainNamesAreKept() {
        XCTAssertEqual(FileNames.sanitized("IMG_0001.jpg"), "IMG_0001.jpg")
        XCTAssertEqual(FileNames.sanitized("report final.pdf"), "report final.pdf")
        XCTAssertEqual(FileNames.sanitized(".bashrc"), ".bashrc")
    }

    func testEmptyAndDotNamesFallBack() {
        for bad in ["", "   ", ".", "..", "...", " . ", "\u{0000}"] {
            let out = FileNames.sanitized(bad)
            XCTAssertFalse(out.isEmpty, bad)
            XCTAssertNotEqual(out, ".", bad)
            XCTAssertNotEqual(out, "..", bad)
        }
        XCTAssertEqual(FileNames.sanitized(""), FileNames.fallbackName)
        XCTAssertEqual(FileNames.sanitized(".."), FileNames.fallbackName)
    }

    func testSeparatorsControlsAndBidiAreReplaced() {
        XCTAssertEqual(FileNames.sanitized("a/b\\c"), "a_b_c")
        XCTAssertEqual(FileNames.sanitized("../../etc/passwd"), ".._.._etc_passwd")
        XCTAssertEqual(FileNames.sanitized("x\u{0000}y"), "x_y")
        XCTAssertEqual(FileNames.sanitized("x\u{0085}y\u{007F}"), "x_y_")
        XCTAssertEqual(FileNames.sanitized("evil\u{202E}gpj.exe"), "evil_gpj.exe")
        XCTAssertEqual(FileNames.sanitized("a\u{2066}b\u{200F}"), "a_b_")
    }

    func testWindowsRulesApply() {
        XCTAssertEqual(FileNames.sanitized("a<b>c:d\"e|f?g*h"), "a_b_c_d_e_f_g_h")
        XCTAssertEqual(FileNames.sanitized("name. . "), "name")
        XCTAssertEqual(FileNames.sanitized("  lead"), "lead")
        XCTAssertEqual(FileNames.sanitized("CON"), "_CON")
        XCTAssertEqual(FileNames.sanitized("nul.txt"), "_nul.txt")
        XCTAssertEqual(FileNames.sanitized("com1.tar.gz"), "_com1.tar.gz")
        XCTAssertEqual(FileNames.sanitized("LPT\u{00B9}"), "_LPT\u{00B9}")
        XCTAssertEqual(FileNames.sanitized("console.txt"), "console.txt")
        XCTAssertEqual(FileNames.sanitized("COM10"), "COM10")
    }

    func testUnicodeIsNfcNormalised() {
        let decomposed = "Cafe\u{0301}.txt"
        let out = FileNames.sanitized(decomposed)
        XCTAssertEqual(Array(out.unicodeScalars), Array("Caf\u{00E9}.txt".unicodeScalars))
        XCTAssertEqual(FileNames.sanitized("写真 📷.heic"), "写真 📷.heic")
    }

    func testLongNamesAreCutToBytesKeepingTheExtension() {
        let out = FileNames.sanitized(String(repeating: "a", count: 300) + ".jpeg")
        XCTAssertEqual(out.utf8.count, 255)
        XCTAssertTrue(out.hasSuffix(".jpeg"))

        // Multi-byte characters are never split.
        let wide = FileNames.sanitized(String(repeating: "é", count: 200))
        XCTAssertLessThanOrEqual(wide.utf8.count, 255)
        XCTAssertEqual(wide.utf8.count, 254)
        XCTAssertTrue(wide.allSatisfy { $0 == "é" })

        let emoji = FileNames.sanitized(String(repeating: "👍🏽", count: 100) + ".png")
        XCTAssertLessThanOrEqual(emoji.utf8.count, 255)
        XCTAssertTrue(emoji.hasSuffix(".png"))
    }

    func testSanitisedIsIdempotent() {
        for name in ["a/b", "CON", "x. ", String(repeating: "z", count: 400), "..", "a\u{202E}b"] {
            let once = FileNames.sanitized(name)
            XCTAssertEqual(FileNames.sanitized(once), once, name)
        }
    }

    func testUniqueAddsCounter() {
        var taken: Set<String> = []
        XCTAssertEqual(FileNames.unique("a.txt") { taken.contains($0) }, "a.txt")
        taken = ["a.txt"]
        XCTAssertEqual(FileNames.unique("a.txt") { taken.contains($0) }, "a (2).txt")
        taken = ["a.txt", "a (2).txt", "a (3).txt"]
        XCTAssertEqual(FileNames.unique("a.txt") { taken.contains($0) }, "a (4).txt")
        taken = ["noext"]
        XCTAssertEqual(FileNames.unique("noext") { taken.contains($0) }, "noext (2)")
        taken = [".bashrc"]
        XCTAssertEqual(FileNames.unique(".bashrc") { taken.contains($0) }, ".bashrc (2)")
        taken = ["a.tar.gz"]
        XCTAssertEqual(FileNames.unique("a.tar.gz") { taken.contains($0) }, "a.tar (2).gz")
    }

    func testUniqueStaysWithinTheByteLimit() {
        let long = FileNames.sanitized(String(repeating: "b", count: 300) + ".mov")
        let out = FileNames.unique(long) { $0 == long }
        XCTAssertEqual(out.utf8.count, 255)
        XCTAssertTrue(out.hasSuffix(" (2).mov"))
        XCTAssertEqual(FileNames.sanitized(out), out)
    }

    func testMimeTable() {
        XCTAssertEqual(FileNames.mimeType(forExtension: "jpg"), "image/jpeg")
        XCTAssertEqual(FileNames.mimeType(forExtension: "JPEG"), "image/jpeg")
        XCTAssertEqual(FileNames.mimeType(forExtension: "heic"), "image/heic")
        XCTAssertEqual(FileNames.mimeType(forExtension: "mov"), "video/quicktime")
        XCTAssertEqual(FileNames.mimeType(forExtension: "txt"), "text/plain")
        XCTAssertEqual(FileNames.mimeType(forExtension: "json"), "application/json")
        XCTAssertNil(FileNames.mimeType(forExtension: "xyz"))
        XCTAssertNil(FileNames.mimeType(forExtension: ""))
    }

    func testExtension() {
        XCTAssertEqual(FileNames.fileExtension(of: "a.JPG"), "jpg")
        XCTAssertNil(FileNames.fileExtension(of: ".bashrc"))
        XCTAssertNil(FileNames.fileExtension(of: "noext"))
        XCTAssertNil(FileNames.fileExtension(of: "trailing."))
    }
}
