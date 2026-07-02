# Changelog

## 2026-07-02 23:09 AEST - Homebrew install support

- Added tap-visible Homebrew formula and cask definitions for the Rust CLI helper and macOS app.
- Replaced placeholder Homebrew checksums with verified `v0.3.0` source archive and DMG hashes.
- Documented the user install flow, release metadata update flow, and tap-qualified Homebrew audit commands.
- Added CI/release syntax checks for Homebrew metadata so packaging regressions are caught earlier.
- Known limitation: release maintainers still need to publish updated Homebrew hashes after each GitHub Release because the immutable tag archive and DMG checksums are only final once assets are available.
- Validation: Ruby syntax checks, Homebrew audit/dry-run checks through a temporary local tap, updater script idempotence check, and `git diff --check`.
