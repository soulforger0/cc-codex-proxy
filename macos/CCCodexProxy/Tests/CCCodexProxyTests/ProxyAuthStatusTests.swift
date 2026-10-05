import Foundation
import XCTest
@testable import CCCodexProxy

final class ProxyAuthStatusTests: XCTestCase {
    @MainActor
    func testRejectedSessionMakesLoginAvailableAndNewLoginRestoresStatus() throws {
        let model = ProxyAppModel()
        model.isAuthenticated = true
        let rejected = try JSONDecoder().decode(ProxyAdminStatus.self, from: Data(#"{"provider":"codex","auth":null}"#.utf8))
        model.applyRuntimeAuthStatus(rejected)
        XCTAssertFalse(model.isAuthenticated)
        XCTAssertTrue(model.authDetailText.contains("Sign in again"))

        let renewed = try JSONDecoder().decode(ProxyAdminStatus.self, from: Data(#"{"provider":"codex","auth":{"expiresAtMs":9223372036854775807,"storage":"local auth file"}}"#.utf8))
        model.applyRuntimeAuthStatus(renewed)
        XCTAssertTrue(model.isAuthenticated)
        XCTAssertEqual(model.authStatusText, "Signed in")
    }

    @MainActor
    func testExpiredAccessTokenDoesNotAppearVerified() throws {
        let model = ProxyAppModel()
        let expired = try JSONDecoder().decode(ProxyAdminStatus.self, from: Data(#"{"provider":"codex","auth":{"expiresAtMs":1,"storage":"local auth file"}}"#.utf8))
        model.applyRuntimeAuthStatus(expired)
        XCTAssertFalse(model.isAuthenticated)
        XCTAssertEqual(model.authStatusText, "OAuth needs refresh")
    }
}
