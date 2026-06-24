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
            authStatus
            HStack {
                Button {
                    Task { await model.login() }
                } label: {
                    Label(model.isAuthenticated ? "Reconnect" : "Login", systemImage: "person.crop.circle.badge.checkmark")
                }
                .disabled(model.isLoggingIn)
                Button {
                    Task { await model.installClaudeSettings() }
                } label: {
                    Label("Install Claude Settings", systemImage: "terminal")
                }
            }
            .buttonStyle(.bordered)
        }
    }

    private var authStatus: some View {
        HStack(alignment: .center, spacing: 10) {
            Image(systemName: model.isAuthenticated ? "checkmark.seal.fill" : "person.crop.circle.badge.exclamationmark")
                .font(.title3)
                .foregroundStyle(authStatusColor)
                .accessibilityHidden(true)

            VStack(alignment: .leading, spacing: 2) {
                Text(model.isLoggingIn ? "OAuth in progress" : model.authStatusText)
                    .font(.subheadline.weight(.semibold))
                Text(model.isLoggingIn ? "Complete the browser sign-in to finish." : model.authDetailText)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
            }

            Spacer()

            if model.isLoggingIn {
                ProgressView()
                    .controlSize(.small)
                    .accessibilityLabel("OAuth login in progress")
            } else {
                Text(model.isAuthenticated ? "OAuth OK" : "OAuth needed")
                    .font(.caption.weight(.semibold))
                    .padding(.horizontal, 8)
                    .padding(.vertical, 4)
                    .background(
                        Capsule()
                            .fill(authStatusColor.opacity(model.isAuthenticated ? 0.18 : 0.12))
                    )
            }
        }
        .padding(10)
        .background(
            RoundedRectangle(cornerRadius: 8)
                .fill(authStatusColor.opacity(model.isAuthenticated ? 0.10 : 0.06))
        )
        .overlay(
            RoundedRectangle(cornerRadius: 8)
                .stroke(authStatusColor.opacity(model.isAuthenticated ? 0.45 : 0.22))
        )
        .accessibilityElement(children: .combine)
        .accessibilityLabel(model.isAuthenticated ? "OAuth signed in" : "OAuth not signed in")
    }

    private var authStatusColor: Color {
        model.isAuthenticated ? .green : .secondary
    }

    private var footer: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(model.lastMessage)
                .font(.caption)
                .foregroundStyle(.secondary)
                .lineLimit(3)
            HStack {
                Button {
                    model.openProjectPage()
                } label: {
                    Label("Project", systemImage: "folder")
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
