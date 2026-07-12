import AppKit
import SwiftUI

struct LogViewerView: View {
    @EnvironmentObject private var model: ProxyAppModel
    @Environment(\.accessibilityReduceTransparency) private var reduceTransparency
    @Environment(\.accessibilityDifferentiateWithoutColor) private var differentiateWithoutColor
    @State private var query = ""
    @State private var source: ProxyLogSource = .all
    @State private var level: LogLevelFilter = .all
    @State private var copied = false

    private var codeSurface: Color {
        reduceTransparency ? Color(nsColor: .textBackgroundColor) : AppTheme.codeSurface
    }

    private var filteredEntries: [ProxyLogEntry] {
        let normalizedQuery = query.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        return model.logEntries.reversed().filter { entry in
            let sourceMatches = source == .all || entry.source == source
            let levelMatches = level.matches(entry.level)
            let queryMatches = normalizedQuery.isEmpty || entry.searchableText.contains(normalizedQuery)
            return sourceMatches && levelMatches && queryMatches
        }
    }

    var body: some View {
        ZStack {
            AppTheme.background.ignoresSafeArea()

            VStack(spacing: 0) {
                header
                Divider()
                filters
                notices
                logList
                footer
            }
        }
        .frame(minWidth: 720, minHeight: 460)
        .task {
            await model.refreshLogs()
        }
    }

