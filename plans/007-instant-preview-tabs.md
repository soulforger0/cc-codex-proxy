# 007 — Make preview tab navigation instant

- **Status**: DONE
- **Commit**: 9d712bc
- **Severity**: LOW
- **Category**: Purpose and frequency
- **Estimated scope**: 1 file, small change

## Problem

The settings preview is a segmented navigation control whose value participates in the root animation:

```swift
// macos/CCCodexProxy/Sources/CCCodexProxy/ContentView.swift:36 — current
.animation(AppTheme.motion, value: settingsPreviewTab)
```

Its content also declares an opacity transition:

```swift
// ContentView.swift:530 — current
Picker("Settings preview", selection: $settingsPreviewTab) {
    ForEach(ClaudeSettingsPreviewTab.allCases) { tab in
        Text(tab.rawValue).tag(tab)
    }
}
.pickerStyle(.segmented)

settingsPreviewContent(preview)
    .transition(.opacity)
```

Users may switch among Diff, Current, After, and Restore repeatedly while comparing settings. A 180 ms crossfade adds latency and can briefly double-expose dense code content.

## Target

Segmented preview navigation updates immediately with no explicit or inherited animation. The selected segment retains native AppKit control feedback, but the preview body performs an instantaneous content swap.

```swift
// target
Picker("Settings preview", selection: $settingsPreviewTab) { ... }
    .pickerStyle(.segmented)

settingsPreviewContent(preview)
```

## Repo conventions to follow

- Frequent developer-tool navigation should be crisp and direct.
- Native segmented-control selection feedback is sufficient.
- Plan 002 removes broad root animation and localizes purposeful transitions; execute this plan as part of or immediately after plan 002.

## Steps

1. Remove the `.animation(..., value: settingsPreviewTab)` root modifier if it still exists.
2. Remove `.transition(.opacity)` from `settingsPreviewContent(preview)`.
3. Do not replace it with a spring, matched geometry effect, blur, keyframe, or custom selection indicator.
4. Confirm changing tabs does not incidentally animate surrounding summary/actions after plan 002 scopes motion.

## Boundaries

- Do NOT change the content, labels, ordering, or picker style.
- Do NOT remove animation from occasional Advanced settings disclosure; that is governed by plan 002.
- Do NOT add dependencies.
- If cited code has drifted since `9d712bc`, STOP and report.

## Verification

- **Mechanical**: `swift test --package-path macos/CCCodexProxy` must pass.
- **Feel check**: click rapidly among all four segments and use keyboard navigation. Every panel appears on the same frame as selection with no opacity overlap or delayed response.
- Enable Reduce Motion and repeat; behavior is identically instant.
- **Done when**: preview comparison feels like native tab/segment navigation rather than a sequence of animated entrances.
