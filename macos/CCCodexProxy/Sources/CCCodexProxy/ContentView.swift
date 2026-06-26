import SwiftUI

struct ContentView: View {
    @EnvironmentObject private var model: ProxyAppModel
    @State private var settingsPreviewTab = ClaudeSettingsPreviewTab.changes
    @State private var showAdvancedClaudeSettings = false

    var body: some View {
        ZStack {
            AppTheme.background
                .ignoresSafeArea()

            ScrollView {
                VStack(alignment: .leading, spacing: AppTheme.sectionSpacing) {
                    header
                    modelSelection
                    controls
                    settings
                    footer
                }
                .padding(AppTheme.outerPadding)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .animation(AppTheme.motion, value: model.isRunning)
        .animation(AppTheme.motion, value: model.isAuthenticated)
        .animation(AppTheme.motion, value: model.isLoggingIn)
        .animation(AppTheme.motion, value: model.isCheckingAuthStatus)
        .animation(AppTheme.motion, value: model.isInstallingClaudeShim)
        .animation(AppTheme.motion, value: model.isSavingDeepSeekAPIKey)
        .animation(AppTheme.motion, value: model.provider)
        .animation(AppTheme.motion, value: settingsPreviewTab)
        .animation(AppTheme.motion, value: showAdvancedClaudeSettings)
    }

    private var header: some View {
        HStack(alignment: .center, spacing: 14) {
            ZStack {
                Circle()
                    .fill(.thinMaterial)
                Circle()
                    .stroke(AppTheme.hairline, lineWidth: 1)
                Image(systemName: model.isRunning ? "bolt.horizontal.fill" : "bolt.horizontal")
                    .font(.system(size: 18, weight: .semibold))
                    .foregroundStyle(statusColor)
                    .accessibilityHidden(true)
            }
            .frame(width: 44, height: 44)

            VStack(alignment: .leading, spacing: 5) {
                HStack(alignment: .center, spacing: 8) {
                    Text("CC Codex Proxy")
                        .font(.headline.weight(.semibold))
                        .lineLimit(1)
                    Spacer(minLength: 8)
                    statusPill
                }
                HStack(alignment: .center, spacing: 8) {
                    Text(model.statusText)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Spacer(minLength: 8)
                    transportPill
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .padding(14)
        .background(
            RoundedRectangle(cornerRadius: AppTheme.largeRadius, style: .continuous)
                .fill(.regularMaterial)
        )
        .overlay(
            RoundedRectangle(cornerRadius: AppTheme.largeRadius, style: .continuous)
                .fill(AppTheme.panelHighlight)
                .blendMode(.plusLighter)
        )
        .overlay(
            RoundedRectangle(cornerRadius: AppTheme.largeRadius, style: .continuous)
                .stroke(AppTheme.hairline, lineWidth: 1)
        )
        .accessibilityElement(children: .combine)
        .accessibilityLabel(model.isRunning ? "CC Codex Proxy running" : "CC Codex Proxy stopped")
    }

    private var statusPill: some View {
        HStack(spacing: 5) {
            Circle()
                .fill(statusColor)
                .frame(width: 6, height: 6)
            Text(model.isRunning ? "Running" : "Stopped")
                .font(.caption2.weight(.semibold))
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 4)
        .background(Capsule().fill(AppTheme.subtleFill))
        .overlay(
            Capsule().stroke(statusColor.opacity(model.isRunning ? 0.28 : 0.16), lineWidth: 1)
        )
        .foregroundStyle(statusColor)
    }

    @ViewBuilder
    private var transportPill: some View {
        if model.isRunning, model.transportCurrentMethod != nil {
            Text(model.transportBadgeText)
                .font(.caption2.weight(.semibold))
                .lineLimit(1)
                .padding(.horizontal, 8)
                .padding(.vertical, 4)
                .background(Capsule().fill(AppTheme.subtleFill))
                .overlay(
                    Capsule().stroke(transportStatusColor.opacity(0.24), lineWidth: 1)
                )
                .foregroundStyle(transportStatusColor)
                .help(model.transportDetailText)
                .accessibilityLabel("Transport \(model.transportBadgeText)")
        }
    }

    private var controls: some View {
        VStack(alignment: .leading, spacing: 10) {
            sectionTitle("Proxy controls", systemImage: "switch.2")

            Grid(alignment: .leading, horizontalSpacing: 10, verticalSpacing: 10) {
                GridRow {
                    actionButton(
                        title: "Start",
                        detail: "Begin proxy",
                        systemImage: "play.fill",
                        tint: AppTheme.success,
                        isDisabled: model.isRunning
                    ) {
                        Task { await model.startProxy() }
                    }

                    actionButton(
                        title: "Stop",
                        detail: "End proxy",
                        systemImage: "stop.fill",
                        tint: AppTheme.danger,
                        isDisabled: !model.isRunning
                    ) {
                        Task { await model.stopProxy() }
                    }
                }
                GridRow {
                    actionButton(
                        title: "Refresh",
                        detail: "Check status",
                        systemImage: "arrow.clockwise",
                        tint: AppTheme.accent
                    ) {
                        Task { await model.refresh() }
                    }

                    actionButton(
                        title: "Logs",
                        detail: "Open file",
                        systemImage: "doc.text.magnifyingglass",
                        tint: AppTheme.accent
                    ) {
                        model.openLogs()
                    }
                }
            }
        }
    }

    private var modelSelection: some View {
        VStack(alignment: .leading, spacing: 12) {
            sectionTitle("Claude models", systemImage: "cpu")

            VStack(alignment: .leading, spacing: 8) {
                settingsInputRow(title: "Provider", detail: "Upstream API") {
                    Picker("Provider", selection: $model.provider) {
                        Text("Codex").tag("codex")
                        Text("DeepSeek").tag("deepseek")
                    }
                    .pickerStyle(.segmented)
                    .frame(width: 210)
                    .onChange(of: model.provider) { _ in
                        Task { await model.applyProviderChange() }
                    }
                }
                settingsInputRow(title: "Model", detail: "Primary model name passed to Claude Code") {
                    modelTextField("Model", text: $model.model)
                }
                settingsInputRow(title: "Small Model", detail: "Fast/compact model fallback") {
                    modelTextField("Small Model", text: $model.smallModel)
                }
            }
        }
        .panelCard()
    }

    private var settings: some View {
        VStack(alignment: .leading, spacing: 10) {
            authStatus
            claudeShimStatus
            DisclosureGroup(isExpanded: $showAdvancedClaudeSettings) {
                claudeSettingsStatus
                    .padding(.top, 8)
                    .transition(.opacity.combined(with: .move(edge: .top)))
            } label: {
                HStack(spacing: 8) {
                    Image(systemName: "doc.badge.gearshape")
                        .frame(width: 18)
                        .foregroundStyle(AppTheme.accent)
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Advanced settings.json")
                            .font(.subheadline.weight(.semibold))
                        Text("Preview, install, and restore managed Claude Code settings")
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                    }
                }
                .contentShape(Rectangle())
            }
            .padding(.horizontal, 2)
        }
    }

    private var authStatus: some View {
        VStack(alignment: .leading, spacing: 8) {
            statusCard(
                title: authStatusTitle,
                detail: authStatusDetail,
                systemImage: model.isAuthenticated ? "checkmark.seal.fill" : "person.crop.circle.badge.exclamationmark",
                tint: authStatusColor,
                accessibilityLabel: model.provider == "deepseek"
                    ? (model.isAuthenticated ? "DeepSeek API key saved" : "DeepSeek API key not saved")
                    : (model.isAuthenticated ? "OAuth signed in" : "OAuth not signed in")
            ) {
                authAccessory
            }

            if model.provider == "deepseek" {
                deepSeekKeyInput
            }
        }
    }

    @ViewBuilder
    private var authAccessory: some View {
        if model.isLoggingIn || model.isCheckingAuthStatus || model.isSavingDeepSeekAPIKey {
            ProgressView()
                .controlSize(.small)
                .accessibilityLabel(model.provider == "deepseek" ? "Checking DeepSeek API key" : "Checking OAuth status")
        } else if !model.isAuthenticated && model.provider == "codex" {
            Button {
                Task { await model.login() }
            } label: {
                Label("Login", systemImage: "person.crop.circle.badge.checkmark")
            }
            .buttonStyle(AppPressButtonStyle(tint: authStatusColor, compact: true))
            .accessibilityHint("Start ChatGPT OAuth login")
        } else {
            Text(model.provider == "deepseek"
                ? (model.isAuthenticated ? "Key OK" : "Key needed")
                : "OAuth OK")
                .font(.caption.weight(.semibold))
                .padding(.horizontal, 8)
                .padding(.vertical, 4)
                .background(Capsule().fill(AppTheme.subtleFill))
                .overlay(
                    Capsule().stroke(authStatusColor.opacity(0.24), lineWidth: 1)
                )
                .foregroundStyle(authStatusColor)
        }
    }

    private var deepSeekKeyInput: some View {
        HStack(alignment: .center, spacing: 10) {
            SecureField("DeepSeek API key", text: $model.deepSeekAPIKey)
                .textFieldStyle(.roundedBorder)
                .font(.system(.body, design: .monospaced))
                .onSubmit {
                    Task { await model.saveDeepSeekAPIKey() }
                }
            Button {
                Task { await model.saveDeepSeekAPIKey() }
            } label: {
                Label("Save Key", systemImage: "key.fill")
            }
            .buttonStyle(AppPressButtonStyle(tint: AppTheme.accent, compact: true))
            .disabled(model.deepSeekAPIKey.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || model.isSavingDeepSeekAPIKey)
        }
        .padding(10)
        .background(
            RoundedRectangle(cornerRadius: AppTheme.smallRadius, style: .continuous)
                .fill(AppTheme.insetSurface)
        )
        .overlay(
            RoundedRectangle(cornerRadius: AppTheme.smallRadius, style: .continuous)
                .stroke(AppTheme.hairline, lineWidth: 1)
        )
    }

    private var authStatusColor: Color {
        model.isAuthenticated ? AppTheme.success : AppTheme.warning
    }

    private var authStatusTitle: String {
        if model.isSavingDeepSeekAPIKey {
            return "Saving DeepSeek key"
        }
        if model.isLoggingIn {
            return "OAuth in progress"
        }
        if model.isCheckingAuthStatus {
            return "Checking OAuth"
        }
        return model.authStatusText
    }

    private var authStatusDetail: String {
        if model.isSavingDeepSeekAPIKey {
            return "Writing the local API key file."
        }
        if model.isLoggingIn {
            return "Complete the browser sign-in to finish."
        }
        if model.isCheckingAuthStatus {
            return model.provider == "deepseek" ? "Checking DeepSeek API key status." : "Reading the local auth file."
        }
        return model.authDetailText
    }

    private var transportStatusColor: Color {
        guard model.isRunning else {
            return AppTheme.muted
        }
        switch model.transportCurrentMethod {
        case "deepseek":
            return AppTheme.accent
        case "websocket":
            return AppTheme.success
        case "http-sse":
            return model.transportConfiguredMode == "auto" ? AppTheme.warning : AppTheme.accent
        default:
            return AppTheme.muted
        }
    }

    private var claudeShimStatus: some View {
        statusCard(
            title: "Claude command",
            detail: model.claudeShimStatusText,
            systemImage: "terminal",
            tint: AppTheme.accent,
            accessibilityLabel: "Claude command shim status"
        ) {
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
                .buttonStyle(AppPressButtonStyle(tint: AppTheme.accent, compact: true))
            }
        }
    }

    private var claudeSettingsStatus: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 8) {
                sectionTitle("Claude Code settings", systemImage: "slider.horizontal.3")
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
                .buttonStyle(AppPressButtonStyle(tint: AppTheme.accent, compact: true, iconOnly: true))
                .disabled(model.isRefreshingClaudeSettings)
                .help("Refresh Claude Code settings preview")
                .accessibilityLabel("Refresh Claude Code settings preview")
            }

            advancedSettingsInputs

            Group {
                if let preview = model.claudeSettingsPreview {
                    settingsSummary(preview)

                    Picker("Settings preview", selection: $settingsPreviewTab) {
                        ForEach(ClaudeSettingsPreviewTab.allCases) { tab in
                            Text(tab.rawValue).tag(tab)
                        }
                    }
                    .pickerStyle(.segmented)

                    settingsPreviewContent(preview)
                        .transition(.opacity)

                    HStack(spacing: 10) {
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
                    .buttonStyle(AppPressButtonStyle(tint: AppTheme.accent, compact: true))
                } else if let error = model.claudeSettingsPreviewError {
                    noticeLabel(error, systemImage: "exclamationmark.triangle", tint: AppTheme.warning)
                } else {
                    noticeLabel("Loading settings preview", systemImage: "hourglass", tint: AppTheme.muted)
                }
            }
        }
        .panelCard()
    }

    private var advancedSettingsInputs: some View {
        VStack(alignment: .leading, spacing: 8) {
            settingsInputRow(title: "Port", detail: "Local Anthropic-compatible endpoint") {
                TextField("Port", value: $model.port, formatter: NumberFormatter())
                    .textFieldStyle(.roundedBorder)
                    .frame(width: 96)
                    .onSubmit {
                        Task {
                            await model.refreshClaudeSettingsPreview()
                            await model.installClaudeShim()
                        }
                    }
            }
        }
    }

    private func modelTextField(_ title: String, text: Binding<String>) -> some View {
        TextField(title, text: text)
            .textFieldStyle(.roundedBorder)
            .font(.system(.body, design: .monospaced))
            .frame(width: 210)
            .onSubmit {
                Task {
                    await model.refreshClaudeSettingsPreview()
                    await model.installClaudeShim()
                }
            }
    }

    private func settingsInputRow<Content: View>(
        title: String,
        detail: String? = nil,
        @ViewBuilder content: () -> Content
    ) -> some View {
        HStack(alignment: .center, spacing: 12) {
            VStack(alignment: .leading, spacing: 2) {
                Text(title)
                    .font(.caption.weight(.semibold))
                if let detail {
                    Text(detail)
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
            }
            Spacer(minLength: 10)
            content()
        }
    }

    private func settingsSummary(_ preview: ClaudeSettingsPreview) -> some View {
        VStack(alignment: .leading, spacing: 6) {
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
        .padding(10)
        .background(
            RoundedRectangle(cornerRadius: AppTheme.smallRadius, style: .continuous)
                .fill(AppTheme.insetSurface)
        )
        .font(.caption)
    }

    private func settingsSummaryRow(title: String, value: String, systemImage: String) -> some View {
        HStack(spacing: 7) {
            Image(systemName: systemImage)
                .frame(width: 14)
                .foregroundStyle(.secondary)
                .accessibilityHidden(true)
            Text(title)
                .foregroundStyle(.secondary)
            Spacer()
            Text(value)
                .fontWeight(.medium)
                .lineLimit(1)
                .truncationMode(.middle)
        }
    }

    @ViewBuilder
    private func settingsPreviewContent(_ preview: ClaudeSettingsPreview) -> some View {
        switch settingsPreviewTab {
        case .changes:
            VStack(alignment: .leading, spacing: 7) {
                ForEach(preview.managedChanges) { change in
                    HStack(alignment: .top, spacing: 8) {
                        Image(systemName: settingsActionIcon(change.action))
                            .foregroundStyle(settingsActionColor(change.action))
                            .frame(width: 16)
                            .accessibilityHidden(true)
                        VStack(alignment: .leading, spacing: 2) {
                            Text("\(change.actionText) \(change.key)")
                                .font(.caption.monospaced().weight(.semibold))
                            Text(change.detailText)
                                .font(.caption2.monospaced())
                                .foregroundStyle(.secondary)
                                .lineLimit(2)
                        }
                        Spacer()
                    }
                    .padding(.vertical, 2)
                }
            }
            .padding(10)
            .background(
                RoundedRectangle(cornerRadius: AppTheme.smallRadius, style: .continuous)
                    .fill(AppTheme.insetSurface)
            )
        case .current:
            codePreview(preview.currentSettings)
        case .proposed:
            codePreview(preview.proposedSettings)
        case .restore:
            if let restoreSettings = preview.restoreSettings {
                codePreview(restoreSettings)
            } else {
                noticeLabel("No backup is available to restore.", systemImage: "tray", tint: AppTheme.muted)
            }
        }
    }

    private func codePreview(_ text: String) -> some View {
        ScrollView([.vertical, .horizontal]) {
            Text(text)
                .font(.caption2.monospaced())
                .frame(maxWidth: .infinity, alignment: .leading)
                .textSelection(.enabled)
                .padding(10)
        }
        .frame(maxHeight: 176)
        .background(
            RoundedRectangle(cornerRadius: AppTheme.smallRadius, style: .continuous)
                .fill(AppTheme.codeSurface)
        )
        .overlay(
            RoundedRectangle(cornerRadius: AppTheme.smallRadius, style: .continuous)
                .stroke(AppTheme.hairline, lineWidth: 1)
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
            return AppTheme.success
        case .change:
            return AppTheme.warning
        case .keep:
            return AppTheme.muted
        }
    }

    private var footer: some View {
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
        .padding(.horizontal, 2)
    }

    private func sectionTitle(_ title: String, systemImage: String) -> some View {
        Label(title, systemImage: systemImage)
            .font(.subheadline.weight(.semibold))
            .foregroundStyle(.primary)
    }

    private func actionButton(
        title: String,
        detail: String,
        systemImage: String,
        tint: Color,
        isDisabled: Bool = false,
        action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            HStack(spacing: 9) {
                Image(systemName: systemImage)
                    .font(.system(size: 14, weight: .semibold))
                    .frame(width: 20)
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 1) {
                    Text(title)
                        .font(.caption.weight(.semibold))
                    Text(detail)
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                }
                Spacer(minLength: 0)
            }
            .frame(maxWidth: .infinity, minHeight: 34, alignment: .leading)
        }
        .buttonStyle(AppPressButtonStyle(tint: tint))
        .disabled(isDisabled)
    }

    private func statusCard<Accessory: View>(
        title: String,
        detail: String,
        systemImage: String,
        tint: Color,
        accessibilityLabel: String,
        @ViewBuilder accessory: () -> Accessory
    ) -> some View {
        HStack(alignment: .center, spacing: 11) {
            Image(systemName: systemImage)
                .font(.title3)
                .symbolRenderingMode(.hierarchical)
                .foregroundStyle(tint)
                .frame(width: 24)
                .accessibilityHidden(true)

            VStack(alignment: .leading, spacing: 3) {
                Text(title)
                    .font(.subheadline.weight(.semibold))
                Text(detail)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
            }

            Spacer(minLength: 8)
            accessory()
        }
        .panelCard(tint: tint)
        .accessibilityElement(children: .contain)
        .accessibilityLabel(accessibilityLabel)
    }

    private func noticeLabel(_ text: String, systemImage: String, tint: Color) -> some View {
        Label(text, systemImage: systemImage)
            .font(.caption)
            .foregroundStyle(tint == AppTheme.muted ? Color.secondary : tint)
            .lineLimit(3)
            .padding(10)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(
                RoundedRectangle(cornerRadius: AppTheme.smallRadius, style: .continuous)
                    .fill(AppTheme.insetSurface)
            )
            .overlay(
                RoundedRectangle(cornerRadius: AppTheme.smallRadius, style: .continuous)
                    .stroke(tint == AppTheme.muted ? AppTheme.hairline : tint.opacity(0.18), lineWidth: 1)
            )
    }

    private var statusColor: Color {
        model.isRunning ? AppTheme.success : AppTheme.muted
    }
}

private enum AppTheme {
    static let outerPadding: CGFloat = 16
    static let sectionSpacing: CGFloat = 14
    static let largeRadius: CGFloat = 14
    static let cardRadius: CGFloat = 12
    static let smallRadius: CGFloat = 8
    static let motion = Animation.easeOut(duration: 0.18)

