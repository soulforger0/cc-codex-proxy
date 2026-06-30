import Foundation
import AppKit
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
    @Published var isInstallingClaudeShim = false
    @Published var isSavingDeepSeekAPIKey = false
    @Published var isSavingCustomOpenAIAPIKey = false
    @Published var isDeepSeekKeyInputExpanded = false
    @Published var isCustomOpenAIKeyInputExpanded = false
    @Published var statusText = "Not checked"
    @Published var transportDetailText = "Start the proxy to see the active upstream method."
    @Published var transportBadgeText = "Waiting"
    @Published var transportConfiguredMode = ""
    @Published var transportCurrentMethod: String?
    @Published var authStatusText = "OAuth not checked"
    @Published var authDetailText = "Login when you are ready to store or verify OAuth tokens."
    @Published var claudeShimStatusText = "Claude command shim not checked"
    @Published var claudeSettingsPreview: ClaudeSettingsPreview?
    @Published var claudeSettingsPreviewError: String?
    @Published var lastMessage = ""
    @Published var provider = "codex"
    @Published var deepSeekAPIKey = ""
    @Published var customOpenAIBaseURL = ""
    @Published var customOpenAIAPIKey = ""
    @Published var customOpenAIProtocol = "responses"
    @Published var model = "gpt-5.5[1m]"
    @Published var smallModel = "gpt-5.4-mini[1m]"
    @Published var port = 18765
    @Published var autoCompactWindow = 272_000

    private var proxyProcess: Process?

    func refresh() async {
        await refreshProxyStatus(updateLastMessage: true)
        await checkAuthStatus()
        await refreshClaudeSettingsPreview()
    }

    func refreshRuntimeStatus() async {
        await refreshProxyStatus(updateLastMessage: false)
    }

    private func refreshProxyStatus(updateLastMessage: Bool) async {
        let healthURL = URL(string: "http://127.0.0.1:\(port)/healthz")!

        do {
            let (_, response) = try await URLSession.shared.data(from: healthURL)
            let isHealthy = (response as? HTTPURLResponse)?.statusCode == 200
            isRunning = isHealthy
            statusText = isHealthy ? "Running on 127.0.0.1:\(port)" : "Stopped"
            if isHealthy {
                await refreshTransportStatus()
            } else {
                applyStoppedTransportStatus()
            }
            if updateLastMessage {
                lastMessage = isHealthy ? "Proxy is running." : "Proxy is stopped."
            }
        } catch {
            isRunning = false
            statusText = "Stopped"
            applyStoppedTransportStatus()
            if updateLastMessage {
                lastMessage = "Proxy is stopped."
            }
        }
    }

    func checkAuthStatus() async {
        isCheckingAuthStatus = true
        defer { isCheckingAuthStatus = false }

        do {
            let output = try await runCLI(["auth", "status", "--provider", provider], allowFailure: true)
            let authenticated = provider == "custom-openai"
                ? !customOpenAIBaseURL.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                : output.contains("Authenticated: yes")
            applyAuthStatus(from: output, authenticated: authenticated)
        } catch {
            isAuthenticated = false
            authStatusText = "Authentication unavailable"
            authDetailText = error.localizedDescription
        }
    }

    func startProxy() async {
        guard proxyProcess == nil else { return }
        replaceCrossProviderModelDefaults()
        if provider == "custom-openai", customOpenAIBaseURL.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            lastMessage = "Enter a custom OpenAI endpoint URL before starting."
            return
        }
        do {
            _ = try await runCLI(["claude", "check-live-sessions"])
        } catch {
            let detail = error.localizedDescription
            lastMessage = "Proxy not started because Claude Code is running."
            showLiveClaudeSessionAlert(detail: detail)
            return
        }

        let process = Process()
        process.arguments = serveArguments
        do {
            process.executableURL = try helperURL()
            try process.run()
            proxyProcess = process
            isRunning = true
            statusText = "Running on 127.0.0.1:\(port)"
            transportDetailText = "Waiting for the first Codex request."
            transportBadgeText = "Waiting"
            transportCurrentMethod = nil
            lastMessage = "Proxy started."
            await installClaudeShim(updateLastMessage: false)
            try? await Task.sleep(nanoseconds: 250_000_000)
            await refreshProxyStatus(updateLastMessage: false)
        } catch {
            lastMessage = "Failed to start proxy: \(error.localizedDescription)"
        }
    }

    func stopProxy() async {
        proxyProcess?.terminate()
        proxyProcess = nil
        isRunning = false
        statusText = "Stopped"
        applyStoppedTransportStatus()
        lastMessage = "Proxy stopped. New claude launches will show an error while this app remains open."
    }

    func login() async {
        if provider == "deepseek" {
            await saveDeepSeekAPIKey()
            return
        }
        guard provider == "codex" else {
            await saveCustomOpenAIAPIKey()
            return
        }
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

    func saveDeepSeekAPIKey() async {
        let trimmed = deepSeekAPIKey.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            lastMessage = "Enter a DeepSeek API key before saving."
            return
        }
        isSavingDeepSeekAPIKey = true
        defer { isSavingDeepSeekAPIKey = false }

        do {
            _ = try await runCLI(
                ["auth", "set-api-key", "--provider", "deepseek", "--stdin"],
                stdin: trimmed
            )
            deepSeekAPIKey = ""
            lastMessage = "DeepSeek API key saved."
            await checkAuthStatus()
        } catch {
            lastMessage = "DeepSeek key save failed: \(error.localizedDescription)"
            await checkAuthStatus()
        }
    }

    func saveCustomOpenAIAPIKey() async {
        let trimmed = customOpenAIAPIKey.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            lastMessage = "Custom OpenAI API key is optional; enter one before saving."
            return
        }
        isSavingCustomOpenAIAPIKey = true
        defer { isSavingCustomOpenAIAPIKey = false }

        do {
            _ = try await runCLI(
                ["auth", "set-api-key", "--provider", "custom-openai", "--stdin"],
                stdin: trimmed
            )
            customOpenAIAPIKey = ""
            lastMessage = "Custom OpenAI API key saved."
            await checkAuthStatus()
        } catch {
            lastMessage = "Custom OpenAI key save failed: \(error.localizedDescription)"
            await checkAuthStatus()
        }
    }

    func applyProviderChange() async {
        applyProviderDefaults()
        await checkAuthStatus()
        await refreshClaudeSettingsPreview()
        await installClaudeShim(updateLastMessage: false)
        await refreshRuntimeStatus()
    }

    func installClaudeSettings() async {
        replaceCrossProviderModelDefaults()
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

    func installClaudeShim(updateLastMessage: Bool = true) async {
        replaceCrossProviderModelDefaults()
        isInstallingClaudeShim = true
        defer { isInstallingClaudeShim = false }

        do {
            let output = try await runCLI([
                "claude",
                "install-shim",
                "--app-pid",
                "\(ProcessInfo.processInfo.processIdentifier)"
            ] + claudeSettingsArguments)
            let message = output.trimmingCharacters(in: .whitespacesAndNewlines)
            claudeShimStatusText = firstLine(from: message) ?? "Claude command shim installed."
            if updateLastMessage {
                lastMessage = message
            }
        } catch {
            claudeShimStatusText = "Claude command shim unavailable"
            if updateLastMessage {
                lastMessage = "Claude shim install failed: \(error.localizedDescription)"
            }
        }
    }

    func restoreClaudeShimForTermination() {
        do {
            _ = try runCLISync(["claude", "restore-shim"], allowFailure: true)
        } catch {
            // The app is terminating; leave the crash-safe shim fallback in place.
        }
        proxyProcess?.terminate()
        proxyProcess = nil
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
        replaceCrossProviderModelDefaults()
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

    private func showLiveClaudeSessionAlert(detail: String) {
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = "Close Claude Code before starting the proxy"
        alert.informativeText = detail.isEmpty
            ? "A Claude Code session is already running. Close all Claude Code sessions, then start the proxy again."
            : detail
        alert.addButton(withTitle: "OK")
        alert.runModal()
    }

    private func runCLI(_ arguments: [String], allowFailure: Bool = false, stdin: String? = nil) async throws -> String {
        try await Task.detached(priority: .userInitiated) {
            try self.runCLISync(arguments, allowFailure: allowFailure, stdin: stdin)
        }.value
    }

    nonisolated private func runCLISync(_ arguments: [String], allowFailure: Bool = false, stdin: String? = nil) throws -> String {
        let process = Process()
        let pipe = Pipe()
        process.executableURL = try helperURL()
        process.arguments = arguments
        process.standardOutput = pipe
        process.standardError = pipe
        if let stdin {
            let input = Pipe()
            process.standardInput = input
            try process.run()
            if let data = stdin.data(using: .utf8) {
                input.fileHandleForWriting.write(data)
            }
            input.fileHandleForWriting.closeFile()
        } else {
            try process.run()
        }
        process.waitUntilExit()
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        let output = String(data: data, encoding: .utf8) ?? ""
        if process.terminationStatus != 0 && !allowFailure {
            throw ProxyAppError.commandFailed(output)
        }
        return output
    }

    private var claudeSettingsArguments: [String] {
        [
            "--provider",
            provider,
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

    private var serveArguments: [String] {
        var arguments = ["serve", "--provider", provider, "--port", "\(port)"]
        if provider == "custom-openai" {
            let trimmedBaseURL = customOpenAIBaseURL.trimmingCharacters(in: .whitespacesAndNewlines)
            if !trimmedBaseURL.isEmpty {
                arguments += ["--custom-openai-base-url", trimmedBaseURL]
            }
            arguments += ["--custom-openai-protocol", customOpenAIProtocol]
        }
        return arguments
    }

    private func applyAuthStatus(from output: String, authenticated: Bool) {
        guard authenticated else {
            isAuthenticated = false
            authStatusText = "Not signed in"
            if provider == "deepseek" {
                authDetailText = "Save a DeepSeek API key before starting the proxy."
                isDeepSeekKeyInputExpanded = true
            } else if provider == "custom-openai" {
                authDetailText = "Configure a base URL. API key is optional."
                isCustomOpenAIKeyInputExpanded = true
            } else {
                authDetailText = "Login to complete ChatGPT OAuth."
            }
            return
        }

        isAuthenticated = true
        authStatusText = provider == "deepseek" ? "API key saved" : (provider == "custom-openai" ? "Custom endpoint ready" : "Signed in")
        if provider == "deepseek" && deepSeekAPIKey.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            isDeepSeekKeyInputExpanded = false
        }
        if provider == "custom-openai" && customOpenAIAPIKey.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            isCustomOpenAIKeyInputExpanded = false
        }

        if let account = value(for: "Account", in: output), !account.isEmpty {
            authDetailText = "Account \(account)"
        } else if let storage = value(for: "Storage", in: output), !storage.isEmpty {
            authDetailText = "Stored in \(storage)."
        } else {
            if provider == "deepseek" {
                authDetailText = "DeepSeek API key is configured."
            } else if provider == "custom-openai" {
                authDetailText = "API key is optional; requests use the configured endpoint."
            } else {
                authDetailText = "OAuth tokens are stored in the local auth file."
            }
        }
    }

    private func refreshTransportStatus() async {
        do {
            let output = try await runCLI(["admin", "status", "--port", "\(port)"])
            let data = Data(output.utf8)
            let status = try JSONDecoder().decode(ProxyAdminStatus.self, from: data)
            applyTransportStatus(status.transport)
        } catch {
            transportDetailText = error.localizedDescription
            transportBadgeText = "Unknown"
            transportConfiguredMode = ""
            transportCurrentMethod = nil
        }
    }

    private func applyTransportStatus(_ status: ProxyTransportStatus?) {
        guard let status else {
            transportDetailText = "Transport status is not available from the proxy."
            transportBadgeText = "Unknown"
            transportConfiguredMode = ""
            transportCurrentMethod = nil
            return
        }

        transportConfiguredMode = status.configured
        transportCurrentMethod = status.currentMethod

        let configured = displayName(forTransport: status.configured)
        guard let currentMethod = status.currentMethod else {
            transportBadgeText = "Waiting"
            transportDetailText = "Mode \(configured). Waiting for the first Codex request."
            return
        }

        let current = displayName(forTransport: currentMethod)
        transportBadgeText = current
        if currentMethod == "http-sse", status.configured == "auto" {
            if let cooldown = status.websocketCooldownMs {
                transportDetailText = "Mode \(configured). Using HTTP SSE fallback; retry WebSocket in \(formatMilliseconds(cooldown))."
            } else {
                transportDetailText = "Mode \(configured). Using HTTP SSE fallback."
            }
        } else {
            transportDetailText = "Mode \(configured). Current method: \(current)."
        }
    }

    private func applyStoppedTransportStatus() {
        transportDetailText = "Start the proxy to see the active upstream method."
        transportBadgeText = "Idle"
        transportConfiguredMode = ""
        transportCurrentMethod = nil
    }

    private func displayName(forTransport value: String) -> String {
        switch value {
        case "deepseek", "custom-openai":
            return "HTTP SSE"
        case "auto":
            return "Auto"
        case "http-sse":
            return "HTTP SSE"
        case "websocket":
            return "WebSocket"
        default:
            return value.isEmpty ? "Unknown" : value
        }
    }

    private func formatMilliseconds(_ value: UInt64) -> String {
        if value >= 1_000 {
            return "\(max(1, value / 1_000))s"
        }
        return "\(value)ms"
    }

    private func applyProviderDefaults() {
        if provider == "deepseek" {
            model = "deepseek-v4-pro[1m]"
            smallModel = "deepseek-v4-flash"
            autoCompactWindow = 1_000_000
        } else if provider == "custom-openai" {
            model = "gpt-5.4[1m]"
            smallModel = "gpt-5.4-mini[1m]"
            autoCompactWindow = 128_000
        } else {
            model = "gpt-5.5[1m]"
            smallModel = "gpt-5.4-mini[1m]"
            autoCompactWindow = 272_000
        }
    }

    private func replaceCrossProviderModelDefaults() {
        let primary = model.trimmingCharacters(in: .whitespacesAndNewlines)
        let small = smallModel.trimmingCharacters(in: .whitespacesAndNewlines)

        if provider == "deepseek" {
            if primary.hasPrefix("gpt-") {
                model = "deepseek-v4-pro[1m]"
            }
            if small.hasPrefix("gpt-") {
                smallModel = "deepseek-v4-flash"
            }
            if autoCompactWindow == 272_000 || autoCompactWindow == 128_000 {
                autoCompactWindow = 1_000_000
            }
        } else if provider == "custom-openai" {
            if primary.hasPrefix("deepseek-") {
                model = "gpt-5.4[1m]"
            }
            if small.hasPrefix("deepseek-") {
                smallModel = "gpt-5.4-mini[1m]"
            }
            if autoCompactWindow == 272_000 || autoCompactWindow == 1_000_000 {
                autoCompactWindow = 128_000
            }
        } else {
            if primary.hasPrefix("deepseek-") {
                model = "gpt-5.5[1m]"
            }
            if small.hasPrefix("deepseek-") {
                smallModel = "gpt-5.4-mini[1m]"
            }
            if autoCompactWindow == 1_000_000 || autoCompactWindow == 128_000 {
                autoCompactWindow = 272_000
            }
        }
    }

    private func successMessage(from output: String) -> String {
        if let storage = value(for: "Storage", in: output), !storage.isEmpty {
            return "OAuth login complete. Tokens saved in \(storage)."
        }
        return "OAuth login complete."
    }

    private func firstLine(from output: String) -> String? {
        output
            .split(whereSeparator: \.isNewline)
            .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .first { !$0.isEmpty }
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

struct ProxyAdminStatus: Decodable {
    let provider: String?
    let transport: ProxyTransportStatus?
}

struct ProxyTransportStatus: Decodable {
    let configured: String
    let currentMethod: String?
    let websocketCooldownMs: UInt64?
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
