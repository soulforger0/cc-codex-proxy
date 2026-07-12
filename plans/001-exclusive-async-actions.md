# 001 — Make asynchronous actions mutually exclusive

- **Status**: DONE
- **Commit**: 9d712bc
- **Severity**: HIGH
- **Category**: Agency / interruptibility
- **Estimated scope**: 3 files, small-to-medium change

## Problem

Several asynchronous operations can be started more than once or run against a conflicting operation. The main Start button is protected, but the status-menu Start item only checks the final running state:

```swift
// macos/CCCodexProxy/Sources/CCCodexProxy/StatusItemController.swift:131 — current
menu.addItem(contextMenuItem(
    title: "Start",
    systemImage: "play.fill",
    action: #selector(startProxyFromMenu),
    isEnabled: !model.isRunning
))
```

The model also has no in-flight guard:

```swift
// macos/CCCodexProxy/Sources/CCCodexProxy/ProxyAppModel.swift:116 — current
func startProxy() async {
    if let proxyProcess, proxyProcess.isRunning {
        lastMessage = "Proxy is already running."
        return
    }
```

Settings installation and restoration can remain enabled against one another:

```swift
// macos/CCCodexProxy/Sources/CCCodexProxy/ContentView.swift:540 — current
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
```

This violates agency and predictability: rapid input can launch competing work, race published state, and make completion feedback unreliable.

## Target

Enforce exclusivity in the model, not only in views. UI disabling is supplementary feedback; model guards are the correctness boundary.

```swift
// target shape in ProxyAppModel
func startProxy() async {
    guard !isStartingProxy else { return }
    if let proxyProcess, proxyProcess.isRunning {
        lastMessage = "Proxy is already running."
        return
    }
    // existing implementation
}

func installClaudeSettings() async {
    guard !isInstallingClaudeSettings, !isRestoringClaudeSettings else { return }
    // existing implementation
}

func restoreClaudeSettings() async {
    guard !isInstallingClaudeSettings, !isRestoringClaudeSettings else { return }
    // existing implementation
}
```

Disable every entry point while its operation, or a conflicting operation, is active. Preserve immediate button press feedback; do not delay input artificially.

## Repo conventions to follow

- Operation state already lives as `@Published` booleans in `ProxyAppModel.swift:13-25`.
- The main Start button already expresses the intended UI rule at `ContentView.swift:133-140`: `model.isRunning || model.isStartingProxy`.
- Async UI actions use `Task { await model.method() }`; keep that pattern.
- Do not add a general task queue or dependency.

## Steps

1. In `ProxyAppModel.startProxy()`, add `guard !isStartingProxy else { return }` before examining `proxyProcess`.
2. In `ProxyAppModel.installClaudeSettings()` and `restoreClaudeSettings()`, guard both `isInstallingClaudeSettings` and `isRestoringClaudeSettings` before changing either flag.
3. If `refresh()` can overlap through the main Refresh control, add an `@Published private(set) var isRefreshing = false` guard around the full refresh sequence. Do not reuse narrower auth/settings flags as a proxy for the aggregate operation.
4. In `StatusItemController.showContextMenu()`, enable Start only when `!model.isRunning && !model.isStartingProxy`.
5. In `ContentView`, disable Install while either install or restore is active; disable Restore while either operation is active or no backup exists.
6. Disable the main Refresh button while aggregate refresh is active and show its in-progress state using the existing progress/status vocabulary.
7. Add unit tests that invoke guarded model operations twice and verify only one underlying operation begins. If direct process/CLI calls prevent a focused unit test, extract only the operation gate into a small internal helper and test that helper; do not redesign the model.

## Boundaries

- Do NOT serialize unrelated operations such as opening Logs or Stop unless they conflict with a verified invariant.
- Do NOT add arbitrary debounce timers.
- Do NOT disable the entire popover while work runs.
- Do NOT add dependencies.
- If the cited state model has drifted since commit `9d712bc`, STOP and report instead of improvising.

## Verification

- **Mechanical**: run `swift test --package-path macos/CCCodexProxy`; all tests must pass.
- **Feel check**: open the status menu and invoke Start repeatedly; only one startup begins, and Start is unavailable while it is underway. Open Advanced settings and confirm Install and Restore cannot run concurrently. Repeated Refresh input must not stack work.
- **Accessibility check**: VoiceOver must still expose disabled controls as unavailable and retain their labels.
- **Done when**: every async operation named above has both a model-level guard and accurate UI availability, with no overlapping duplicate execution.
