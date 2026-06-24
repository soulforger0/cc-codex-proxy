import Foundation
import SwiftUI

@MainActor
final class ProxyAppModel: ObservableObject {
    @Published var isRunning = false
    @Published var isAuthenticated = false
    @Published var isLoggingIn = false
    @Published var isCheckingAuthStatus = false
    @Published var isRefreshingClaudeSettings = false
    @Published var isInstallingClaudeSettings = false
    @Published var isRestoringClaudeSettings = false
    @Published var statusText = "Not checked"
    @Published var authStatusText = "OAuth not checked"
    @Published var authDetailText = "Login when you are ready to store or verify OAuth tokens."
    @Published var claudeSettingsPreview: ClaudeSettingsPreview?
    @Published var claudeSettingsPreviewError: String?
    @Published var lastMessage = ""
    @Published var model = "gpt-5.4[1m]"
    @Published var smallModel = "gpt-5.4-mini[1m]"
    @Published var port = 18765
    @Published var autoCompactWindow = 272_000

    private var proxyProcess: Process?

    func refresh() async {
        await refreshProxyStatus(updateLastMessage: true)
        await refreshClaudeSettingsPreview()
    }

    private func refreshProxyStatus(updateLastMessage: Bool) async {
        let healthURL = URL(string: "http://127.0.0.1:\(port)/healthz")!

        do {
            let (_, response) = try await URLSession.shared.data(from: healthURL)
            let isHealthy = (response as? HTTPURLResponse)?.statusCode == 200
            isRunning = isHealthy
            statusText = isHealthy ? "Running on 127.0.0.1:\(port)" : "Stopped"
            if updateLastMessage {
                lastMessage = isHealthy ? "Proxy is running." : "Proxy is stopped."
            }
        } catch {
            isRunning = false
            statusText = "Stopped"
            if updateLastMessage {
                lastMessage = "Proxy is stopped."
            }
        }
    }

    func checkAuthStatus() async {
        isCheckingAuthStatus = true
        defer { isCheckingAuthStatus = false }

        do {
            let output = try await runCLI(["auth", "status"], allowFailure: true)
            applyAuthStatus(from: output, authenticated: output.contains("Authenticated: yes"))
        } catch {
            isAuthenticated = false
            authStatusText = "Authentication unavailable"
            authDetailText = error.localizedDescription
        }
    }

    func startProxy() async {
        guard proxyProcess == nil else { return }
        let process = Process()
        process.arguments = ["serve", "--port", "\(port)"]
        do {
            process.executableURL = try helperURL()
            try process.run()
            proxyProcess = process
            isRunning = true
            statusText = "Running on 127.0.0.1:\(port)"
            lastMessage = "Proxy started."
        } catch {
            lastMessage = "Failed to start proxy: \(error.localizedDescription)"
        }
    }

    func stopProxy() async {
        proxyProcess?.terminate()
        proxyProcess = nil
        isRunning = false
        statusText = "Stopped"
        lastMessage = "Proxy stopped."
    }

    func login() async {
        isLoggingIn = true
        defer { isLoggingIn = false }

        do {
            let output = try await runCLI(["auth", "login"])
            applyAuthStatus(from: output, authenticated: true)
            lastMessage = successMessage(from: output)
            await refreshProxyStatus(updateLastMessage: false)
        } catch {
            lastMessage = "Login failed: \(error.localizedDescription)"
            isAuthenticated = false
            authStatusText = "OAuth not verified"
            authDetailText = "Login did not complete. The local auth file was not updated."
        }
    }

    func installClaudeSettings() async {
        isInstallingClaudeSettings = true
        defer { isInstallingClaudeSettings = false }

        do {
            lastMessage = try await runCLI([
                "claude",
                "install-settings"
            ] + claudeSettingsArguments)
            await refreshClaudeSettingsPreview()
        } catch {
            lastMessage = "Settings install failed: \(error.localizedDescription)"
            await refreshClaudeSettingsPreview()
        }
    }

    func restoreClaudeSettings() async {
        isRestoringClaudeSettings = true
        defer { isRestoringClaudeSettings = false }

        do {
            lastMessage = try await runCLI(["claude", "restore-settings"])
            await refreshClaudeSettingsPreview()
        } catch {
            lastMessage = "Settings restore failed: \(error.localizedDescription)"
            await refreshClaudeSettingsPreview()
        }
    }

    func refreshClaudeSettingsPreview() async {
        isRefreshingClaudeSettings = true
        defer { isRefreshingClaudeSettings = false }

        do {
            let output = try await runCLI([
                "claude",
                "preview-settings"
            ] + claudeSettingsArguments)
            let data = Data(output.utf8)
            claudeSettingsPreview = try JSONDecoder().decode(ClaudeSettingsPreview.self, from: data)
            claudeSettingsPreviewError = nil
        } catch {
            claudeSettingsPreview = nil
            claudeSettingsPreviewError = error.localizedDescription
        }
    }

    func openLogs() {
        let url = FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Logs/CCCodexProxy/proxy.log")
        NSWorkspace.shared.open(url)
    }

    func openProjectPage() {
        NSWorkspace.shared.open(URL(string: "https://github.com/soulforger0/cc-codex-proxy")!)
    }

