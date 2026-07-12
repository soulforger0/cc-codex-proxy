import Foundation
import AppKit
import SwiftUI

private let claudePublicPrimaryModel = "claude-opus-4-8"
private let claudePublicSmallModel = "claude-haiku-4-5"
private let defaultOpenAIPrimaryModel = "gpt-5.6-sol[1m]"
private let defaultOpenAISonnetModel = "gpt-5.6-terra[1m]"
private let defaultOpenAISmallModel = "gpt-5.6-luna[1m]"

@MainActor
final class ProxyAppModel: ObservableObject {
    @Published var isRunning = false
    @Published var isStartingProxy = false
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
    @Published var customOpenAITransport = "auto"
    @Published var model = defaultOpenAIPrimaryModel
    @Published var sonnetModel = defaultOpenAISonnetModel
    @Published var smallModel = defaultOpenAISmallModel
    @Published var port = 18765
    @Published var autoCompactWindow = 372_000
    @Published private(set) var logEntries: [ProxyLogEntry] = []
    @Published private(set) var isRefreshingLogs = false
    @Published private(set) var logLoadError: String?
    @Published private(set) var lastStartupFailure: String?

    private var proxyProcess: Process?
    private var proxyStandardOutput: Pipe?
    private var proxyStandardError: Pipe?
    private var outputCaptureProcess: Process?
    private var expectedTerminationProcess: Process?
    private var helperOutputBuffer = ""
    private var logWindow: NSWindow?

    func refresh() async {
        await refreshProxyStatus(updateLastMessage: true)
        await checkAuthStatus()
        await refreshClaudeSettingsPreview()
    }

    func refreshRuntimeStatus() async {
        await refreshProxyStatus(updateLastMessage: false)
    }

