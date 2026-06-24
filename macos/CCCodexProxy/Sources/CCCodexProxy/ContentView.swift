import SwiftUI

struct ContentView: View {
    @EnvironmentObject private var model: ProxyAppModel
    @State private var settingsPreviewTab = ClaudeSettingsPreviewTab.changes
    @State private var showAdvancedClaudeSettings = false

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                header
                Divider()
                modelSelection
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

    private var modelSelection: some View {
        VStack(alignment: .leading, spacing: 8) {
            Label("Claude models", systemImage: "cpu")
                .font(.subheadline.weight(.semibold))

            VStack(alignment: .leading, spacing: 6) {
                settingsInputRow(title: "Model") {
                    TextField("Model", text: $model.model)
                        .textFieldStyle(.roundedBorder)
                        .frame(width: 180)
                        .onSubmit {
                            Task {
                                await model.refreshClaudeSettingsPreview()
                                await model.installClaudeShim()
                            }
                        }
                }
                settingsInputRow(title: "Small Model") {
                    TextField("Small Model", text: $model.smallModel)
                        .textFieldStyle(.roundedBorder)
                        .frame(width: 180)
                        .onSubmit {
                            Task {
                                await model.refreshClaudeSettingsPreview()
                                await model.installClaudeShim()
                            }
                        }
                }
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

    private var settings: some View {
        VStack(alignment: .leading, spacing: 10) {
            authStatus
            claudeShimStatus
            DisclosureGroup(isExpanded: $showAdvancedClaudeSettings) {
                claudeSettingsStatus
                    .padding(.top, 6)
            } label: {
                Label("Advanced settings.json", systemImage: "doc.badge.gearshape")
                    .font(.subheadline.weight(.semibold))
            }
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
            } else if !model.isAuthenticated {
                Button {
                    Task { await model.login() }
                } label: {
                    Label("Login", systemImage: "person.crop.circle.badge.checkmark")
                }
                .buttonStyle(.bordered)
                .accessibilityHint("Start ChatGPT OAuth login")
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
        .accessibilityElement(children: .contain)
        .accessibilityLabel(model.isAuthenticated ? "OAuth signed in" : "OAuth not signed in")
    }

    private var authStatusColor: Color {
        model.isAuthenticated ? .green : .orange
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

    private var claudeShimStatus: some View {
        HStack(alignment: .center, spacing: 10) {
            Image(systemName: "terminal")
                .font(.title3)
                .foregroundStyle(.blue)
                .accessibilityHidden(true)

            VStack(alignment: .leading, spacing: 2) {
                Text("Claude command")
                    .font(.subheadline.weight(.semibold))
                Text(model.claudeShimStatusText)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
            }

            Spacer()

            if model.isInstallingClaudeShim {
                ProgressView()
                    .controlSize(.small)
                    .accessibilityLabel("Installing Claude command shim")
            } else {
                Button {
                    Task { await model.installClaudeShim() }
                } label: {
                    Label("Repair", systemImage: "wrench.adjustable")
                }
                .buttonStyle(.bordered)
            }
        }
        .padding(10)
        .background(
            RoundedRectangle(cornerRadius: 8)
                .fill(Color.blue.opacity(0.06))
        )
        .overlay(
            RoundedRectangle(cornerRadius: 8)
                .stroke(Color.blue.opacity(0.22))
        )
    }

    private var claudeSettingsStatus: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                Label("Claude Code settings", systemImage: "slider.horizontal.3")
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
                .disabled(model.isRefreshingClaudeSettings)
                .frame(width: 28, height: 28)
                .help("Refresh Claude Code settings preview")
                .accessibilityLabel("Refresh Claude Code settings preview")
            }

            advancedSettingsInputs

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

    private var advancedSettingsInputs: some View {
        VStack(alignment: .leading, spacing: 6) {
            settingsInputRow(title: "Port") {
                TextField("Port", value: $model.port, formatter: NumberFormatter())
                    .textFieldStyle(.roundedBorder)
                    .frame(width: 90)
                    .onSubmit {
                        Task {
                            await model.refreshClaudeSettingsPreview()
                            await model.installClaudeShim()
                        }
                    }
            }
        }
    }

    private func settingsInputRow<Content: View>(
        title: String,
        @ViewBuilder content: () -> Content
    ) -> some View {
        HStack {
            Text(title)
                .font(.caption)
                .foregroundStyle(.secondary)
            Spacer()
            content()
        }
    }

    private func settingsSummary(_ preview: ClaudeSettingsPreview) -> some View {
        VStack(alignment: .leading, spacing: 5) {
            settingsSummaryRow(
                title: "Current",
                value: preview.settingsExists ? "Existing settings.json" : "New settings.json",
                systemImage: "doc.text"
            )
            settingsSummaryRow(
                title: "After",
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
                Button {
                    NSApplication.shared.terminate(nil)
                } label: {
                    Label("Quit", systemImage: "power")
                }
            }
            .buttonStyle(.borderless)
        }
    }
}

private enum ClaudeSettingsPreviewTab: String, CaseIterable, Identifiable {
    case changes = "Diff"
    case current = "Current"
    case proposed = "After"
    case restore = "Restore"

    var id: Self { self }
}
