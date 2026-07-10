import Foundation
import XCTest
@testable import CCCodexProxy

final class ProxyLogEntryTests: XCTestCase {
    func testParsesStructuredAppLogEntry() throws {
        let line = #"{"fields":{"detail":"status=2","message":"Proxy helper exited unexpectedly"},"level":"ERROR","target":"CCCodexProxy.app","timestamp":"2026-07-10T03:10:11Z"}"#

        let entry = try XCTUnwrap(ProxyLogEntry.parse(line: line, source: .app, sequence: 7))

        XCTAssertEqual(entry.level, "ERROR")
        XCTAssertEqual(entry.message, "Proxy helper exited unexpectedly")
        XCTAssertEqual(entry.detail, "status=2")
        XCTAssertEqual(entry.target, "CCCodexProxy.app")
        XCTAssertEqual(entry.source, .app)
        XCTAssertEqual(entry.timestampLabel, "03:10:11")
    }

    func testParsesProxyMetadataAndFallbackText() throws {
        let structured = #"{"timestamp":"2026-07-10T03:10:11.123Z","level":"INFO","fields":{"message":"active route updated","model":"gpt-5.6-sol[1m]","small_model":"gpt-5.6-luna[1m]"},"target":"cc_codex_proxy"}"#
        let proxyEntry = try XCTUnwrap(ProxyLogEntry.parse(line: structured, source: .proxy, sequence: 1))
        let rawEntry = try XCTUnwrap(ProxyLogEntry.parse(line: "helper failed before tracing initialized", source: .app, sequence: 2))

        XCTAssertEqual(proxyEntry.message, "active route updated")
        XCTAssertTrue(proxyEntry.detail.contains("model=gpt-5.6-sol[1m]"))
        XCTAssertTrue(proxyEntry.detail.contains("small_model=gpt-5.6-luna[1m]"))
        XCTAssertEqual(rawEntry.message, "helper failed before tracing initialized")
    }

    func testReadsOnlyRequestedTailLines() throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let logURL = directory.appendingPathComponent("proxy.log")
        try (0..<20).map { "line-\($0)" }.joined(separator: "\n").write(to: logURL, atomically: true, encoding: .utf8)

        let lines = try readTailLines(at: logURL, maxBytes: 1_024, maxLines: 3)

        XCTAssertEqual(lines, ["line-17", "line-18", "line-19"])
    }
}
