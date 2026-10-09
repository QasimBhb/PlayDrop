// FileOpener: metadata of a picked file, a readable descriptor, and refusal of non-files.
import Darwin
import PlayDropCore
import XCTest
@testable import PlayDrop

final class FileOpenerTests: XCTestCase {
    private var dir: URL!

    override func setUpWithError() throws {
        dir = try Fixtures.tempDir()
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: dir)
    }

    func testMetadata() throws {
        let url = dir.appendingPathComponent("Holiday photo.jpg")
        try Data(repeating: 7, count: 1234).write(to: url)
        let when = Date(timeIntervalSince1970: 1_791_000_000.5)
        try FileManager.default.setAttributes([.modificationDate: when], ofItemAtPath: url.path)
        let meta = try FileOpener.metadata(of: url)
        XCTAssertEqual(meta.name, "Holiday photo.jpg")
        XCTAssertEqual(meta.size, 1234)
        XCTAssertEqual(meta.mtimeMs, 1_791_000_000_500)
        XCTAssertEqual(meta.mime, "image/jpeg")
    }

    func testMetadataRefusesDirectoriesAndMissingFiles() {
        XCTAssertThrowsError(try FileOpener.metadata(of: dir)) { error in
            guard case PlayDropError.Io = error else { return XCTFail("expected Io, got \(error)") }
        }
        XCTAssertThrowsError(try FileOpener.metadata(of: dir.appendingPathComponent("missing.txt")))
    }

    func testOpenGivesAFreshDescriptorEachTime() throws {
        let url = dir.appendingPathComponent("a.txt")
        try Data("hello".utf8).write(to: url)
        let opener = FileOpener(url: url, securityScoped: false)
        let first = try opener.open()
        let second = try opener.open()
        defer {
            close(first)
            close(second)
        }
        XCTAssertGreaterThanOrEqual(first, 0)
        XCTAssertNotEqual(first, second)
        var buffer = [UInt8](repeating: 0, count: 5)
        XCTAssertEqual(read(second, &buffer, 5), 5, "starts at offset 0")
        XCTAssertEqual(String(decoding: buffer, as: UTF8.self), "hello")
    }

    func testOpenRefusesADirectory() {
        XCTAssertThrowsError(try FileOpener(url: dir, securityScoped: false).open())
    }
}
