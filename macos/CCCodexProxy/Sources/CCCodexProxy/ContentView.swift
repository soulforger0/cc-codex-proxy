import SwiftUI

struct ContentView: View {
    @EnvironmentObject private var model: ProxyAppModel

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            header
            Divider()
            controls
            Divider()
            settings
            Divider()
            footer
        }
        .padding(16)
    }

    private var header: some View {
        HStack {
            VStack(alignment: .leading, spacing: 4) {
                Text("CC Codex Proxy")
                    .font(.headline)
                Text(model.statusText)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Spacer()
            Circle()
                .fill(model.isRunning ? Color.green : Color.gray)
                .frame(width: 10, height: 10)
                .accessibilityLabel(model.isRunning ? "Running" : "Stopped")
        }
    }

    private var controls: some View {
        Grid(alignment: .leading, horizontalSpacing: 10, verticalSpacing: 10) {
            GridRow {
                Button {
                    Task { await model.startProxy() }
                } label: {
                    Label("Start", systemImage: "play.fill")
                }
                .disabled(model.isRunning)

                Button {
                    Task { await model.stopProxy() }
                } label: {
                    Label("Stop", systemImage: "stop.fill")
                }
                .disabled(!model.isRunning)
            }
            GridRow {
                Button {
                    Task { await model.refresh() }
                } label: {
                    Label("Refresh", systemImage: "arrow.clockwise")
                }
                Button {
                    model.openLogs()
                } label: {
                    Label("Logs", systemImage: "doc.text.magnifyingglass")
                }
            }
        }
        .buttonStyle(.bordered)
    }

    private var settings: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text("Model")
                Spacer()
                TextField("Model", text: $model.model)
                    .textFieldStyle(.roundedBorder)
                    .frame(width: 160)
            }
            HStack {
                Text("Port")
                Spacer()
                TextField("Port", value: $model.port, formatter: NumberFormatter())
                    .textFieldStyle(.roundedBorder)
                    .frame(width: 90)
            }
            HStack {
                Button {
                    Task { await model.login() }
                } label: {
                    Label("Login", systemImage: "person.crop.circle.badge.checkmark")
                }
                Button {
                    Task { await model.installClaudeSettings() }
                } label: {
                    Label("Install Claude Settings", systemImage: "terminal")
                }
            }
            .buttonStyle(.bordered)
        }
    }

    private var footer: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(model.lastMessage)
                .font(.caption)
                .foregroundStyle(.secondary)
                .lineLimit(3)
            HStack {
                Button {
                    model.openHomebrewInstructions()
                } label: {
                    Label("Homebrew", systemImage: "shippingbox")
                }
                Spacer()
                Button("Quit") {
                    NSApplication.shared.terminate(nil)
                }
            }
            .buttonStyle(.borderless)
        }
    }
}