    private var header: some View {
        HStack(spacing: 12) {
            ZStack {
                RoundedRectangle(cornerRadius: AppTheme.radiusIconTile, style: .continuous)
                    .fill(AppTheme.subtleFill)
                Image(systemName: "doc.text.magnifyingglass")
                    .font(.system(size: 17, weight: .semibold))
                    .foregroundStyle(AppTheme.accent)
            }
            .frame(width: 38, height: 38)

            VStack(alignment: .leading, spacing: 2) {
                Text("Proxy logs")
                    .font(.headline.weight(.semibold))
                Text("Launcher diagnostics and proxy runtime events")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            Spacer()

            statusPill

            Button {
                Task { await model.refreshLogs() }
            } label: {
                Label("Refresh", systemImage: "arrow.clockwise")
            }
            .buttonStyle(AppPressButtonStyle(tint: AppTheme.accent, compact: true))
            .disabled(model.isRefreshingLogs)

            Button(action: copyVisibleLogs) {
                Label(copied ? "Copied" : "Copy", systemImage: copied ? "checkmark" : "doc.on.doc")
            }
            .buttonStyle(AppPressButtonStyle(tint: copied ? AppTheme.success : AppTheme.accent, compact: true))
            .disabled(filteredEntries.isEmpty)

            Button {
                model.revealLogsInFinder()
            } label: {
                Label("Reveal", systemImage: "folder")
            }
            .buttonStyle(AppPressButtonStyle(tint: AppTheme.accent, compact: true))
        }
        .padding(.horizontal, 18)
        .padding(.vertical, 14)
        .background(
            reduceTransparency
                ? AnyShapeStyle(Color(nsColor: .windowBackgroundColor))
                : AnyShapeStyle(.regularMaterial)
        )
    }

    private var statusPill: some View {
        HStack(spacing: 6) {
            Circle()
                .fill(statusColor)
                .frame(width: 7, height: 7)
            Text(statusText)
                .font(.caption.weight(.semibold))
        }
        .foregroundStyle(statusColor)
        .padding(.horizontal, 9)
        .padding(.vertical, 5)
        .background(Capsule().fill(AppTheme.subtleFill))
        .overlay(Capsule().stroke(statusColor.opacity(0.22), lineWidth: 1))
    }

    private var filters: some View {
        ViewThatFits(in: .horizontal) {
            filterRow
            VStack(alignment: .leading, spacing: 8) {
                searchField.frame(maxWidth: .infinity)
                HStack(spacing: 8) { sourcePicker; levelPicker }
            }
        }
        .padding(.horizontal, 18)
        .padding(.vertical, 12)
    }

    private var searchField: some View {
        HStack(spacing: 7) {
            Image(systemName: "magnifyingglass").foregroundStyle(.secondary)
            TextField("Search messages, details, or targets", text: $query).textFieldStyle(.plain)
            if !query.isEmpty {
                Button { query = "" } label: { Image(systemName: "xmark.circle.fill").foregroundStyle(.secondary) }
                    .buttonStyle(.plain).accessibilityLabel("Clear search")
            }
        }
        .padding(.horizontal, 10).frame(height: 30)
        .background(RoundedRectangle(cornerRadius: AppTheme.radiusInset, style: .continuous).fill(AppTheme.insetSurface))
        .overlay(RoundedRectangle(cornerRadius: AppTheme.radiusInset, style: .continuous).stroke(AppTheme.hairline, lineWidth: 1))
    }

    private var sourcePicker: some View {
        Picker("Source", selection: $source) { ForEach(ProxyLogSource.allCases) { source in Text(source.rawValue).tag(source) } }
            .pickerStyle(.segmented).frame(minWidth: 150, idealWidth: 190, maxWidth: .infinity)
    }

    private var levelPicker: some View {
        Picker("Level", selection: $level) { ForEach(LogLevelFilter.allCases) { level in Text(level.rawValue).tag(level) } }
            .pickerStyle(.segmented).frame(minWidth: 180, idealWidth: 255, maxWidth: .infinity)
    }

    private var filterRow: some View {
        HStack(spacing: 12) { searchField; sourcePicker; levelPicker }
    }

    @ViewBuilder
    private var notices: some View {
        if let failure = model.lastStartupFailure, !failure.isEmpty {
            notice(
                title: "Latest startup failure",
                detail: failure,
                systemImage: "exclamationmark.triangle.fill",
                tint: AppTheme.danger
            )
        }
        if let error = model.logLoadError, !error.isEmpty {
            notice(
                title: "Could not load every log entry",
                detail: error,
                systemImage: "exclamationmark.circle.fill",
                tint: AppTheme.warning
            )
        }
    }

    private func notice(title: String, detail: String, systemImage: String, tint: Color) -> some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: systemImage)
                .foregroundStyle(tint)
                .padding(.top, 1)
            VStack(alignment: .leading, spacing: 2) {
                Text(title)
                    .font(.caption.weight(.semibold))
                Text(detail)
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
                    .lineLimit(4)
                    .textSelection(.enabled)
            }
            Spacer()
        }
        .padding(10)
        .background(
            RoundedRectangle(cornerRadius: AppTheme.radiusControl, style: .continuous)
                .fill(tint.opacity(0.07))
        )
        .overlay(
            RoundedRectangle(cornerRadius: AppTheme.radiusControl, style: .continuous)
                .stroke(tint.opacity(0.18), lineWidth: 1)
        )
        .padding(.horizontal, 18)
        .padding(.bottom, 10)
    }

    private var logList: some View {
        Group {
            if model.isRefreshingLogs && model.logEntries.isEmpty {
                VStack(spacing: 10) {
                    ProgressView()
                    Text("Loading logs…")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if filteredEntries.isEmpty {
                VStack(spacing: 10) {
                    Image(systemName: query.isEmpty ? "tray" : "line.3.horizontal.decrease.circle")
                        .font(.system(size: 28))
                        .foregroundStyle(.tertiary)
                    Text(query.isEmpty ? "No log entries yet" : "No matching log entries")
                        .font(.subheadline.weight(.medium))
                    Text(query.isEmpty ? "Start the proxy to capture launcher and runtime diagnostics." : "Try a different search, source, or level.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                ScrollView {
                    LazyVStack(spacing: 0) {
                        ForEach(filteredEntries) { entry in
                            LogEntryRow(entry: entry)
                            Divider().padding(.leading, 102)
                        }
                    }
                    .padding(.horizontal, 8)
                }
            }
        }
        .background(codeSurface)
        .overlay(
            Rectangle()
                .stroke(AppTheme.hairline, lineWidth: 1)
        )
        .padding(.horizontal, 18)
    }

    private var footer: some View {
        HStack {
            Text("Showing \(filteredEntries.count) of \(model.logEntries.count) entries · newest first")
            Spacer()
            if model.isRefreshingLogs {
                ProgressView().controlSize(.small)
            }
            Text("app.log + proxy.log")
        }
        .font(.caption2)
        .foregroundStyle(.secondary)
        .padding(.horizontal, 20)
        .padding(.vertical, 9)
    }

    private var statusText: String {
        model.isStartingProxy ? "Starting" : (model.isRunning ? "Running" : "Stopped")
    }

    private var statusColor: Color {
        model.isStartingProxy ? AppTheme.accent : (model.isRunning ? AppTheme.success : AppTheme.muted)
    }

    private func copyVisibleLogs() {
        let text = filteredEntries
            .reversed()
            .map(\ProxyLogEntry.rawLine)
            .joined(separator: "\n")
        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        pasteboard.setString(text, forType: .string)
        announceAccessibility("Logs copied")
        copied = true
        Task {
            try? await Task.sleep(nanoseconds: 1_400_000_000)
            copied = false
        }
    }
}

private struct LogEntryRow: View {
    @Environment(\.accessibilityDifferentiateWithoutColor) private var differentiateWithoutColor
    let entry: ProxyLogEntry

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Text(entry.timestampLabel)
                .font(.caption.monospacedDigit())
                .foregroundStyle(.secondary)
                .frame(width: 62, alignment: .leading)

            HStack(spacing: 3) {
                if differentiateWithoutColor {
                    Image(systemName: levelSymbol)
                        .accessibilityHidden(true)
                }
                Text(entry.level)
            }
            .accessibilityElement(children: .combine)
            .accessibilityLabel(entry.level)
                .font(.caption2.monospaced().weight(.bold))
                .foregroundStyle(levelColor)
                .padding(.horizontal, 6)
                .padding(.vertical, 3)
                .background(Capsule().fill(levelColor.opacity(0.09)))
                .overlay(Capsule().stroke(levelColor.opacity(0.18), lineWidth: 1))
                .frame(width: 56, alignment: .leading)

            VStack(alignment: .leading, spacing: 4) {
                HStack(alignment: .firstTextBaseline, spacing: 7) {
                    Text(entry.message)
                        .font(.system(.caption, design: .monospaced).weight(.medium))
                        .textSelection(.enabled)
                    Text(entry.source.rawValue)
                        .font(.caption2.weight(.semibold))
                        .foregroundStyle(.secondary)
                        .padding(.horizontal, 5)
                        .padding(.vertical, 2)
                        .background(Capsule().fill(AppTheme.subtleFill))
                }
                if !entry.detail.isEmpty {
                    Text(entry.detail)
                        .font(.caption.monospaced())
                        .foregroundStyle(.secondary)
                        .lineLimit(8)
                        .textSelection(.enabled)
                }
                if !entry.target.isEmpty {
                    Text(entry.target)
                        .font(.caption2.monospaced())
                        .foregroundStyle(.tertiary)
                        .textSelection(.enabled)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 9)
    }

    private var levelSymbol: String {
        if entry.level.contains("ERROR") { return "exclamationmark.circle.fill" }
        if entry.level.contains("WARN") { return "exclamationmark.triangle.fill" }
        if entry.level.contains("DEBUG") || entry.level.contains("TRACE") { return "ladybug.fill" }
        return "info.circle.fill"
    }

    private var levelColor: Color {
        if entry.level.contains("ERROR") { return AppTheme.danger }
        if entry.level.contains("WARN") { return AppTheme.warning }
        if entry.level.contains("DEBUG") || entry.level.contains("TRACE") { return AppTheme.muted }
        return AppTheme.accent
    }
}

private enum LogLevelFilter: String, CaseIterable, Identifiable {
    case all = "All"
    case errors = "Errors"
    case warnings = "Warnings"
    case info = "Info"

    var id: Self { self }

    func matches(_ value: String) -> Bool {
        switch self {
        case .all:
            return true
        case .errors:
            return value.contains("ERROR")
        case .warnings:
            return value.contains("WARN")
        case .info:
            return !value.contains("ERROR") && !value.contains("WARN")
        }
    }
}
