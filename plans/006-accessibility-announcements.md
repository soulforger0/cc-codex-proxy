# 006 — Announce meaningful status and completion feedback

- **Status**: DONE
- **Commit**: 9d712bc
- **Severity**: MEDIUM
- **Category**: Accessibility / feedback
- **Estimated scope**: 3 files, medium change

## Problem

The app gives useful visual status, but dynamic outcomes are not explicitly announced. The combined header has a static accessibility label:

```swift
// macos/CCCodexProxy/Sources/CCCodexProxy/ContentView.swift:88 — current
.accessibilityElement(children: .combine)
.accessibilityLabel(model.isStartingProxy ? "CC Codex Proxy starting" : (model.isRunning ? "CC Codex Proxy running" : "CC Codex Proxy stopped"))
```

Operational messages appear visually at `ContentView.swift:174-193`, and Copy temporarily changes its label:

```swift
// macos/CCCodexProxy/Sources/CCCodexProxy/LogViewerView.swift:71 — current
Label(copied ? "Copied" : "Copy", systemImage: copied ? "checkmark" : "doc.on.doc")
```

```swift
// LogViewerView.swift:267 — current
copied = true
Task {
    try? await Task.sleep(nanoseconds: 1_400_000_000)
    copied = false
}
```

A VoiceOver user may not learn that startup completed, failed, a key was saved, settings were installed, or logs were copied unless focus happens to revisit the changed element.

## Target

Announce only meaningful causal events:

- Status: “Proxy started”, “Proxy stopped”, and “Proxy failed to start” after user-initiated operations.
- Completion: “Logs copied”, “API key saved”, “Settings installed”, “Settings restored”, and equivalent direct action outcomes.
- Error: concise actionable error summary, once.

Do not announce five-second background refreshes, every intermediate published value, or timer-driven reversion from “Copied” to “Copy”. Prefer SwiftUI accessibility announcements supported by the macOS 13 deployment target; if the modern SwiftUI API is unavailable, use `NSAccessibility.post(element:announcement:)` in a narrowly scoped helper.

## Repo conventions to follow

- Accessibility labels and hints are colocated with their controls throughout `ContentView` and `LogViewerView`.
- Model methods own the point at which an operation conclusively succeeds or fails.
- Visual feedback remains in the UI; announcements complement rather than replace it.

## Steps

1. Add a small main-actor announcement helper in an appropriate macOS UI file. It must accept a concise `String`, post through supported AppKit/SwiftUI accessibility APIs, and remain a no-op for empty text.
2. Do not announce generic `lastMessage` changes automatically; that property changes during background and multi-stage work.
3. Post a completion or error announcement at terminal branches of user-initiated `startProxy`, `stopProxy`, login/key-save, settings install/restore, and shim repair operations. Avoid announcing intermediate “checking” states unless an operation takes long enough that the visible `ProgressView` is the only feedback.
4. Ensure startup failure announces the concise recovery message, not an unbounded technical log dump.
5. In `copyVisibleLogs()`, announce “Logs copied” immediately after the pasteboard write succeeds. Do not announce when `copied` resets after 1.4 seconds.
6. If Refresh completes with no meaningful change, do not announce. If it produces a direct error initiated by the user, announce the concise error once.
7. Review the header and status cards so current state remains queryable when focused; announcements do not remove existing labels.
8. Add unit coverage around announcement decision logic by extracting pure event-to-message mapping if needed. Do not attempt to assert VoiceOver itself in unit tests.

## Boundaries

- Do NOT announce periodic runtime refreshes from `StatusItemController.swift:75-80`.
- Do NOT speak secrets, endpoint credentials, raw API keys, or long raw process output.
- Do NOT announce both an intermediate state and a terminal state in rapid succession unless each is essential.
- Do NOT move keyboard focus automatically to status messages.
- Do NOT add dependencies.
- If cited code has drifted since `9d712bc`, STOP and report.

## Verification

- **Mechanical**: `swift test --package-path macos/CCCodexProxy` must pass.
- **VoiceOver feel check**: with VoiceOver enabled, start and stop successfully, force a startup failure, save a key, install/restore settings, repair the shim, and copy logs. Each meaningful terminal event is announced exactly once in concise language.
- Leave the proxy running for at least 15 seconds; periodic refresh must not produce announcement noise.
- Confirm focus remains on the initiating control and that all current status remains discoverable by navigating normally.
- **Done when**: users receive timely status/completion/error feedback without repetitive background chatter or focus disruption.
