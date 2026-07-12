# 005 — Adapt materials for transparency and contrast preferences

- **Status**: DONE
- **Commit**: 9d712bc
- **Severity**: MEDIUM
- **Category**: Accessibility / materials and depth
- **Estimated scope**: 2 files, medium change

## Problem

The visual system relies on translucent materials, low-alpha fills, and hairlines:

```swift
// macos/CCCodexProxy/Sources/CCCodexProxy/ContentView.swift:75 — current
RoundedRectangle(cornerRadius: AppTheme.largeRadius, style: .continuous)
    .fill(.regularMaterial)
// plus a .plusLighter white gradient and 10% hairline
```

```swift
// ContentView.swift:876 — current
static let hairline = Color.primary.opacity(0.10)
static let subtleFill = Color.primary.opacity(0.045)
static let pressedFill = Color.primary.opacity(0.075)
static let disabledFill = Color.primary.opacity(0.028)
```

```swift
// ContentView.swift:903 — current PanelCard
.fill(.regularMaterial)
// highlight overlay, low-alpha stroke, and shadow
```

These values work in normal appearance but have no explicit fallback for Reduce Transparency or stronger differentiation needs. On variable backgrounds, disabled and secondary surfaces can become difficult to distinguish.

## Target

Keep the existing restrained material hierarchy in normal mode. Add a theme environment/modifier that adapts surfaces based on platform accessibility settings:

- With Reduce Transparency: replace material card fills with near-solid platform colors (`windowBackgroundColor` for floating panels and `controlBackgroundColor`/`textBackgroundColor` for inset surfaces); do not stack blur/material layers.
- With Differentiate Without Color: preserve icons and text labels for state and strengthen relevant outlines; never rely on green/orange/red alone.
- Use platform semantic colors in light and dark appearance.
- Strengthen boundaries only in accessibility modes; avoid making the normal UI border-heavy.

SwiftUI macOS exposes `@Environment(\.accessibilityReduceTransparency)` and `@Environment(\.accessibilityDifferentiateWithoutColor)`. Use these rather than custom preference storage.

## Repo conventions to follow

- Theme definitions and surface modifiers live in `ContentView.swift:862-937` and are reused by `LogViewerView`.
- Existing status components already combine color with icons/text: status pill text, auth symbols, and notice symbols. Preserve these redundant cues.
- `Color(nsColor:)` is already the bridge to macOS semantic colors.

## Steps

1. Introduce a small `AppSurfaceStyle` or environment-aware `PanelCard` implementation that reads `accessibilityReduceTransparency` and `accessibilityDifferentiateWithoutColor`.
2. Under Reduce Transparency, replace `.regularMaterial` in `PanelCard` and the main header with a near-solid semantic surface. Suppress the `.plusLighter` highlight overlay when it no longer describes a translucent material.
3. Add an environment-aware background surface for `LogViewerView.header`, which currently uses `.regularMaterial` directly at `LogViewerView.swift:84-87`.
4. Keep `AppTheme.insetSurface` and `codeSurface` semantic, but provide higher-opacity or solid variants under Reduce Transparency.
5. Increase boundary contrast for cards, capsules, inputs, and disabled controls only in accessibility modes. Use semantic `Color.primary` opacity values sufficient to create a visible edge in both light and dark modes; validate visually rather than introducing arbitrary decorative borders.
6. Audit status and action components for color-only meaning. Retain or add symbol/text distinctions when Differentiate Without Color is enabled; do not encode state solely through tint.
7. Preserve normal-mode material weight: header and primary cards may use regular material; inset/code surfaces remain visually lower than cards.
8. Add previews or a lightweight host configuration for normal, Reduce Transparency, and Differentiate Without Color if feasible without new dependencies.

## Boundaries

- Do NOT introduce web-style `backdrop-filter` concepts or custom blur shaders; this is native SwiftUI.
- Do NOT make every surface opaque in normal mode.
- Do NOT stack multiple light translucent surfaces.
- Do NOT use color alone for state.
- Do NOT alter product branding or accent-color behavior.
- If cited code has drifted since `9d712bc`, STOP and report.

## Verification

- **Mechanical**: `swift test --package-path macos/CCCodexProxy` must pass.
- **Feel check**: inspect popover and Logs window in light and dark mode. In normal settings, hierarchy should look unchanged or slightly more coherent. Enable Reduce Transparency: cards and headers become solid enough for legibility, without doubled layers or muddy highlights. Enable Differentiate Without Color: status remains understandable with color perception removed.
- Use macOS Accessibility Display settings to enable Increase Contrast as an additional manual check; all borders, labels, disabled controls, and error states must remain distinguishable.
- **Done when**: the material system retains depth in normal mode and becomes solid, legible, and redundantly encoded under accessibility preferences.
