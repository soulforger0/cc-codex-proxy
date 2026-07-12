# 003 — Make layouts and typography adaptive

- **Status**: DONE
- **Commit**: 9d712bc
- **Severity**: MEDIUM
- **Category**: Flexibility / typography
- **Estimated scope**: 3 files, medium-to-large change

## Problem

The menu popover and hosted content are fixed to 440×660 points:

```swift
// macos/CCCodexProxy/Sources/CCCodexProxy/StatusItemController.swift:49 — current
popover.contentSize = NSSize(width: 440, height: 660)
popover.contentViewController = NSHostingController(
    rootView: ContentView()
        .environmentObject(model)
        .frame(width: 440, height: 660)
)
```

Multiple controls use absolute widths:

```swift
// ContentView.swift:203, :228, :587 — current
.frame(width: 300)
.frame(width: 300)
.frame(width: title == "https://host.example" ? 300 : 210)
```

The log view combines fixed filter widths and small hard-coded typography:

```swift
// LogViewerView.swift:133 — current
.frame(width: 190)
// ...
.frame(width: 255)

// LogViewerView.swift:293 — current
.font(.system(size: 9, weight: .bold, design: .monospaced))
// ...
.font(.system(size: 11, design: .monospaced))
```

This is brittle under larger accessibility text sizes, localization, and narrower/resized windows.

## Target

Keep the menu-bar popover compact but allow SwiftUI content to adapt. Use semantic text styles and flexible sizing before introducing alternate layouts.

- Popover: keep an ideal size of 440×660, with content expressed using `minWidth`/`idealWidth` and `minHeight`/`idealHeight`, not a second exact frame.
- Settings rows: controls should use `frame(minWidth:..., idealWidth:..., maxWidth:...)` and consume available width.
- Log filters: use `ViewThatFits(in: .horizontal)` to present one horizontal row when space permits and a two-row layout otherwise.
- Log typography: replace 9 pt with `.caption2`, and 11 pt with `.caption`; preserve monospaced design and weight.
- Preserve macOS system fonts and semantic styles; do not add a custom typeface.

## Repo conventions to follow

- Semantic styles are already common: `.headline`, `.subheadline`, `.caption`, and `.caption2` throughout both views.
- `settingsInputRow` is the shared mapping between labels and their controls at `ContentView.swift:603-622`; prefer improving this abstraction over per-row offsets.
- The root content already scrolls vertically, so increased text should expand naturally rather than clip.
- Log rows use monospaced designs for diagnostic data; retain that distinction.

## Steps

1. In `StatusItemController`, retain `popover.contentSize = NSSize(width: 440, height: 660)` as the initial/ideal native size, but replace the exact `.frame(width:height:)` on `ContentView` with adaptive minimum/ideal constraints. Use a minimum width no smaller than 400 and ideal width 440; use minimum height no smaller than 520 and ideal height 660.
2. Convert provider and transport pickers from exact 300-point frames to flexible frames that can shrink without truncating their short labels and expand up to the available trailing column.
3. Refactor `modelTextField` so the caller or row role determines sizing explicitly; remove the width decision based on placeholder string equality. Give model fields a useful minimum around 160 and endpoint fields a useful minimum around 220, with flexible maximum width.
4. Update `settingsInputRow` to keep labels readable and controls aligned. If large text causes the horizontal mapping to fail, use `ViewThatFits` to fall back to a leading-aligned vertical label/control stack.
5. Retain a compact width for the numeric port field, but use minimum/ideal sizing instead of a hard exact width and allow the field to expand for larger text.
6. In `LogViewerView.filters`, extract the search field and segmented controls into reusable subviews, then wrap horizontal and stacked arrangements in `ViewThatFits(in: .horizontal)`. The fallback must place search on its own full-width row and the pickers beneath it.
7. Remove exact 190/255 picker widths; use minimum/ideal/flexible sizing that keeps all segments legible.
8. Replace `.system(size: 9, ...)` with `.caption2` plus monospaced design and bold weight. Replace `.system(size: 11, design: .monospaced)` with `.caption.monospaced()` or the equivalent semantic design call.
9. Review every `.lineLimit(1)` or fixed-width metadata column in both views. Retain truncation only where full text remains available through selection, help, or detail; otherwise allow wrapping.
10. Add previews or focused layout tests for default and large accessibility content sizes if the package's macOS target supports them without new dependencies.

## Boundaries

- Do NOT turn the popover into a custom floating window.
- Do NOT remove the log window's sensible minimum size without replacing it with a verified adaptive layout.
- Do NOT replace segmented controls unless they demonstrably fail after the adaptive layout work.
- Do NOT reduce font sizes to make content fit.
- Do NOT add dependencies or a custom font.
- If cited code has drifted since `9d712bc`, STOP and report.

## Verification

- **Mechanical**: `swift test --package-path macos/CCCodexProxy` must pass.
- **Feel check**: inspect the popover at default text size and confirm it remains compact and aligned. Increase macOS text/accessibility sizing and verify every setting remains reachable, labels do not overlap controls, and horizontal rows fall back cleanly.
- Resize the Logs window to its minimum and wider sizes. Search, Source, and Level controls must remain legible and usable without collision.
- Test long model names, a long endpoint URL, and long localized-like labels; inputs must not force labels offscreen.
- **Done when**: the default layout retains its visual density while larger text and constrained widths reflow rather than clip or collide.
