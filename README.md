# CC Codex Proxy

`cc-codex-proxy` is a local macOS proxy for running Claude Code against a ChatGPT subscription Codex backend. It exposes the Anthropic-compatible endpoints Claude Code expects and translates requests to the ChatGPT Codex Responses backend.

The implementation is intentionally ChatGPT/Codex-only. Kimi, Cursor, and generic OpenAI API-key routing are out of scope.

## Current Shape

- Self-contained SwiftUI menu bar app for macOS 13+.
- Rust/Tokio proxy helper embedded inside `CCCodexProxy.app/Contents/Helpers`.
- Rust CLI remains available for development, tests, and optional headless installs.
- Local-only proxy binding on `127.0.0.1`.
- OAuth tokens stored in a local user-only auth file.
- App-managed Claude Code command shim, plus advanced settings install/restore with timestamped backups.
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

Launching the macOS app installs a temporary managed `claude` command shim. New `claude` sessions route through the proxy only while the app is running and the proxy health check passes. If the app quits cleanly, the original `claude` command is restored. If the app crashes, the shim falls back to the original Claude command without proxy environment variables.

The proxy will not start while existing Claude Code sessions are running. Close active Claude Code sessions first, then start the proxy so new sessions consistently inherit the proxy environment.

The permanent `~/.claude/settings.json` workflow remains available as an advanced option in the app, or through the development CLI after authenticating:

```sh
cc-codex-proxy auth login
cc-codex-proxy claude install-settings --model gpt-5.4 --small-model gpt-5.4-mini
```

The command backs up `~/.claude/settings.json` before merging managed environment keys. Restore the newest backup with:

```sh
cc-codex-proxy claude restore-settings
```

The managed settings point Claude Code at the local Anthropic-compatible proxy, set primary and Haiku/small model defaults, enable reasoning-effort requests for the custom model names, and set the auto-compaction window to match the Codex context window.

### Transport Selection

Claude Code always talks to the local proxy over HTTP. Streaming responses are returned as Anthropic-compatible SSE.

The proxy's upstream Codex transport defaults to `auto`: try WebSocket first, then fall back to HTTP SSE for a cooldown period if WebSocket setup fails. Override when needed:

```sh
export CCP_CODEX_TRANSPORT=http       # most conservative
export CCP_CODEX_TRANSPORT=websocket  # fail hard if WebSocket is unavailable
export CCP_CODEX_TRANSPORT=auto       # default
```

## Runtime Files

- Config: `~/Library/Application Support/CCCodexProxy/config.json`
- Model profiles: `~/Library/Application Support/CCCodexProxy/model-profiles.json`
- Admin token: `~/Library/Application Support/CCCodexProxy/admin-token`
- Claude shim state: `~/Library/Application Support/CCCodexProxy/claude-shim.json`
- Logs: `~/Library/Logs/CCCodexProxy/proxy.log`
- Auth: `~/Library/Application Support/CCCodexProxy/auth.json`

## Warning

The ChatGPT subscription Codex backend is not a public OpenAI API contract. `doctor` verifies the current local auth and model/backend reachability, but backend behavior can change.
