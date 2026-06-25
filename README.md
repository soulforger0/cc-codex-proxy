# CC Codex Proxy

<p align="center">
  <strong>A macOS menu bar app that lets Claude Code run through your ChatGPT Codex subscription.</strong>
</p>

<p align="center">
  <a href="https://github.com/soulforger0/cc-codex-proxy/releases/latest/download/CCCodexProxy-macOS.dmg"><strong>Download latest DMG</strong></a>
  ·
  <a href="https://github.com/soulforger0/cc-codex-proxy/releases/latest/download/SHA256SUMS">Checksums</a>
  ·
  <a href="https://github.com/soulforger0/cc-codex-proxy/releases">All releases</a>
  ·
  <a href="https://github.com/soulforger0/cc-codex-proxy/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/soulforger0/cc-codex-proxy/actions/workflows/ci.yml/badge.svg?branch=main"></a>
  <a href="https://github.com/soulforger0/cc-codex-proxy/releases"><img alt="GitHub release" src="https://img.shields.io/github/v/release/soulforger0/cc-codex-proxy?include_prereleases&sort=semver"></a>
  <a href="./LICENSE"><img alt="License: MIT" src="https://img.shields.io/badge/license-MIT-blue.svg"></a>
</p>

CC Codex Proxy is a local-only macOS app for people who want to use Claude Code with a ChatGPT subscription Codex session. It runs in your menu bar, starts a local Anthropic-compatible proxy, handles ChatGPT OAuth, and temporarily routes new `claude` launches through the proxy while the app is running.

You do not need to install or configure a separate CLI. Download the app, sign in, click **Start**, then open a fresh Claude Code session.

> [!WARNING]
> This project uses the ChatGPT subscription Codex backend, which is not a public OpenAI API contract. Backend behavior can change. Kimi, Cursor, and generic OpenAI API-key routing are intentionally out of scope.

## Why use it?

Claude Code speaks Anthropic's Messages API. ChatGPT Codex sessions use a different upstream protocol. CC Codex Proxy bridges that gap locally so Claude Code can keep its normal terminal workflow while requests are translated to the Codex backend.

It helps you:

- Use Claude Code through a ChatGPT subscription Codex session.
- Avoid manual `ANTHROPIC_*` environment setup.
- Start, stop, and monitor the local proxy from the macOS menu bar.
- Sign in with ChatGPT OAuth from the app.
- Keep routing temporary: when the app quits, the managed Claude shim is restored or falls back safely.

## Install

