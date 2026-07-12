# 002 — Scope motion and honor Reduce Motion

- **Status**: DONE
- **Commit**: 9d712bc
- **Severity**: MEDIUM
- **Category**: Accessibility / purpose and frequency
- **Estimated scope**: 2 files, medium change

## Problem

The root view attaches the same animation to thirteen values, causing unrelated descendant changes—including network-driven status and layout updates—to animate implicitly:

```swift
// macos/CCCodexProxy/Sources/CCCodexProxy/ContentView.swift:25 — current
.animation(AppTheme.motion, value: model.isRunning)
.animation(AppTheme.motion, value: model.isStartingProxy)
.animation(AppTheme.motion, value: model.isAuthenticated)
.animation(AppTheme.motion, value: model.isLoggingIn)
.animation(AppTheme.motion, value: model.isCheckingAuthStatus)
.animation(AppTheme.motion, value: model.isInstallingClaudeShim)
.animation(AppTheme.motion, value: model.isSavingDeepSeekAPIKey)
.animation(AppTheme.motion, value: model.isSavingCustomOpenAIAPIKey)
.animation(AppTheme.motion, value: model.isDeepSeekKeyInputExpanded)
.animation(AppTheme.motion, value: model.isCustomOpenAIKeyInputExpanded)
.animation(AppTheme.motion, value: model.provider)
.animation(AppTheme.motion, value: settingsPreviewTab)
.animation(AppTheme.motion, value: showAdvancedClaudeSettings)
```

Movement is used for expansions without a reduced-motion alternative:

```swift
// ContentView.swift:249 and :284 — current
.transition(.opacity.combined(with: .move(edge: .top)))
```

The shared button style also scales for every user regardless of accessibility preference:

```swift
// ContentView.swift:960 — current
.scaleEffect(configuration.isPressed && isEnabled ? 0.97 : 1)
.animation(.easeOut(duration: 0.12), value: configuration.isPressed)
```

## Target

Remove all root-level implicit animations. Attach animation only to occasional, spatially meaningful disclosure/editor transitions. Keep the existing crisp 180 ms ease-out for these small UI transitions and use a short opacity-only transition under Reduce Motion.

```swift
// target shape
@Environment(\.accessibilityReduceMotion) private var reduceMotion

private var disclosureTransition: AnyTransition {
    reduceMotion
        ? .opacity
        : .opacity.combined(with: .move(edge: .top))
}
```

For full motion, use `AppTheme.motion = .easeOut(duration: 0.18)`. Under Reduce Motion, use opacity only with `Animation.easeOut(duration: 0.18)`; do not remove comprehension-preserving fades.

Press feedback remains immediate via fill/color, but scale becomes `1` when Reduce Motion is active. Use 120 ms press-in and a faster 80 ms release response.

The native status popover may retain AppKit's standard animation because it follows platform convention. Do not replace it with custom motion; only disable it if AppKit exposes a reliable Reduce Motion signal in the supported macOS 13 API without private API or notification plumbing.

## Repo conventions to follow

- Motion tokens belong in `AppTheme` at `ContentView.swift:862-900`.
- Existing transitions are attached directly to conditional content at `ContentView.swift:249-252` and `284-290`; keep motion local there.
- `AppPressButtonStyle` is the shared press-feedback implementation for both main and log views.
- The product personality is a crisp developer utility: no bounce, overshoot, parallax, or decorative stagger.

## Steps

1. Add `@Environment(\.accessibilityReduceMotion)` to `ContentView` and `AppPressButtonStyle`.
2. Delete all thirteen root `.animation(AppTheme.motion, value:)` modifiers from `ContentView.body`.
3. Add semantic AppTheme tokens: `disclosureMotion = Animation.easeOut(duration: 0.18)`, `pressInMotion = Animation.easeOut(duration: 0.12)`, and `pressOutMotion = Animation.easeOut(duration: 0.08)`. Replace the old generic token if it has no remaining purpose.
4. Create a local transition helper returning opacity-only under Reduce Motion and opacity plus top-edge movement otherwise.
5. Apply the helper and a narrowly scoped `.animation(..., value:)` to Advanced settings, DeepSeek key editor, and custom endpoint key editor only.
6. Remove animation from `settingsPreviewTab`; segmented preview navigation must update instantly.
7. In `AppPressButtonStyle`, preserve pressed fill in both modes. Apply `scaleEffect(0.97)` only when enabled, pressed, and Reduce Motion is off.
8. Implement asymmetric press response without adding state outside the style: choose 120 ms while `configuration.isPressed` is true and 80 ms when false.
9. Confirm status/auth/progress values now update immediately unless the specific inserted/removed view has a local transition.

## Boundaries

- Do NOT add spring or bounce motion.
- Do NOT animate keyboard-initiated or repeated segmented navigation.
- Do NOT set `.animation(nil)` broadly; remove broad animation and attach intentional local animation instead.
- Do NOT remove opacity/color feedback under Reduce Motion.
- Do NOT introduce custom popover presentation.
- If cited code has drifted since `9d712bc`, STOP and report.

## Verification

- **Mechanical**: `swift test --package-path macos/CCCodexProxy` must pass.
- **Feel check**: repeatedly switch preview tabs; content changes instantly. Toggle Advanced settings and API-key editors; full-motion mode fades/moves from the top in 180 ms and reverses without a jump. Start/Refresh status updates do not animate the whole layout.
- In Accessibility settings, enable Reduce Motion. Repeat every interaction: disclosure/key editors use opacity only; buttons change fill immediately without scaling.
- Capture the expansion at slow playback and confirm it remains anchored to the disclosure/key control rather than moving unrelated content.
- **Done when**: all motion is local and purposeful, repeated navigation is instant, and Reduce Motion removes positional and scale movement while preserving feedback.
