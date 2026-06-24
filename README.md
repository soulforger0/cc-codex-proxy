# CC Codex Proxy

`cc-codex-proxy` is a local macOS proxy for running Claude Code against a ChatGPT subscription Codex backend. It exposes the Anthropic-compatible endpoints Claude Code expects and translates requests to the ChatGPT Codex Responses backend.

The implementation is intentionally ChatGPT/Codex-only. Kimi, Cursor, and generic OpenAI API-key routing are out of scope.

## Current Shape

- Self-contained SwiftUI menu bar app for macOS 13+.
- Rust/Tokio proxy helper embedded inside `CCCodexProxy.app/Contents/Helpers`.
- Rust CLI remains available for development, tests, and optional headless installs.
- Local-only proxy binding on `127.0.0.1`.
- OAuth tokens stored in macOS Keychain.
- Claude Code settings install/restore with timestamped backups.
- Mockable upstream boundary and load-test harness for 100+ concurrent local agents.

## Build The App

Build a single app bundle that contains both the menu bar UI and the proxy helper:

```sh
scripts/build-app.sh
```

The output is `dist/CCCodexProxy.app` and `dist/CCCodexProxy.zip`. The app does not require a separate `cc-codex-proxy` command on `PATH`.

## Development

```sh
cargo test
cargo run -p cc-codex-proxy -- serve
cargo run -p cc-codex-proxy -- auth login
cargo run -p cc-codex-proxy -- doctor
swift build --package-path macos/CCCodexProxy
```

Rust is required to build the proxy/CLI. Swift is required to build the menu bar app.

Run the explicit 250-agent mock streaming stress test with:

```sh
cargo test -p proxy-core --test server_mock -- streaming_stress_250_agents --ignored --nocapture
```

## Claude Code Configuration

Install managed Claude Code settings from the app, or use the development CLI after authenticating:

```sh
cc-codex-proxy auth login
cc-codex-proxy claude install-settings --model gpt-5.4 --small-model gpt-5.4-mini
```

The command backs up `~/.claude/settings.json` before merging managed environment keys. Restore the newest backup with:

```sh
cc-codex-proxy claude restore-settings
```

## Runtime Files

- Config: `~/Library/Application Support/CCCodexProxy/config.json`
- Model profiles: `~/Library/Application Support/CCCodexProxy/model-profiles.json`
- Admin token: `~/Library/Application Support/CCCodexProxy/admin-token`
- Logs: `~/Library/Logs/CCCodexProxy/proxy.log`
- Auth: macOS Keychain service `CCCodexProxy.codex`

## Warning

The ChatGPT subscription Codex backend is not a public OpenAI API contract. `doctor` verifies the current local auth and model/backend reachability, but backend behavior can change.
