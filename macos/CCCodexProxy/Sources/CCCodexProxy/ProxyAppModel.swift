import Foundation
import SwiftUI

@MainActor
final class ProxyAppModel: ObservableObject {
    @Published var isRunning = false
    @Published var statusText = "Not checked"
    @Published var lastMessage = ""
    @Published var model = "gpt-5.4[1m]"
    @Published var port = 18765

    private var proxyProcess: Process?

    func refresh() async {
        do {
            let output = try await runCLI(["admin", "status"], allowFailure: true)
            if output.contains("\"ok\":true") || output.contains("\"ok\": true") {
                isRunning = true
                statusText = "Running on 127.0.0.1:\(port)"
            } else {
                isRunning = false
                statusText = "Stopped"
            }
            lastMessage = output.trimmingCharacters(in: .whitespacesAndNewlines)
        } catch {
            isRunning = false
            statusText = "Stopped"
            lastMessage = error.localizedDescription
        }
    }

    func startProxy() async {
        guard proxyProcess == nil else { return }
        let process = Process()
        process.executableURL = cliURL()
        process.arguments = ["serve", "--port", "\(port)"]
        do {
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
        do {
            lastMessage = try await runCLI(["auth", "login"])
            await refresh()
        } catch {
            lastMessage = "Login failed: \(error.localizedDescription)"
        }
    }

    func installClaudeSettings() async {
        do {
            lastMessage = try await runCLI([
                "claude",
                "install-settings",
                "--model",
                model,
                "--small-model",
                "gpt-5.4-mini[1m]",
                "--port",
                "\(port)"
            ])
        } catch {
            lastMessage = "Settings install failed: \(error.localizedDescription)"
        }
    }

    func openLogs() {
        let url = FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Logs/CCCodexProxy/proxy.log")
        NSWorkspace.shared.open(url)
    }

    func openHomebrewInstructions() {
        NSWorkspace.shared.open(URL(string: "https://github.com/soulforger0/cc-codex-proxy")!)
    }

    private func runCLI(_ arguments: [String], allowFailure: Bool = false) async throws -> String {
        try await Task.detached(priority: .userInitiated) {
            let process = Process()
            let pipe = Pipe()
            process.executableURL = cliURL()
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
}

private func cliURL() -> URL {
    if let bundled = Bundle.main.url(forResource: "cc-codex-proxy", withExtension: nil) {
        return bundled
    }
    return URL(fileURLWithPath: "/opt/homebrew/bin/cc-codex-proxy")
}

enum ProxyAppError: LocalizedError {
    case commandFailed(String)

    var errorDescription: String? {
        switch self {
        case .commandFailed(let output):
            return output.isEmpty ? "Command failed" : output
        }
    }
}

