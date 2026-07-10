import Foundation

enum ProxyLogSource: String, CaseIterable, Identifiable, Sendable {
    case all = "All"
    case app = "App"
    case proxy = "Proxy"

    var id: Self { self }
}

struct ProxyLogEntry: Identifiable, Sendable {
    let id: String
    let timestamp: Date?
    let timestampText: String
    let level: String
    let message: String
    let detail: String
    let target: String
    let source: ProxyLogSource
    let rawLine: String
    let sequence: Int

    var timestampLabel: String {
        guard timestampText.count >= 19 else { return "--:--:--" }
        let start = timestampText.index(timestampText.startIndex, offsetBy: 11)
        let end = timestampText.index(start, offsetBy: 8)
        return String(timestampText[start..<end])
    }

    var searchableText: String {
        [level, message, detail, target, source.rawValue]
            .joined(separator: " ")
            .lowercased()
    }

    static func parse(line: String, source: ProxyLogSource, sequence: Int) -> ProxyLogEntry? {
        let rawLine = line.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !rawLine.isEmpty else { return nil }

        guard
            let data = rawLine.data(using: .utf8),
            let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else {
            return ProxyLogEntry(
                id: "\(source.rawValue)-\(sequence)-raw",
                timestamp: nil,
                timestampText: "",
                level: "INFO",
                message: rawLine,
                detail: "",
                target: "",
                source: source,
                rawLine: rawLine,
                sequence: sequence
            )
        }

        let timestampText = stringValue(object["timestamp"])
        let timestamp = parseISO8601Date(timestampText)
        let level = stringValue(object["level"]).uppercased()
        let target = stringValue(object["target"])
        let fields = object["fields"] as? [String: Any] ?? [:]
        let message = stringValue(fields["message"]).isEmpty
            ? (target.isEmpty ? "Log event" : target)
            : stringValue(fields["message"])

        var details: [String] = []
        if let detail = fields["detail"] {
            let text = stringValue(detail)
            if !text.isEmpty {
                details.append(text)
            }
        }
        let metadata = fields
            .filter { $0.key != "message" && $0.key != "detail" }
            .sorted { $0.key < $1.key }
            .map { "\($0.key)=\(stringValue($0.value))" }
        details.append(contentsOf: metadata)

        return ProxyLogEntry(
            id: "\(source.rawValue)-\(sequence)-\(timestampText)",
            timestamp: timestamp,
            timestampText: timestampText,
            level: level.isEmpty ? "INFO" : level,
            message: message,
            detail: details.joined(separator: "\n"),
            target: target,
            source: source,
            rawLine: rawLine,
            sequence: sequence
        )
    }
}

func loadProxyLogEntries(appLogURL: URL, proxyLogURL: URL) throws -> [ProxyLogEntry] {
    let appEntries = try readTailLines(at: appLogURL).enumerated().compactMap { index, line in
        ProxyLogEntry.parse(line: line, source: .app, sequence: index)
    }
    let proxyEntries = try readTailLines(at: proxyLogURL).enumerated().compactMap { index, line in
        ProxyLogEntry.parse(line: line, source: .proxy, sequence: index)
    }

    return (appEntries + proxyEntries).sorted { lhs, rhs in
        switch (lhs.timestamp, rhs.timestamp) {
        case let (left?, right?):
            if left != right { return left < right }
            if lhs.source != rhs.source { return lhs.source.rawValue < rhs.source.rawValue }
            return lhs.sequence < rhs.sequence
        case (nil, nil):
            if lhs.source != rhs.source { return lhs.source.rawValue < rhs.source.rawValue }
            return lhs.sequence < rhs.sequence
        case (nil, _?):
            return true
        case (_?, nil):
            return false
        }
    }
}

func readTailLines(at url: URL, maxBytes: UInt64 = 1_048_576, maxLines: Int = 1_500) throws -> [String] {
    guard FileManager.default.fileExists(atPath: url.path) else { return [] }
    let handle = try FileHandle(forReadingFrom: url)
    defer { try? handle.close() }

    let size = try handle.seekToEnd()
    let offset = size > maxBytes ? size - maxBytes : 0
    try handle.seek(toOffset: offset)
    let data = try handle.readToEnd() ?? Data()
    guard var text = String(data: data, encoding: .utf8) else { return [] }

    if offset > 0, let newline = text.firstIndex(of: "\n") {
        text.removeSubrange(text.startIndex...newline)
    }
    return text
        .split(whereSeparator: \Character.isNewline)
        .suffix(maxLines)
        .map(String.init)
}

private func stringValue(_ value: Any?) -> String {
    switch value {
    case let string as String:
        return string
    case let number as NSNumber:
        return number.stringValue
    case .none:
        return ""
    default:
        return String(describing: value!)
    }
}

private func parseISO8601Date(_ value: String) -> Date? {
    guard !value.isEmpty else { return nil }
    let fractional = ISO8601DateFormatter()
    fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
    if let date = fractional.date(from: value) {
        return date
    }
    return ISO8601DateFormatter().date(from: value)
}
