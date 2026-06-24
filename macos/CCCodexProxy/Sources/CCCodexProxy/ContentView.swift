import SwiftUI

struct ContentView: View {
    @EnvironmentObject private var model: ProxyAppModel
    @State private var settingsPreviewTab = ClaudeSettingsPreviewTab.changes

    var body: some View {
        ScrollView {
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
        .frame(maxWidth: .infinity, maxHeight: .infinity)
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
                Text("Small Model")
                Spacer()
                TextField("Small Model", text: $model.smallModel)
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
            claudeSettingsStatus
            HStack {
                Button {
                    Task { await model.login() }
                } label: {
                    Label(model.isAuthenticated ? "Reconnect" : "Login", systemImage: "person.crop.circle.badge.checkmark")
                }
                .disabled(model.isLoggingIn || model.isCheckingAuthStatus)

                Button {
                    Task { await model.checkAuthStatus() }
                } label: {
                    Label("Check OAuth", systemImage: "key")
                }
                .disabled(model.isLoggingIn || model.isCheckingAuthStatus)
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
                Text(authStatusTitle)
                    .font(.subheadline.weight(.semibold))
                Text(authStatusDetail)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
            }

            Spacer()

            if model.isLoggingIn || model.isCheckingAuthStatus {
                ProgressView()
                    .controlSize(.small)
                    .accessibilityLabel(model.isLoggingIn ? "OAuth login in progress" : "Checking OAuth status")
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

    private var authStatusTitle: String {
        if model.isLoggingIn {
            return "OAuth in progress"
        }
        if model.isCheckingAuthStatus {
            return "Checking OAuth"
        }
        return model.authStatusText
    }

    private var authStatusDetail: String {
        if model.isLoggingIn {
            return "Complete the browser sign-in to finish."
        }
        if model.isCheckingAuthStatus {
            return "Reading the local auth file."
        }
        return model.authDetailText
    }

    private var claudeSettingsStatus: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                Label("Claude settings", systemImage: "slider.horizontal.3")
                    .font(.subheadline.weight(.semibold))
                Spacer()
                if model.isRefreshingClaudeSettings {
                    ProgressView()
                        .controlSize(.small)
                        .accessibilityLabel("Refreshing Claude settings preview")
                }
                Button {
                    Task { await model.refreshClaudeSettingsPreview() }
                } label: {
                    Image(systemName: "arrow.clockwise")
                }
                .buttonStyle(.borderless)
                .accessibilityLabel("Refresh Claude settings preview")
            }

            if let preview = model.claudeSettingsPreview {
                settingsSummary(preview)

                Picker("Settings preview", selection: $settingsPreviewTab) {
                    ForEach(ClaudeSettingsPreviewTab.allCases) { tab in
                        Text(tab.rawValue).tag(tab)
                    }
                }
                .pickerStyle(.segmented)

                settingsPreviewContent(preview)

                HStack {
                    Button {
                        Task { await model.installClaudeSettings() }
                    } label: {
                        Label("Install Settings", systemImage: "square.and.arrow.down")
                    }
                    .disabled(model.isInstallingClaudeSettings)

                    Button {
                        Task { await model.restoreClaudeSettings() }
                    } label: {
                        Label("Restore", systemImage: "arrow.uturn.backward")
                    }
                    .disabled(!preview.canRestore || model.isRestoringClaudeSettings)
                }
                .buttonStyle(.bordered)
            } else if let error = model.claudeSettingsPreviewError {
                Label(error, systemImage: "exclamationmark.triangle")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(3)
            } else {
                Label("Loading settings preview", systemImage: "hourglass")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
        .padding(10)
        .background(
            RoundedRectangle(cornerRadius: 8)
                .fill(Color.secondary.opacity(0.06))
        )
        .overlay(
            RoundedRectangle(cornerRadius: 8)
                .stroke(Color.secondary.opacity(0.20))
        )
    }

    private func settingsSummary(_ preview: ClaudeSettingsPreview) -> some View {
        VStack(alignment: .leading, spacing: 5) {
            settingsSummaryRow(
                title: "Current",
                value: preview.settingsExists ? "Existing settings.json" : "New settings.json",
                systemImage: "doc.text"
            )
            settingsSummaryRow(
                title: "Will apply",
                value: preview.changeSummary,
                systemImage: "plus.forwardslash.minus"
            )
            settingsSummaryRow(
                title: "Preserve",
                value: "Unmanaged settings",
                systemImage: "lock"
            )
            settingsSummaryRow(
                title: "Restore",
                value: preview.restoreSummary,
                systemImage: "arrow.uturn.backward"
            )
        }
        .font(.caption)
    }

    private func settingsSummaryRow(title: String, value: String, systemImage: String) -> some View {
        HStack(spacing: 6) {
            Image(systemName: systemImage)
                .frame(width: 14)
                .accessibilityHidden(true)
            Text(title)
                .foregroundStyle(.secondary)
            Spacer()
            Text(value)
                .lineLimit(1)
                .truncationMode(.middle)
        }
    }

    @ViewBuilder
    private func settingsPreviewContent(_ preview: ClaudeSettingsPreview) -> some View {
        switch settingsPreviewTab {
        case .changes:
            VStack(alignment: .leading, spacing: 6) {
                ForEach(preview.managedChanges) { change in
                    HStack(alignment: .top, spacing: 8) {
                        Image(systemName: settingsActionIcon(change.action))
                            .foregroundStyle(settingsActionColor(change.action))
                            .frame(width: 14)
                            .accessibilityHidden(true)
                        VStack(alignment: .leading, spacing: 1) {
                            Text("\(change.actionText) \(change.key)")
                                .font(.caption.monospaced().weight(.semibold))
                            Text(change.detailText)
                                .font(.caption2.monospaced())
                                .foregroundStyle(.secondary)
                                .lineLimit(2)
                        }
                        Spacer()
                    }
                }
            }
        case .current:
            codePreview(preview.currentSettings)
        case .proposed:
            codePreview(preview.proposedSettings)
        case .restore:
            if let restoreSettings = preview.restoreSettings {
                codePreview(restoreSettings)
            } else {
                Label("No backup is available to restore.", systemImage: "tray")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
    }

    private func codePreview(_ text: String) -> some View {
        ScrollView([.vertical, .horizontal]) {
            Text(text)
                .font(.caption2.monospaced())
                .frame(maxWidth: .infinity, alignment: .leading)
                .textSelection(.enabled)
                .padding(8)
        }
        .frame(maxHeight: 180)
        .background(
            RoundedRectangle(cornerRadius: 6)
                .fill(Color.secondary.opacity(0.08))
        )
        .overlay(
            RoundedRectangle(cornerRadius: 6)
                .stroke(Color.secondary.opacity(0.18))
        )
    }

    private func settingsActionIcon(_ action: ClaudeEnvAction) -> String {
        switch action {
        case .add:
            return "plus.circle.fill"
        case .change:
            return "pencil.circle.fill"
        case .keep:
            return "checkmark.circle.fill"
        }
    }

    private func settingsActionColor(_ action: ClaudeEnvAction) -> Color {
        switch action {
        case .add:
            return .green
        case .change:
            return .orange
        case .keep:
            return .secondary
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

private enum ClaudeSettingsPreviewTab: String, CaseIterable, Identifiable {
    case changes = "Diff"
    case current = "Current"
    case proposed = "Apply"
    case restore = "Restore"

    var id: Self { self }
}
