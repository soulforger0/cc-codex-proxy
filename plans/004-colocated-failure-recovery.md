# 004 — Put recovery actions beside startup failures

- **Status**: DONE
- **Commit**: 9d712bc
- **Severity**: MEDIUM
- **Category**: Feedback / wayfinding
- **Estimated scope**: 2 files, medium change

## Problem

The main controls render operational feedback as a passive label:

```swift
// macos/CCCodexProxy/Sources/CCCodexProxy/ContentView.swift:174 — current
if !model.lastMessage.isEmpty {
    Label(
        model.lastMessage,
        systemImage: model.lastStartupFailure == nil ? "info.circle" : "exclamationmark.triangle.fill"
    )
    // styling only
}
```

A health-check failure tells the user to open Logs, but does not provide a colocated action:

```swift
// macos/CCCodexProxy/Sources/CCCodexProxy/ProxyAppModel.swift:180 — current
lastStartupFailure = detail.isEmpty
    ? "The helper did not pass its health check within 5 seconds."
    : detail
lastMessage = "Proxy failed to start. Open Logs for diagnostics."
```

The failure detail appears again in a separate Logs window, requiring extra wayfinding at the moment recovery is needed.

## Target

Keep ordinary informational messages lightweight. When `lastStartupFailure` is non-nil, replace the passive label with a dedicated failure card that contains:

- Title: **Proxy failed to start**
- A concise user-facing next step from `lastMessage`
- Selectable technical detail from `lastStartupFailure`, with a reasonable line limit in the popover
- Primary action: **Try Again** (`arrow.clockwise`), disabled while starting
- Secondary action: **Open Logs** (`doc.text.magnifyingglass`)

Use the existing danger tint, inset/material vocabulary, and shared press button style. Do not add a modal confirmation. Recovery must be one click away and remain inside the current context.

## Repo conventions to follow

- `statusCard` at `ContentView.swift:807-838` establishes icon/title/detail/accessory grouping.
- `PanelCard` and `AppPressButtonStyle` are the shared surface/control vocabulary.
- `model.openLogs()` is already wired at `ContentView.swift:163-170`.
- Technical diagnostic text elsewhere is selectable and monospaced, e.g. `LogViewerView.swift:173-186`.

## Steps

1. Extract the current message block from `controls` into a `controlMessage` view.
2. When `model.lastStartupFailure == nil`, preserve the current compact informational treatment with no new actions.
3. When a failure exists, render a danger-tinted card with the exact title, concise message, and selectable monospaced detail described above.
4. Add **Try Again** to call `Task { await model.startProxy() }`; disable it when running or starting. Rely on the model guard from plan 001 as the correctness boundary.
5. Add **Open Logs** to call `model.openLogs()` immediately.
6. Ensure long failure detail expands to no more than four lines in the popover, truncates at the tail or middle as appropriate, and remains selectable. Provide a help/tooltip with full detail if truncation prevents inspection.
7. Keep the existing Logs control in the normal action grid; the recovery action is contextual duplication, not a replacement.
8. If starting again succeeds, clear the failure through the existing `startProxy()` behavior and return to the standard informational state without a custom celebratory animation.

## Boundaries

- Do NOT show a modal alert for ordinary startup failures. The existing live-session alert is a separate, deliberate blocked-start flow.
- Do NOT expose secrets or API keys in failure detail.
- Do NOT remove the Logs-window failure notice.
- Do NOT add a dismiss button that hides unresolved failure state.
- Do NOT add dependencies.
- If cited code has drifted since `9d712bc`, STOP and report.

## Verification

- **Mechanical**: `swift test --package-path macos/CCCodexProxy` must pass.
- **Feel check**: force a startup health-check failure. The failure card must be the most visually salient item inside Proxy controls without overwhelming the entire popover. Click Open Logs and confirm the diagnostics window opens. Fix the cause, click Try Again, and confirm the card clears after success.
- **Keyboard check**: tab to both recovery actions and activate them with Space/Return.
- **VoiceOver check**: the failure title, next step, technical detail, and both actions are read in a sensible order.
- **Done when**: users can understand and act on startup failure without searching elsewhere in the interface.
