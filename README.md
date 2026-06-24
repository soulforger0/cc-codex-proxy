# CC Codex Proxy

<p align="center">
  <strong>Run Claude Code through a local macOS proxy backed by a ChatGPT subscription Codex session.</strong>
</p>

<p align="center">
  <a href="https://github.com/soulforger0/cc-codex-proxy/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/soulforger0/cc-codex-proxy/actions/workflows/ci.yml/badge.svg?branch=main"></a>
  <a href="https://github.com/soulforger0/cc-codex-proxy/releases"><img alt="GitHub release" src="https://img.shields.io/github/v/release/soulforger0/cc-codex-proxy?include_prereleases&sort=semver"></a>
  <a href="./LICENSE"><img alt="License: MIT" src="https://img.shields.io/badge/license-MIT-blue.svg"></a>
</p>

`cc-codex-proxy` is a local-only Anthropic-compatible proxy for Claude Code. It exposes the endpoints Claude Code expects, translates Anthropic Messages requests to the ChatGPT Codex Responses backend, and ships as a self-contained SwiftUI menu bar app with an embedded Rust/Tokio helper.

> [!WARNING]
> This project uses the ChatGPT subscription Codex backend, which is not a public OpenAI API contract. Backend behavior can change. Kimi, Cursor, and generic OpenAI API-key routing are intentionally out of scope.

## Quick Start

1. Download `CCCodexProxy-<version>-macOS.dmg` from [GitHub Releases](https://github.com/soulforger0/cc-codex-proxy/releases).
2. Open the DMG and drag `CCCodexProxy.app` into Applications.
3. Launch the app and complete ChatGPT OAuth login.
4. Start the proxy from the menu bar app.
5. Open a new Claude Code session after the app reports that the proxy is running.

The app installs a temporary shim for new `claude` launches. Existing Claude Code sessions must be closed before starting the proxy so they do not silently switch backend assumptions mid-session.

> [!NOTE]
> Releases are currently ad-hoc signed but not Developer ID signed or notarized. macOS may show a Gatekeeper warning on first launch; if that happens, right-click `CCCodexProxy.app` and choose **Open**.

If the DMG does not work in your environment, download the zip release asset, unzip it, and move `CCCodexProxy.app` to Applications manually.

## Features

- **Self-contained macOS app** — SwiftUI menu bar app for macOS 13+ with the Rust proxy helper embedded at `CCCodexProxy.app/Contents/Helpers`.
- **Local-only proxy** — binds to `127.0.0.1` and does not expose a remote service.
- **Claude Code shim management** — temporarily installs a managed `claude` command shim while the app is running, then restores or falls back safely.
- **Anthropic-compatible surface** — implements `/v1/messages` and `/v1/messages/count_tokens` for Claude Code.
- **Codex transport fallback** — supports upstream WebSocket, HTTP SSE, or `auto` fallback mode.
- **Data-driven model profiles** — model IDs and context-window metadata live in `model-profiles.json`.

## Claude Code Configuration

Launching the macOS app is the recommended path. It installs a crash-safe, temporary managed `claude` command shim. New Claude Code sessions route through the proxy only while the app process is alive and the proxy health check passes.

Advanced users can still install permanent managed environment keys into `~/.claude/settings.json` through the CLI:

```sh
cc-codex-proxy auth login
cc-codex-proxy claude install-settings --model gpt-5.5[1m] --small-model gpt-5.4-mini[1m]
```

Restore the newest settings backup with:

```sh
cc-codex-proxy claude restore-settings
```

### Transport Selection

Claude Code always talks to the local proxy over HTTP. Streaming responses are returned as Anthropic-compatible SSE.

The proxy's upstream Codex transport defaults to `auto`: it tries WebSocket first, then falls back to HTTP SSE if WebSocket setup fails.

```sh
export CCP_CODEX_TRANSPORT=auto       # default, try WebSocket first, then HTTP SSE
export CCP_CODEX_TRANSPORT=websocket  # fail hard if WebSocket is unavailable
export CCP_CODEX_TRANSPORT=http       # always use HTTP SSE
```

## Build From Source

Rust is required to build the proxy/CLI. Swift is required to build the menu bar app.

```sh
scripts/build-app.sh
```

The output is:

- `dist/CCCodexProxy.app`
- `dist/CCCodexProxy-<version>-macOS.dmg`
- `dist/CCCodexProxy-<version>-macOS.zip`
- `dist/SHA256SUMS`

The app does not require a separate `cc-codex-proxy` command on `PATH`.

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

## Architecture

The supported path is:

```text
Claude Code -> 127.0.0.1 Anthropic-compatible proxy -> ChatGPT subscription Codex Responses backend
```

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the runtime model, transport fallback strategy, request/response mapping, and robustness targets.

## Runtime Files

- Config/auth/model profiles: `~/Library/Application Support/CCCodexProxy/`
- Logs: `~/Library/Logs/CCCodexProxy/proxy.log`

## Contributing

Issues and pull requests are welcome. Before opening a PR, run the local checks that match your change:

```sh
cargo test --all
swift build --package-path macos/CCCodexProxy
```

For security-sensitive reports, please avoid posting secrets, OAuth tokens, or private session details in public issues.

## License

MIT © Ling Li. See [LICENSE](LICENSE).