1. Download the latest DMG: [CCCodexProxy-macOS.dmg](https://github.com/soulforger0/cc-codex-proxy/releases/latest/download/CCCodexProxy-macOS.dmg).
2. Open the DMG and drag `CCCodexProxy.app` into Applications.
3. Launch `CCCodexProxy.app`.
4. If macOS blocks the first launch, right-click the app and choose **Open**.

If the direct DMG link ever fails, use the [GitHub Releases page](https://github.com/soulforger0/cc-codex-proxy/releases) and download the newest `CCCodexProxy-<version>-macOS.dmg` asset. Each release also includes checksums, a release manifest, and GitHub artifact attestations for the uploaded files.

> [!NOTE]
> Releases are currently ad-hoc signed but not Developer ID signed or notarized. macOS may show a Gatekeeper warning on first launch.

## Quick Start

1. Launch **CC Codex Proxy** from Applications.
2. Click **Login** and complete ChatGPT OAuth in your browser.
3. Close any running Claude Code sessions.
4. Click **Start** in the menu bar app.
5. Open a new Claude Code session normally with `claude`.

The app automatically manages the temporary `claude` command shim. New Claude Code sessions route through `127.0.0.1` only while the app is open and the proxy health check passes.

## How the app connects Claude Code

No manual Claude Code setup is required.

When the app launches, it discovers your existing `claude` command and installs a managed shim in its place. The shim preserves the original Claude Code executable and only changes the environment for new launches when CC Codex Proxy is running and healthy.

When you start Claude Code while the proxy is running, the shim injects the managed environment that points Claude Code at the local proxy, including the base URL, model names, small-model fallback, compaction window, and proxy auth token placeholder. When the app quits, it restores the original `claude` command. If the proxy is stopped while the app is still open, new Claude Code launches show an error instead of silently using the wrong backend.

The app also refuses to start the proxy while Claude Code is already running. Close existing Claude Code sessions first, then start the proxy and open a new session so routing is consistent from the beginning.

Advanced users can preview or repair managed Claude Code settings from the app's **Advanced settings.json** section, but this is not part of the normal install flow.

## Features

- **DMG install** — drag `CCCodexProxy.app` into Applications like a normal Mac app.
- **Menu bar controls** — start, stop, refresh status, and open logs without touching the terminal.
- **ChatGPT OAuth** — sign in from the app and store tokens locally under Application Support.
- **Temporary Claude routing** — managed shim routes new `claude` launches only while the app is alive and healthy.
- **Local-only proxy** — binds to `127.0.0.1` and does not expose a remote service.
- **Anthropic-compatible surface** — implements `/v1/messages` and `/v1/messages/count_tokens` for Claude Code.
- **Codex transport fallback** — tries WebSocket first in `auto` mode, then falls back to HTTP SSE if needed.
- **Embedded helper** — the SwiftUI app bundles the Rust/Tokio proxy helper at `CCCodexProxy.app/Contents/Helpers`.

## Verify a download

Download `SHA256SUMS` from the release page, then verify the DMG checksum from the same directory:

```sh
shasum -a 256 -c SHA256SUMS --ignore-missing
```

If you have the GitHub CLI installed, you can also verify the GitHub artifact attestation:

```sh
gh attestation verify CCCodexProxy-macOS.dmg --repo soulforger0/cc-codex-proxy
```

## Runtime Files

- Config/auth/model profiles: `~/Library/Application Support/CCCodexProxy/`
- Logs: `~/Library/Logs/CCCodexProxy/proxy.log`
- Claude shim state: `~/Library/Application Support/CCCodexProxy/claude-shim.json`

## Build From Source

Rust is required to build the proxy helper. Swift is required to build the menu bar app.

```sh
scripts/build-app.sh
```

The output is:

- `dist/CCCodexProxy.app`
- `dist/CCCodexProxy-<version>-macOS.dmg`
- `dist/CCCodexProxy-<version>-macOS.zip`
- `dist/CCCodexProxy-macOS.dmg`
- `dist/CCCodexProxy-macOS.zip`
- `dist/SHA256SUMS`
- `dist/RELEASE_MANIFEST.json`

## Development

```sh
cargo test --all
swift build --package-path macos/CCCodexProxy
cargo run -p cc-codex-proxy -- serve
cargo run -p cc-codex-proxy -- auth login
cargo run -p cc-codex-proxy -- doctor
```

Run the explicit 250-agent mock streaming stress test with:

```sh
cargo test -p proxy-core --test server_mock -- streaming_stress_250_agents --ignored --nocapture
```

See [docs/RELEASING.md](docs/RELEASING.md) for the tag-based release process, artifact checks, attestations, and signing/notarization roadmap.

## Architecture

The supported path is:

```text
Claude Code -> 127.0.0.1 Anthropic-compatible proxy -> ChatGPT subscription Codex Responses backend
```

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the runtime model, transport fallback strategy, request/response mapping, and robustness targets.

## Contributing

Issues and pull requests are welcome. Before opening a PR, run the local checks that match your change:

```sh
cargo test --all
swift build --package-path macos/CCCodexProxy
```

For security-sensitive reports, please avoid posting secrets, OAuth tokens, or private session details in public issues.

## License

MIT © Ling Li. See [LICENSE](LICENSE).
