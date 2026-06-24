# CC Codex Proxy

<p align="center">
  <strong>Run Claude Code through a local macOS proxy backed by a ChatGPT subscription Codex session.</strong>
</p>

<p align="center">
  <a href="https://github.com/soulforger0/cc-codex-proxy/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/soulforger0/cc-codex-proxy/actions/workflows/ci.yml/badge.svg?branch=main"></a>
  <a href="https://github.com/soulforger0/cc-codex-proxy/releases"><img alt="GitHub release" src="https://img.shields.io/github/v/release/soulforger0/cc-codex-proxy?include_prereleases&sort=semver"></a>
  <a href="https://github.com/soulforger0/cc-codex-proxy/stargazers"><img alt="GitHub stars" src="https://img.shields.io/github/stars/soulforger0/cc-codex-proxy?style=social"></a>
  <a href="./LICENSE"><img alt="License: MIT" src="https://img.shields.io/badge/license-MIT-blue.svg"></a>
</p>

`cc-codex-proxy` is a local-only Anthropic-compatible proxy for Claude Code. It exposes the endpoints Claude Code expects, translates Anthropic Messages requests to the ChatGPT Codex Responses backend, and ships as a self-contained SwiftUI menu bar app with an embedded Rust/Tokio helper.

> [!WARNING]
> This project uses the ChatGPT subscription Codex backend, which is not a public OpenAI API contract. Backend behavior can change. Kimi, Cursor, and generic OpenAI API-key routing are intentionally out of scope.

## Contents

- [Why](#why)
- [Features](#features)
- [Quick Start](#quick-start)
- [Claude Code Configuration](#claude-code-configuration)
- [Build From Source](#build-from-source)
- [Development](#development)
- [Runtime Files](#runtime-files)
- [Architecture](#architecture)
- [Star History](#star-history)
- [Contributing](#contributing)
- [License](#license)

## Why

Claude Code speaks Anthropic's Messages API. ChatGPT Codex sessions use a different upstream protocol. CC Codex Proxy bridges that gap locally so new Claude Code sessions can route through `127.0.0.1` while preserving Claude Code's normal streaming, tool-use, context-counting, and settings workflows.

## Features

- **Self-contained macOS app** — SwiftUI menu bar app for macOS 13+ with the Rust proxy helper embedded at `CCCodexProxy.app/Contents/Helpers`.
- **Local-only proxy** — binds to `127.0.0.1` and does not expose a remote service.
- **Claude Code shim management** — temporarily installs a managed `claude` command shim while the app is running, then restores or falls back safely.
- **Anthropic-compatible surface** — implements `/v1/messages` and `/v1/messages/count_tokens` for Claude Code.
- **Codex transport fallback** — supports upstream WebSocket, HTTP SSE, or `auto` fallback mode.
- **Data-driven model profiles** — model IDs and context-window metadata live in `model-profiles.json`.
- **Stress-tested streaming path** — includes mock upstream tests for high-concurrency local agent streams.

## Quick Start

1. Download the latest `CCCodexProxy.zip` from [GitHub Releases](https://github.com/soulforger0/cc-codex-proxy/releases).
2. Unzip and move `CCCodexProxy.app` to `/Applications`.
3. Launch the app and complete ChatGPT OAuth login.
4. Start the proxy from the menu bar app.
5. Open a new Claude Code session after the app reports that the proxy is running.

The app installs a temporary shim for new `claude` launches. Existing Claude Code sessions must be closed before starting the proxy so they do not silently switch backend assumptions mid-session.

## Claude Code Configuration

Launching the macOS app is the recommended path. It installs a crash-safe, temporary managed `claude` command shim. New Claude Code sessions route through the proxy only while the app process is alive and the proxy health check passes.

Advanced users can still install permanent managed environment keys into `~/.claude/settings.json` through the CLI:

```sh
cc-codex-proxy auth login
cc-codex-proxy claude install-settings --model gpt-5.5[1m] --small-model gpt-5.4-mini[1m]
```

The command backs up `~/.claude/settings.json` before merging managed environment keys. Restore the newest backup with:

```sh
cc-codex-proxy claude restore-settings
```

The managed settings point Claude Code at the local proxy, set primary and Haiku/small model defaults, enable reasoning-effort requests for custom model names, and set the auto-compaction window to match the Codex context window.

### Transport Selection

Claude Code always talks to the local proxy over HTTP. Streaming responses are returned as Anthropic-compatible SSE.

The proxy's upstream Codex transport defaults to `auto`: it tries WebSocket first, then falls back to HTTP SSE if WebSocket setup fails. Override when needed:

```sh
export CCP_CODEX_TRANSPORT=auto       # default, try WebSocket first, then HTTP SSE
export CCP_CODEX_TRANSPORT=websocket  # fail hard if WebSocket is unavailable
export CCP_CODEX_TRANSPORT=http       # always use HTTP SSE
```

## Build From Source

Build a single app bundle that contains both the menu bar UI and the proxy helper:

```sh
scripts/build-app.sh
```

The output is:

- `dist/CCCodexProxy.app`
- `dist/CCCodexProxy.zip`

The app does not require a separate `cc-codex-proxy` command on `PATH`.

## Development

Rust is required to build the proxy/CLI. Swift is required to build the menu bar app.

```sh
cargo test
cargo run -p cc-codex-proxy -- serve
cargo run -p cc-codex-proxy -- auth login
cargo run -p cc-codex-proxy -- doctor
swift build --package-path macos/CCCodexProxy
```

Run the explicit 250-agent mock streaming stress test with:

```sh
cargo test -p proxy-core --test server_mock -- streaming_stress_250_agents --ignored --nocapture
```

## Runtime Files

- Config: `~/Library/Application Support/CCCodexProxy/config.json`
- Model profiles: `~/Library/Application Support/CCCodexProxy/model-profiles.json`
- Admin token: `~/Library/Application Support/CCCodexProxy/admin-token`
- Claude shim state: `~/Library/Application Support/CCCodexProxy/claude-shim.json`
- Logs: `~/Library/Logs/CCCodexProxy/proxy.log`
- Auth: `~/Library/Application Support/CCCodexProxy/auth.json`

## Architecture

The supported path is:

```text
Claude Code -> 127.0.0.1 Anthropic-compatible proxy -> ChatGPT subscription Codex Responses backend
```

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the runtime model, transport fallback strategy, request/response mapping, and robustness targets.

## Star History

<a href="https://www.star-history.com/#soulforger0/cc-codex-proxy&Date">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/svg?repos=soulforger0/cc-codex-proxy&type=Date&theme=dark" />
    <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/svg?repos=soulforger0/cc-codex-proxy&type=Date" />
    <img alt="Star History Chart" src="https://api.star-history.com/svg?repos=soulforger0/cc-codex-proxy&type=Date" />
  </picture>
</a>

## Contributing

Issues and pull requests are welcome. Before opening a PR, run the local checks that match your change:

```sh
cargo test --all
swift build --package-path macos/CCCodexProxy
```

For security-sensitive reports, please avoid posting secrets, OAuth tokens, or private session details in public issues.

## License

MIT © Ling Li. See [LICENSE](LICENSE).