    private func runCLI(_ arguments: [String], allowFailure: Bool = false) async throws -> String {
        try await Task.detached(priority: .userInitiated) {
            let process = Process()
            let pipe = Pipe()
            process.executableURL = try helperURL()
            process.arguments = arguments
            process.standardOutput = pipe
            process.standardError = pipe
            try process.run()
            process.waitUntilExit()
            let data = pipe.fileHandleForReading.readDataToEndOfFile()
            let output = String(data: data, encoding: .utf8) ?? ""
            if process.terminationStatus != 0 && !allowFailure {
                throw ProxyAppError.commandFailed(output)
            }
            return output
        }.value
    }

    private var claudeSettingsArguments: [String] {
        [
            "--model",
            model,
            "--small-model",
            smallModel,
            "--port",
            "\(port)",
            "--auto-compact-window",
            "\(autoCompactWindow)"
        ]
    }

    private func applyAuthStatus(from output: String, authenticated: Bool) {
        guard authenticated else {
            isAuthenticated = false
            authStatusText = "Not signed in"
            authDetailText = "Login to complete ChatGPT OAuth."
            return
        }

        isAuthenticated = true
        authStatusText = "Signed in"

        if let account = value(for: "Account", in: output), !account.isEmpty {
            authDetailText = "Account \(account)"
        } else if let storage = value(for: "Storage", in: output), !storage.isEmpty {
            authDetailText = "Stored in \(storage)."
        } else {
            authDetailText = "OAuth tokens are stored for this Mac."
        }
    }

    private func successMessage(from output: String) -> String {
        if let storage = value(for: "Storage", in: output), !storage.isEmpty {
            return "OAuth login complete. Tokens saved in \(storage)."
        }
        return "OAuth login complete."
    }

    private func value(for label: String, in output: String) -> String? {
        output
            .split(whereSeparator: \.isNewline)
            .compactMap { line -> String? in
                let prefix = "\(label):"
                guard line.hasPrefix(prefix) else { return nil }
                return line.dropFirst(prefix.count).trimmingCharacters(in: .whitespacesAndNewlines)
            }
            .first
    }
}

struct ClaudeSettingsPreview: Decodable {
    let settingsPath: String
    let settingsExists: Bool
    let currentSettings: String
    let proposedSettings: String
    let latestBackupPath: String?
    let restoreSettings: String?
    let managedChanges: [ClaudeEnvChange]

    var changedCount: Int {
        managedChanges.filter { $0.action != .keep }.count
    }

    var changeSummary: String {
        changedCount == 0 ? "No changes" : "\(changedCount) managed changes"
    }

    var restoreSummary: String {
        guard let latestBackupPath else {
            return "No backup"
        }
        return URL(fileURLWithPath: latestBackupPath).lastPathComponent
    }

    var canRestore: Bool {
        restoreSettings != nil
    }
}

struct ClaudeEnvChange: Decodable, Identifiable {
    let key: String
    let action: ClaudeEnvAction
    let current: String?
    let proposed: String

    var id: String { key }

    var actionText: String {
        switch action {
        case .add:
            return "Add"
        case .change:
            return "Change"
        case .keep:
            return "Keep"
        }
    }

    var detailText: String {
        switch action {
        case .add:
            return "Set to \(proposed)"
        case .change:
            return "\(current ?? "Not set") -> \(proposed)"
        case .keep:
            return "Already \(proposed)"
        }
    }
}

enum ClaudeEnvAction: String, Decodable {
    case add
    case change
    case keep
}

private func helperURL() throws -> URL {
    let bundledHelper = Bundle.main.bundleURL
        .appendingPathComponent("Contents")
        .appendingPathComponent("Helpers")
        .appendingPathComponent("cc-codex-proxy")
    if FileManager.default.isExecutableFile(atPath: bundledHelper.path) {
        return bundledHelper
    }

    if let resourceHelper = Bundle.main.url(forResource: "cc-codex-proxy", withExtension: nil),
       FileManager.default.isExecutableFile(atPath: resourceHelper.path) {
        return resourceHelper
    }

    if Bundle.main.bundleURL.pathExtension == "app" {
        throw ProxyAppError.missingBundledHelper(bundledHelper.path)
    }

    for relativePath in [
        "target/release/cc-codex-proxy",
        "target/debug/cc-codex-proxy"
    ] {
        let devHelper = URL(fileURLWithPath: FileManager.default.currentDirectoryPath)
            .appendingPathComponent(relativePath)
        if FileManager.default.isExecutableFile(atPath: devHelper.path) {
            return devHelper
        }
    }

    throw ProxyAppError.missingDevelopmentHelper
}

enum ProxyAppError: LocalizedError {
    case commandFailed(String)
    case missingBundledHelper(String)
    case missingDevelopmentHelper

    var errorDescription: String? {
        switch self {
        case .commandFailed(let output):
            return output.isEmpty ? "Command failed" : output
        case .missingBundledHelper(let path):
            return "Bundled proxy helper is missing or not executable at \(path). Rebuild the app with scripts/build-app.sh."
        case .missingDevelopmentHelper:
            return "Proxy helper is missing. Run cargo build -p cc-codex-proxy before launching the SwiftPM app directly."
        }
    }
}