    static let accent = Color(nsColor: .controlAccentColor)
    static let success = Color(nsColor: .systemGreen)
    static let warning = Color(nsColor: .systemOrange)
    static let danger = Color(nsColor: .systemRed)
    static let muted = Color.secondary

    static let hairline = Color.primary.opacity(0.10)
    static let subtleFill = Color.primary.opacity(0.045)
    static let pressedFill = Color.primary.opacity(0.075)
    static let disabledFill = Color.primary.opacity(0.028)
    static let panelHighlight = LinearGradient(
        colors: [Color.white.opacity(0.12), Color.white.opacity(0.02)],
        startPoint: .topLeading,
        endPoint: .bottomTrailing
    )
    static let insetSurface = Color(nsColor: .textBackgroundColor).opacity(0.46)
    static let codeSurface = Color(nsColor: .textBackgroundColor).opacity(0.64)

    static var background: some View {
        ZStack {
            Color(nsColor: .windowBackgroundColor)
            LinearGradient(
                colors: [
                    Color.white.opacity(0.08),
                    Color(nsColor: .underPageBackgroundColor).opacity(0.28)
                ],
                startPoint: .top,
                endPoint: .bottom
            )
        }
    }
}

private struct PanelCard: ViewModifier {
    let tint: Color?

    func body(content: Content) -> some View {
        content
            .padding(12)
            .background(
                RoundedRectangle(cornerRadius: AppTheme.cardRadius, style: .continuous)
                    .fill(.regularMaterial)
            )
            .overlay(
                RoundedRectangle(cornerRadius: AppTheme.cardRadius, style: .continuous)
                    .fill(AppTheme.panelHighlight)
                    .blendMode(.plusLighter)
                    .allowsHitTesting(false)
            )
            .overlay(
                RoundedRectangle(cornerRadius: AppTheme.cardRadius, style: .continuous)
                    .stroke(AppTheme.hairline, lineWidth: 1)
                    .allowsHitTesting(false)
            )
            .overlay(
                RoundedRectangle(cornerRadius: AppTheme.cardRadius, style: .continuous)
                    .stroke((tint ?? Color.clear).opacity(tint == nil ? 0 : 0.14), lineWidth: 1)
                    .allowsHitTesting(false)
            )
            .shadow(color: Color.black.opacity(0.05), radius: 12, x: 0, y: 6)
    }
}

private extension View {
    func panelCard(tint: Color? = nil) -> some View {
        modifier(PanelCard(tint: tint))
    }
}

private struct AppPressButtonStyle: ButtonStyle {
    @Environment(\.isEnabled) private var isEnabled