    private func refreshProxyStatus(updateLastMessage: Bool) async {
        do {
            let isHealthy = try await proxyIsHealthy()
            isRunning = isHealthy
            statusText = isHealthy
                ? "Running on 127.0.0.1:\(port)"
                : (isStartingProxy ? "Starting on 127.0.0.1:\(port)…" : "Stopped")
            if isHealthy {
                await refreshTransportStatus()
            } else if !isStartingProxy {
                applyStoppedTransportStatus()
            }
            if updateLastMessage {
                lastMessage = isHealthy
                    ? "Proxy is running."
                    : (isStartingProxy ? "Proxy is starting…" : "Proxy is stopped.")
            }
        } catch {
            isRunning = false
            statusText = isStartingProxy ? "Starting on 127.0.0.1:\(port)…" : "Stopped"
            if !isStartingProxy {
                applyStoppedTransportStatus()
            }
            if updateLastMessage {
                lastMessage = isStartingProxy ? "Proxy is starting…" : "Proxy is stopped."
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
        if let proxyProcess, proxyProcess.isRunning {
            lastMessage = "Proxy is already running."
            return
        }
        proxyProcess = nil
        isStartingProxy = true
        lastStartupFailure = nil
        statusText = "Preparing proxy…"
        lastMessage = "Checking whether the proxy can start…"
        defer { isStartingProxy = false }

        replaceCrossProviderModelDefaults()
        if provider == "custom-openai", customOpenAIBaseURL.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            lastMessage = "Enter a custom OpenAI endpoint URL before starting."
            recordAppEvent(level: "ERROR", message: "Proxy start rejected", detail: lastMessage)
            return
        }

        do {
            do {
                let sessionCheck = try await runCLI(["claude", "check-live-sessions"])
                recordAppEvent(
                    level: "INFO",
                    message: "Claude Code session preflight passed",
                    detail: sessionCheck.trimmingCharacters(in: .whitespacesAndNewlines)
                )
            } catch {
                let detail = error.localizedDescription.trimmingCharacters(in: .whitespacesAndNewlines)
                guard detail.localizedCaseInsensitiveContains("Claude Code is already running") else {
                    throw error
                }
                let message = "Close every Claude Code session, then start the proxy again."
                lastStartupFailure = detail.isEmpty ? message : detail
                lastMessage = message
                statusText = "Start blocked"
                recordAppEvent(level: "ERROR", message: "Proxy start blocked by Claude Code session preflight", detail: detail)
                showLiveClaudeSessionAlert(detail: detail)
                return
            }

            let executableURL = try helperURL()
            let process = Process()
            let standardOutput = Pipe()
            let standardError = Pipe()
            process.executableURL = executableURL
            process.arguments = serveArguments
            process.standardOutput = standardOutput
            process.standardError = standardError
            configureOutputCapture(for: process, standardOutput: standardOutput, standardError: standardError)

            recordAppEvent(
                level: "INFO",
                message: "Starting proxy helper",
                detail: "helper=\(executableURL.path) provider=\(provider) model=\(model) sonnet_model=\(sonnetModel) small_model=\(smallModel) port=\(port)"
            )
            try process.run()
            proxyProcess = process
            statusText = "Starting on 127.0.0.1:\(port)…"
            transportDetailText = "Waiting for the first Codex request."
            transportBadgeText = "Waiting"
            transportCurrentMethod = nil
            lastMessage = "Waiting for the proxy health check…"

            guard await waitForProxyHealth(process: process) else {
                let detail = helperOutputBuffer.trimmingCharacters(in: .whitespacesAndNewlines)
                if process.isRunning {
                    expectedTerminationProcess = process
                    process.terminate()
                }
                proxyProcess = nil
                isRunning = false
                statusText = "Failed to start"
                applyStoppedTransportStatus()
                lastStartupFailure = detail.isEmpty
                    ? "The helper did not pass its health check within 5 seconds."
                    : detail
                lastMessage = "Proxy failed to start. Open Logs for diagnostics."
                recordAppEvent(
                    level: "ERROR",
                    message: "Proxy helper failed its startup health check",
                    detail: lastStartupFailure
                )
                return
            }

            isRunning = true
            statusText = "Running on 127.0.0.1:\(port)"
            lastMessage = "Proxy started and passed its health check."
            recordAppEvent(
                level: "INFO",
                message: "Proxy startup health check passed",
                detail: "http://127.0.0.1:\(port)/healthz"
            )
            await setActiveRoute(updateLastMessage: false)
            await installClaudeShim(updateLastMessage: false)
            await refreshProxyStatus(updateLastMessage: false)
        } catch {
            let detail = error.localizedDescription.trimmingCharacters(in: .whitespacesAndNewlines)
            proxyStandardOutput?.fileHandleForReading.readabilityHandler = nil
            proxyStandardError?.fileHandleForReading.readabilityHandler = nil
            proxyStandardOutput = nil
            proxyStandardError = nil
            outputCaptureProcess = nil
            proxyProcess = nil
            isRunning = false
            statusText = "Failed to start"
            applyStoppedTransportStatus()
            lastStartupFailure = detail
            lastMessage = "Failed to start proxy: \(detail)"
            recordAppEvent(level: "ERROR", message: "Failed to launch proxy helper", detail: detail)
        }
    }

    func stopProxy() async {
        recordAppEvent(level: "INFO", message: "Stopping proxy helper")
        if let process = proxyProcess, process.isRunning {
            expectedTerminationProcess = process
            process.terminate()
        } else {
            proxyProcess = nil
        }
        isRunning = false
        isStartingProxy = false
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
        replaceCrossProviderModelDefaults()
        await checkAuthStatus()
        if isRunning {
            await setActiveRoute()
        } else {
            lastMessage = "Provider selected. It will apply when the proxy starts."
        }
        await refreshClaudeSettingsPreview()
        await refreshRuntimeStatus()
    }

    func applyUpstreamModelChange() async {
        replaceCrossProviderModelDefaults()
        if isRunning {
            await setActiveRoute()
        } else {
            lastMessage = "Model selected. It will apply when the proxy starts."
        }
        await refreshClaudeSettingsPreview()
        await installClaudeShim(updateLastMessage: false)
        await refreshRuntimeStatus()
    }

    private func setActiveRoute(updateLastMessage: Bool = true) async {
        do {
            let output = try await runCLI([
                "admin",
                "route",
                "set",
                provider,
                "--port",
                "\(port)",
                "--model",
                model,
                "--sonnet-model",
                sonnetModel,
                "--small-model",
                smallModel,
                "--context-window",
                "\(autoCompactWindow)"
            ])
            if updateLastMessage {
                lastMessage = activeRouteMessage(from: output)
            }
            await refreshProxyStatus(updateLastMessage: false)
        } catch {
            recordAppEvent(
                level: "ERROR",
                message: "Failed to update the active route",
                detail: error.localizedDescription
            )
            if updateLastMessage {
                lastMessage = "Provider switch failed: \(error.localizedDescription)"
            }
            await refreshProxyStatus(updateLastMessage: false)
        }
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
            recordAppEvent(
                level: "ERROR",
                message: "Failed to install the Claude command shim",
                detail: error.localizedDescription
            )
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
        expectedTerminationProcess = proxyProcess
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

    private func proxyIsHealthy() async throws -> Bool {
        var request = URLRequest(url: URL(string: "http://127.0.0.1:\(port)/healthz")!)
        request.timeoutInterval = 0.5
        let (_, response) = try await URLSession.shared.data(for: request)
        return (response as? HTTPURLResponse)?.statusCode == 200
    }

    private func waitForProxyHealth(process: Process) async -> Bool {
        for _ in 0..<50 {
            guard process.isRunning else { return false }
            if (try? await proxyIsHealthy()) == true {
                return true
            }
            try? await Task.sleep(nanoseconds: 100_000_000)
        }
        return false
    }

    private func configureOutputCapture(
        for process: Process,
        standardOutput: Pipe,
        standardError: Pipe
    ) {
        helperOutputBuffer = ""
        proxyStandardOutput = standardOutput
        proxyStandardError = standardError
        outputCaptureProcess = process

        standardOutput.fileHandleForReading.readabilityHandler = { [weak self] handle in
            let data = handle.availableData
            guard !data.isEmpty, let output = String(data: data, encoding: .utf8) else { return }
            Task { @MainActor [weak self] in
                self?.captureHelperOutput(output, stream: "stdout")
            }
        }
        standardError.fileHandleForReading.readabilityHandler = { [weak self] handle in
            let data = handle.availableData
            guard !data.isEmpty, let output = String(data: data, encoding: .utf8) else { return }
            Task { @MainActor [weak self] in
                self?.captureHelperOutput(output, stream: "stderr")
            }
        }
        process.terminationHandler = { [weak self] terminatedProcess in
            Task { @MainActor [weak self] in
                self?.handleProxyTermination(terminatedProcess)
            }
        }
    }

    private func captureHelperOutput(_ output: String, stream: String) {
        helperOutputBuffer += output
        if helperOutputBuffer.count > 24_000 {
            helperOutputBuffer = String(helperOutputBuffer.suffix(24_000))
        }

        for line in output.split(whereSeparator: \Character.isNewline) {
            let message = String(line).trimmingCharacters(in: .whitespacesAndNewlines)
            guard !message.isEmpty else { continue }
            recordAppEvent(
                level: stream == "stderr" ? "ERROR" : "INFO",
                message: "Proxy helper \(stream)",
                detail: message
            )
        }
    }

    private func handleProxyTermination(_ process: Process) {
        let wasExpected = expectedTerminationProcess === process
        if wasExpected {
            expectedTerminationProcess = nil
        }
        if outputCaptureProcess === process {
            proxyStandardOutput?.fileHandleForReading.readabilityHandler = nil
            proxyStandardError?.fileHandleForReading.readabilityHandler = nil
            proxyStandardOutput = nil
            proxyStandardError = nil
            outputCaptureProcess = nil
        }
        if proxyProcess === process {
            proxyProcess = nil
        }

        let reason = process.terminationReason == .uncaughtSignal ? "signal" : "exit"
        let summary = "status=\(process.terminationStatus) reason=\(reason)"
        if wasExpected {
            recordAppEvent(level: "INFO", message: "Proxy helper stopped", detail: summary)
            return
        }

        isRunning = false
        isStartingProxy = false
        statusText = "Proxy exited"
        applyStoppedTransportStatus()
        let output = helperOutputBuffer.trimmingCharacters(in: .whitespacesAndNewlines)
        lastStartupFailure = output.isEmpty ? summary : "\(summary)\n\(output)"
        lastMessage = "Proxy exited unexpectedly (\(summary)). Open Logs for diagnostics."
        recordAppEvent(
            level: "ERROR",
            message: "Proxy helper exited unexpectedly",
            detail: lastStartupFailure
        )
    }

    func openLogs() {
        Task { await refreshLogs() }

        if logWindow == nil {
            let rootView = LogViewerView().environmentObject(self)
            let window = NSWindow(
                contentRect: NSRect(x: 0, y: 0, width: 920, height: 620),
                styleMask: [.titled, .closable, .miniaturizable, .resizable],
                backing: .buffered,
                defer: false
            )
            window.title = "CC Codex Proxy Logs"
            window.contentViewController = NSHostingController(rootView: rootView)
            window.titlebarSeparatorStyle = .line
            window.isReleasedWhenClosed = false
            window.setFrameAutosaveName("CCCodexProxyLogViewer")
            window.center()
            logWindow = window
        }

        logWindow?.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    func refreshLogs() async {
        isRefreshingLogs = true
        defer { isRefreshingLogs = false }
        let appLogURL = appLogURL
        let proxyLogURL = proxyLogURL

        do {
            let entries = try await Task.detached(priority: .userInitiated) {
                try loadProxyLogEntries(appLogURL: appLogURL, proxyLogURL: proxyLogURL)
            }.value
            logEntries = entries
            logLoadError = nil
        } catch {
            logLoadError = error.localizedDescription
        }
    }

    func revealLogsInFinder() {
        ensureLogsDirectoryExists()
        let existingLogs = [appLogURL, proxyLogURL].filter {
            FileManager.default.fileExists(atPath: $0.path)
        }
        if existingLogs.isEmpty {
            NSWorkspace.shared.open(logsDirectoryURL)
        } else {
            NSWorkspace.shared.activateFileViewerSelecting(existingLogs)
        }
    }

    private var logsDirectoryURL: URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Logs/CCCodexProxy", isDirectory: true)
    }

    private var proxyLogURL: URL {
        logsDirectoryURL.appendingPathComponent("proxy.log")
    }

    private var appLogURL: URL {
        logsDirectoryURL.appendingPathComponent("app.log")
    }

    private func ensureLogsDirectoryExists() {
        try? FileManager.default.createDirectory(
            at: logsDirectoryURL,
            withIntermediateDirectories: true
        )
    }

    private func recordAppEvent(level: String, message: String, detail: String? = nil) {
        ensureLogsDirectoryExists()
        let timestamp = ISO8601DateFormatter().string(from: Date())
        var fields: [String: Any] = ["message": message]
        if let detail, !detail.isEmpty {
            fields["detail"] = detail
        }
        let object: [String: Any] = [
            "timestamp": timestamp,
            "level": level,
            "fields": fields,
            "target": "CCCodexProxy.app"
        ]

        guard var data = try? JSONSerialization.data(withJSONObject: object, options: [.sortedKeys]) else {
            return
        }
        data.append(0x0A)

        do {
            if !FileManager.default.fileExists(atPath: appLogURL.path) {
                FileManager.default.createFile(atPath: appLogURL.path, contents: nil)
            }
            let handle = try FileHandle(forWritingTo: appLogURL)
            try handle.seekToEnd()
            try handle.write(contentsOf: data)
            try handle.close()
        } catch {
            logLoadError = "Unable to write app log: \(error.localizedDescription)"
        }

        if let line = String(data: data, encoding: .utf8),
           let entry = ProxyLogEntry.parse(line: line, source: .app, sequence: logEntries.count) {
            logEntries.append(entry)
            if logEntries.count > 2_000 {
                logEntries.removeFirst(logEntries.count - 2_000)
            }
        }
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
            claudePublicPrimaryModel,
            "--small-model",
            claudePublicSmallModel,
            "--port",
            "\(port)",
            "--auto-compact-window",
            "\(autoCompactWindow)"
        ]
    }

    private var serveArguments: [String] {
        var arguments = [
            "serve",
            "--provider",
            provider,
            "--port",
            "\(port)",
            "--model",
            model,
            "--sonnet-model",
            sonnetModel,
            "--small-model",
            smallModel,
            "--context-window",
            "\(autoCompactWindow)"
        ]
        if provider == "custom-openai" {
            let trimmedBaseURL = customOpenAIBaseURL.trimmingCharacters(in: .whitespacesAndNewlines)
            if !trimmedBaseURL.isEmpty {
                arguments += ["--custom-openai-base-url", trimmedBaseURL]
            }
            arguments += ["--custom-openai-transport", customOpenAITransport]
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
            sonnetModel = "deepseek-v4-pro[1m]"
            smallModel = "deepseek-v4-flash"
            autoCompactWindow = 1_000_000
        } else if provider == "custom-openai" {
            model = defaultOpenAIPrimaryModel
            sonnetModel = defaultOpenAISonnetModel
            smallModel = defaultOpenAISmallModel
            autoCompactWindow = 372_000
        } else {
            model = defaultOpenAIPrimaryModel
            sonnetModel = defaultOpenAISonnetModel
            smallModel = defaultOpenAISmallModel
            autoCompactWindow = 372_000
        }
    }

    private func replaceCrossProviderModelDefaults() {
        let primary = model.trimmingCharacters(in: .whitespacesAndNewlines)
        let sonnet = sonnetModel.trimmingCharacters(in: .whitespacesAndNewlines)
        let small = smallModel.trimmingCharacters(in: .whitespacesAndNewlines)

        if provider == "deepseek" {
            if primary.hasPrefix("gpt-") {
                model = "deepseek-v4-pro[1m]"
            }
            if sonnet.hasPrefix("gpt-") {
                sonnetModel = "deepseek-v4-pro[1m]"
            }
            if small.hasPrefix("gpt-") {
                smallModel = "deepseek-v4-flash"
            }
            if autoCompactWindow == 372_000 || autoCompactWindow == 272_000 || autoCompactWindow == 128_000 {
                autoCompactWindow = 1_000_000
            }
        } else if provider == "custom-openai" {
            if primary.hasPrefix("deepseek-") {
                model = defaultOpenAIPrimaryModel
            }
            if sonnet.hasPrefix("deepseek-") {
                sonnetModel = defaultOpenAISonnetModel
            }
            if small.hasPrefix("deepseek-") {
                smallModel = defaultOpenAISmallModel
            }
            if autoCompactWindow == 272_000 || autoCompactWindow == 128_000 || autoCompactWindow == 1_000_000 {
                autoCompactWindow = 372_000
            }
        } else {
            if primary.hasPrefix("deepseek-") {
                model = defaultOpenAIPrimaryModel
            }
            if sonnet.hasPrefix("deepseek-") {
                sonnetModel = defaultOpenAISonnetModel
            }
            if small.hasPrefix("deepseek-") {
                smallModel = defaultOpenAISmallModel
            }
            if autoCompactWindow == 1_000_000 || autoCompactWindow == 128_000 || autoCompactWindow == 272_000 {
                autoCompactWindow = 372_000
            }
        }
    }

    private func successMessage(from output: String) -> String {
        if let storage = value(for: "Storage", in: output), !storage.isEmpty {
            return "OAuth login complete. Tokens saved in \(storage)."
        }
        return "OAuth login complete."
    }

    private func activeRouteMessage(from output: String) -> String {
        let data = Data(output.utf8)
        if let status = try? JSONDecoder().decode(ProxyAdminRouteUpdate.self, from: data) {
            return "Provider switched to \(displayName(forProvider: status.activeProvider))."
        }
        return "Provider switched."
    }

    private func displayName(forProvider value: String) -> String {
        switch value {
        case "codex":
            return "Codex"
        case "deepseek":
            return "DeepSeek"
        case "custom-openai":
            return "Custom OpenAI"
        default:
            return value.isEmpty ? "Unknown" : value
        }
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

struct ProxyAdminRouteUpdate: Decodable {
    let activeProvider: String
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
