# 009 — Refine button press and release response

- **Status**: DONE
- **Commit**: 9d712bc
- **Severity**: LOW
- **Category**: Physicality and response
- **Estimated scope**: 1 file, small change

## Problem

The shared button style already responds on press with an appropriate subtle scale, but uses symmetric timing for press and release:

```swift
// macos/CCCodexProxy/Sources/CCCodexProxy/ContentView.swift:960 — current
.scaleEffect(configuration.isPressed && isEnabled ? 0.97 : 1)
.animation(.easeOut(duration: 0.12), value: configuration.isPressed)
.animation(.easeOut(duration: 0.12), value: isEnabled)
```

A deliberate press can take 120 ms, while the system's response on release should snap faster. The style also needs the Reduce Motion behavior specified in plan 002.

## Target

- Press-in: `scaleEffect(0.97)` with `Animation.easeOut(duration: 0.12)`.
- Release: return to `1.0` with `Animation.easeOut(duration: 0.08)`.
- Reduce Motion: no scale change; pressed fill and tint still respond instantly.
- Disabled-state changes: do not inherit press scaling. Animate color/fill only if it improves legibility and does not delay availability feedback.

```swift
// target shape
@Environment(\.accessibilityReduceMotion) private var reduceMotion

.scaleEffect(configuration.isPressed && isEnabled && !reduceMotion ? 0.97 : 1)
.animation(
    reduceMotion
        ? nil
        : (configuration.isPressed ? AppTheme.pressInMotion : AppTheme.pressOutMotion),
    value: configuration.isPressed
)
```

If SwiftUI's optional animation overload or value semantics differ on macOS 13, use a transaction-based equivalent that produces these exact outcomes; do not fall back to one symmetric 120 ms animation.

## Repo conventions to follow

- `AppPressButtonStyle` is shared by action buttons, compact controls, and icon-only controls in both UI views.
- `backgroundFill(isPressed:)` already provides immediate causal feedback and must remain.
- Motion tokens should be supplied by plans 002/008 rather than redefined locally.

## Steps

1. Execute or incorporate plan 002's `accessibilityReduceMotion` environment handling and plan 008's press motion tokens.
2. Make the press animation conditional on the current pressed state: 120 ms entering the pressed state, 80 ms returning.
3. Disable scale under Reduce Motion while preserving `pressedFill` and foreground/stroke response.
4. Separate `isEnabled` feedback from press animation. Ensure disabling a button updates immediately or with a short opacity/color-only response; it must never scale.
5. Test normal, compact, and icon-only uses to ensure 0.97 remains subtle and does not move surrounding layout.

## Boundaries

- Do NOT use spring bounce or overshoot.
- Do NOT reduce scale below 0.95; use exactly 0.97.
- Do NOT delay action execution until the animation completes.
- Do NOT remove pressed fill feedback under Reduce Motion.
- Do NOT add haptics or sound to routine desktop controls.
- If cited code has drifted since `9d712bc`, STOP and report.

## Verification

- **Mechanical**: `swift test --package-path macos/CCCodexProxy` must pass.
- **Feel check**: press and hold Start, Refresh, Login/Repair, the icon-only refresh control, and Log viewer buttons. Feedback must begin on mouse-down, remain stable while held, and recover faster on release. Rapid press/release must retarget from the current presentation without jumping.
- Enable Reduce Motion: scale remains at 1.0, but pressed fill still changes on mouse-down.
- Review at 10% playback speed and confirm no surrounding layout moves.
- **Done when**: buttons feel immediate and physical in normal mode and remain clear without spatial movement under Reduce Motion.