    let tint: Color
    var compact = false
    var iconOnly = false

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .padding(.horizontal, iconOnly ? 0 : (compact ? 10 : 11))
            .padding(.vertical, iconOnly ? 0 : (compact ? 5 : 7))
            .frame(width: iconOnly ? 28 : nil, height: iconOnly ? 28 : nil)
            .background(
                RoundedRectangle(cornerRadius: compact || iconOnly ? 7 : 9, style: .continuous)
                    .fill(backgroundFill(isPressed: configuration.isPressed))
            )
            .overlay(
                RoundedRectangle(cornerRadius: compact || iconOnly ? 7 : 9, style: .continuous)
                    .stroke(tint.opacity(isEnabled ? 0.22 : 0.10), lineWidth: 1)
            )
            .foregroundStyle(tint.opacity(isEnabled ? 0.92 : 0.42))
            .scaleEffect(configuration.isPressed && isEnabled ? 0.97 : 1)
            .animation(.easeOut(duration: 0.12), value: configuration.isPressed)
            .animation(.easeOut(duration: 0.12), value: isEnabled)
    }

    private func backgroundFill(isPressed: Bool) -> Color {
        if !isEnabled {
            return AppTheme.disabledFill
        }
        return isPressed ? AppTheme.pressedFill : AppTheme.subtleFill
    }
}

private enum ClaudeSettingsPreviewTab: String, CaseIterable, Identifiable {
    case changes = "Diff"
    case current = "Current"
    case proposed = "After"
    case restore = "Restore"

    var id: Self { self }
}
