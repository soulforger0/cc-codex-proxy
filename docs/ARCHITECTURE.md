# Architecture

CC Codex Proxy has one supported path:

Claude Code -> `127.0.0.1` Anthropic-compatible proxy -> ChatGPT subscription Codex Responses backend.

The proxy does not implement a generic provider abstraction. It keeps small internal module boundaries for auth, request translation, upstream transport, stream reduction, and Claude settings so each part can be tested in isolation.

## Runtime

- The shipped `CCCodexProxy.app` embeds the Rust proxy helper at `Contents/Helpers/cc-codex-proxy`.
- The menu bar process starts/stops that bundled helper; app users do not need a separate CLI install.
- `cc-codex-proxy serve` binds only to `127.0.0.1`.
- `/v1/messages` streams Anthropic SSE back to Claude Code without buffering the full upstream response.
- Non-streaming requests are accumulated only after the upstream stream completes.
- Dropping the downstream response body drops the upstream request stream, so client disconnects cancel in-flight work promptly.
- Upstream 429/403/400 responses are returned to Claude Code as failures. The proxy does not queue or retry 429s.

## Auth

- Browser login uses OAuth PKCE against `auth.openai.com`.
- Tokens are stored in macOS Keychain under `CCCodexProxy.codex`.
- Access-token refresh is single-flight inside `AuthManager`.
- A 401 response from Codex forces one refresh and one retry.

## Translation

- Claude Code messages, system prompts, tools, tool calls, tool results, images, JSON output formats, and reasoning effort are translated into the Codex Responses request shape.
- Hosted web search maps to Codex `web_search`.
- Unsupported reasoning stream events are dropped.
- Image blocks inside tool results become text placeholders because Codex function outputs are text-only.

## Configuration

- App config: `~/Library/Application Support/CCCodexProxy/config.json`
- Model profiles: `~/Library/Application Support/CCCodexProxy/model-profiles.json`
- Admin token: `~/Library/Application Support/CCCodexProxy/admin-token`
- Logs: `~/Library/Logs/CCCodexProxy/proxy.log`

Model names are intentionally data-driven. If ChatGPT Codex model identifiers change, update `model-profiles.json` instead of rebuilding.

## Robustness Targets

- 100 concurrent local Claude Code-like sessions complete against a mock upstream.
- 250-session stress runs record latency, cancellation, memory, and file descriptor behavior.
- Live upstream limits are treated as external constraints and surfaced to clients.
