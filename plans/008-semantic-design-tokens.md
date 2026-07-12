# 008 — Complete semantic visual and motion tokens

- **Status**: DONE
- **Commit**: 9d712bc
- **Severity**: LOW
- **Category**: Cohesion and tokens
- **Estimated scope**: 2 files, medium mechanical change

## Problem

The app has a useful `AppTheme`, but local values still create parallel micro-scales. The shared theme defines three radii and one generic animation:

```swift
// macos/CCCodexProxy/Sources/CCCodexProxy/ContentView.swift:862 — current
static let largeRadius: CGFloat = 14
static let cardRadius: CGFloat = 12
static let smallRadius: CGFloat = 8
static let motion = Animation.easeOut(duration: 0.18)
```

Local controls add 7, 8, 9, and 10 point radii:

```swift
// ContentView.swift:952 — current
RoundedRectangle(cornerRadius: compact || iconOnly ? 7 : 9, style: .continuous)
```

```swift
// macos/CCCodexProxy/Sources/CCCodexProxy/LogViewerView.swift:43, :125, :191 — current
RoundedRectangle(cornerRadius: 10, style: .continuous)
RoundedRectangle(cornerRadius: 8, style: .continuous)
RoundedRectangle(cornerRadius: 9, style: .continuous)
```

Press animation also hardcodes a second duration at `ContentView.swift:961-962`. These values are individually reasonable, but role-less literals make future visual drift likely.

## Target

Use semantic role tokens rather than a large arbitrary numeric palette. Define only values with distinct UI roles, for example:

```swift
// target shape — exact names may follow surrounding style
static let radiusHero: CGFloat = 14
static let radiusCard: CGFloat = 12
static let radiusIconTile: CGFloat = 10
static let radiusControl: CGFloat = 9
static let radiusInset: CGFloat = 8
static let radiusCompactControl: CGFloat = 7

static let disclosureMotion = Animation.easeOut(duration: 0.18)
static let pressInMotion = Animation.easeOut(duration: 0.12)
static let pressOutMotion = Animation.easeOut(duration: 0.08)
```

The target values preserve the current visual design; this plan names and consolidates them. If plan 002 already introduced motion tokens, reuse those exact definitions and do not duplicate them.

## Repo conventions to follow

- Shared visual primitives live in `AppTheme` because both ContentView and LogViewerView already reference it.
- Continuous rounded rectangles are the established shape language.
- The product is a compact macOS utility: retain restrained radii and crisp ease-out motion.

## Steps

1. Inventory every corner-radius and animation literal in `ContentView.swift` and `LogViewerView.swift`.
2. Map each current radius to a semantic role. Use the exact values 14, 12, 10, 9, 8, and 7 only where those current distinctions correspond to real roles.
3. Rename existing `largeRadius`, `cardRadius`, and `smallRadius` only if doing so improves role clarity without causing unrelated churn. Otherwise retain them and add semantic aliases for uncovered roles.
4. Replace local radius literals in the header icon tile, search field, notices, and shared button style with tokens.
5. Consolidate 180 ms disclosure and 120/80 ms press animations with plan 002's definitions. Delete unused generic tokens.
6. Keep shape modifiers (`Capsule`, `Circle`) as shapes; do not convert them into numeric radius tokens.
7. Add brief comments only where a semantic role would otherwise be ambiguous; match the repository's sparse comment style.

## Boundaries

- Do NOT visually redesign components or collapse intentionally different roles merely to reduce token count.
- Do NOT create tokens for one-off dimensions unrelated to the visual system.
- Do NOT add a separate theme file unless file size/readability clearly justifies it after all planned UI work.
- Do NOT add dependencies.
- If cited code has drifted since `9d712bc`, STOP and report.

## Verification

- **Mechanical**: `swift test --package-path macos/CCCodexProxy` must pass. Search both UI files for remaining `cornerRadius: <number>` and hard-coded `Animation.easeOut(duration:)`; each remaining literal must be intentional and documented in the implementation report.
- **Feel check**: compare before/after screenshots in light and dark mode. Geometry should be visually unchanged; controls, cards, insets, and icon tiles should retain their hierarchy.
- **Done when**: visual and motion roles are centralized, no near-duplicate ad hoc values remain, and the refactor causes no unintended appearance change.
