# Changelog

## 2026-07-12 AEST - v1.0.0 unified GPT-5.6 Responses

- Added unified Sol/Terra/Luna routing for Opus, Sonnet, and Haiku/subagents across ChatGPT Codex and custom OpenAI endpoints.
- Updated GPT-5.6 requests to the current Responses Lite HTTP/WebSocket contract and pinned compatibility to Codex `0.144.0-alpha.4`, with `CCP_CODEX_COMPAT_VERSION` as an emergency override.
- Replaced the separate custom Responses client with the shared OpenAI Responses transport, including optional bearer auth and custom `auto|websocket|http` selection.
- Removed custom Chat Completions support and added targeted migration errors for legacy configuration.
- Raised the default OpenAI-backed context window to 372,000 tokens and added Sonnet model controls to config, admin, CLI, and the macOS app.

## 2026-07-12 AEST - v0.5.1 background helper session detection

- Excluded Claude Code background pty hosts, spare background workers, and daemon processes from live-session detection so they no longer block proxy startup or shim repair.
- Kept the startup guard for real foreground Claude Code sessions so active sessions cannot silently switch backend assumptions.
- Updated README, architecture notes, package/app versions, and pre-release Homebrew metadata for `v0.5.1`.

## 2026-07-10 AEST - v0.5.0 startup health, combined logs, and GPT-5.6 defaults

- Added startup preflight diagnostics and `/healthz` polling so the macOS app reports Running only after the bundled proxy helper is ready, and surfaces early exits with actionable status.
- Added a searchable, newest-first Logs window that combines launcher events from `app.log` with proxy events from `proxy.log`, including source/severity filters and copy/reveal actions.
- Updated the built-in Codex and custom OpenAI defaults to `gpt-5.6-sol` for primary traffic and `gpt-5.6-luna` for small/subagent traffic, while keeping `gpt-5.6-terra` available for explicit selection.
- Preserved `max` reasoning effort for GPT-5.6 requests and retained the compatible `xhigh` mapping for older model families.
- Updated README, architecture notes, package/app versions, and pre-release Homebrew metadata for `v0.5.0`.

## 2026-07-06 AEST - v0.4.3 Homebrew app architecture guard

- Restricted the prebuilt Homebrew app cask to Apple Silicon (`arm64`) so Intel Macs cannot install a non-universal DMG that will not launch.
- Added release packaging checks for app/helper binary architecture, manifest architecture metadata, checksums, DMG verification, and cask architecture requirements.
- Documented that current prebuilt app DMGs are arm64-only while the CLI formula remains source-built.

## 2026-07-04 AEST - v0.4.2 background agent proxy routing

- Fixed Claude Code background agents launched through the managed shim so they work without native Anthropic/Claude auth.
- Started Claude's background daemon with managed proxy environment variables before launching sessions, while leaving daemon subcommands free of inline settings.
- Kept foreground and background sessions on inline proxy settings so daemon-respawned jobs persist `ANTHROPIC_BASE_URL`, model aliases, and related proxy routing without editing `~/.claude/settings.json`.
- Ignored Claude background pty host processes in live-session checks so daemon-owned helper processes do not block proxy startup or repair flows.
- Updated README, architecture notes, app/package versions, and Homebrew release metadata for `v0.4.2`.
- Validation: `cargo fmt --all -- --check`; `cargo test -p cc-codex-proxy -- --nocapture`; `cargo test --all`; installed-shim live smoke test with `claude --bg`; real Claude native auth remained logged out.

## 2026-07-04 AEST - v0.4.1 runtime session state and tool canonicalization

- Added shared Anthropic request canonicalization so Codex, DeepSeek, and custom OpenAI routes send stable tool definitions and JSON Schemas while removing exact duplicate tools.
- Persisted Codex upstream session generations in `codex-session-state.json` so helper restarts keep compacted or cleared Claude Code conversations on the correct generated Codex session.
- Stored Codex session state by full SHA-256 session hash, with bounded 30-day retention and a 512-session cap, so raw Claude Code session IDs are not written to the state file.
- Updated Claude Code launches to pass proxy routing through environment variables plus inline process settings, without writing `~/.claude/settings.json` during the normal app flow.
- Updated README and architecture documentation for the new runtime file, cache/reset behavior, and v0.4.1 changes.
- Validation: local proxy runtime verification against mock Codex, DeepSeek, and custom OpenAI upstreams; `cargo fmt --check`; `cargo test --all`; Swift package build; release artifact build and checksum verification.

## 2026-07-02 23:09 AEST - Homebrew install support

- Added tap-visible Homebrew formula and cask definitions for the Rust CLI helper and macOS app.
- Replaced placeholder Homebrew checksums with verified `v0.3.0` source archive and DMG hashes.
- Documented the user install flow, release metadata update flow, and tap-qualified Homebrew audit commands.
- Added CI/release syntax checks for Homebrew metadata so packaging regressions are caught earlier.
- Known limitation: release maintainers still need to publish updated Homebrew hashes after each GitHub Release because the immutable tag archive and DMG checksums are only final once assets are available.
- Validation: Ruby syntax checks, Homebrew audit/dry-run checks through a temporary local tap, updater script idempotence check, and `git diff --check`.
