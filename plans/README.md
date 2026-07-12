# UI/UX polish plans

Audit baseline: commit `9d712bc`

These plans coordinate the `improve-animations` audit with Apple design principles for agency, response, familiarity, flexibility, accessibility, craft, and restrained delight.

| # | Plan | Severity | Status | Depends on |
|---|---|---|---|---|
| 001 | [Make asynchronous actions mutually exclusive](001-exclusive-async-actions.md) | HIGH | DONE | — |
| 002 | [Scope motion and honor Reduce Motion](002-accessible-scoped-motion.md) | MEDIUM | DONE | — |
| 003 | [Make layouts and typography adaptive](003-adaptive-layout-typography.md) | MEDIUM | DONE | — |
| 004 | [Put recovery actions beside startup failures](004-colocated-failure-recovery.md) | MEDIUM | DONE | 001 |
| 005 | [Adapt materials for transparency and contrast preferences](005-accessible-materials-contrast.md) | MEDIUM | DONE | — |
| 006 | [Announce meaningful status and completion feedback](006-accessibility-announcements.md) | MEDIUM | DONE | 001, 004 |
| 007 | [Make preview tab navigation instant](007-instant-preview-tabs.md) | LOW | DONE | 002 |
| 008 | [Complete semantic visual and motion tokens](008-semantic-design-tokens.md) | LOW | DONE | 002, 005 |
| 009 | [Refine button press and release response](009-asymmetric-button-response.md) | LOW | DONE | 002, 008 |

## Recommended execution order

1. **001** establishes operation correctness and reliable availability state before visual polish.
2. **002** removes broad implicit motion and creates the accessibility-aware motion foundation.
3. **007** is a small directness cleanup naturally completed with 002.
4. **003** makes the UI resilient before surface-level refinements are judged.
5. **004** builds recovery UX on the operation guards from 001.
6. **005** introduces accessibility-aware material behavior.
7. **006** adds announcements after terminal states and recovery language are stable.
8. **008** consolidates the final visual/motion roles after 002 and 005 establish them.
9. **009** performs the last motion feel pass using the finalized tokens.

Plans 002 and 007 may be implemented in one change because they touch the same preview-animation code. Plans 008 and 009 may also be combined after their dependencies are complete. Keep each plan's verification checklist intact even when combining implementation work.

## Final integrated verification

After all plans are complete:

1. Run `swift test --package-path macos/CCCodexProxy`.
2. Launch the actual menu-bar app and exercise Start, Stop, Refresh, provider changes, authentication/key entry, Advanced settings, install/restore, Logs filters, Copy, and recovery from a forced startup failure.
3. Repeat with keyboard-only navigation and VoiceOver.
4. Repeat with Reduce Motion, Reduce Transparency, Differentiate Without Color, and increased contrast enabled.
5. Check light and dark appearances, default and large accessibility text, minimum/default/wide Logs-window sizes, and the menu popover's compact viewport.
6. Inspect intentional motion at normal speed and 10% playback. Frequent navigation must be instant; disclosures must be locally anchored; no unrelated layout should animate.
