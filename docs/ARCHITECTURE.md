# Architecture

CC Codex Proxy has three supported upstream paths:

- Claude Code -> `127.0.0.1` Anthropic-compatible proxy -> ChatGPT subscription Codex Responses backend.
- Claude Code -> `127.0.0.1` Anthropic-compatible proxy -> DeepSeek Anthropic-compatible API.
- Claude Code -> `127.0.0.1` Anthropic-compatible proxy -> Custom OpenAI-compatible Responses or Chat Completions API.

Provider selection is explicit through app/CLI config as `codex`, `deepseek`, or `custom-openai`. The proxy keeps small internal module boundaries for auth, request translation, upstream transport, stream handling, and Claude settings so each part can be tested in isolation.

## Runtime

- The shipped `CCCodexProxy.app` embeds the Rust proxy helper at `Contents/Helpers/cc-codex-proxy`.
- The menu bar process starts/stops that bundled helper; app users do not need a separate CLI install.
- On launch, the app temporarily replaces the shell-resolved `claude` command with a managed shim. The shim applies proxy environment variables only when the app PID is alive and `/healthz` succeeds; otherwise it either falls back to the original Claude command or reports that the proxy is stopped.
- Proxy startup is blocked while existing Claude Code processes are running, so active sessions do not silently switch backend assumptions mid-session.
- `cc-codex-proxy serve` binds only to `127.0.0.1`.
- `/v1/messages` streams Anthropic SSE back to Claude Code without buffering the full upstream response. Streaming responses include Claude-compatible `ping` keepalives and anti-buffering headers; the Messages endpoints also have an explicit bounded JSON body limit for large-but-controlled Claude Code transcripts.
- Codex non-streaming requests are accumulated only after the upstream stream completes; DeepSeek non-streaming responses are passed through as JSON. Custom OpenAI non-streaming responses are translated back into Anthropic JSON.
- Dropping the downstream response body drops the upstream request stream, so client disconnects cancel in-flight work promptly.
- Upstream response headers use provider-specific timeouts. Open upstream streams are monitored for long idle periods and warn by default; fatal stream-idle timeouts are configurable but disabled by default so legitimate long reasoning turns are not interrupted.
- Upstream 429/403/400 responses are returned to Claude Code as failures. The proxy does not queue or retry 429s.
- Graceful shutdown is bounded: the helper stops accepting new work, waits briefly for active requests, then aborts the server task if streams do not drain.

## Transport Selection

The Claude Code-facing side is always HTTP. Claude Code points `ANTHROPIC_BASE_URL` at the local server, sends Anthropic Messages requests to `/v1/messages`, and receives either JSON or Anthropic SSE depending on the request `stream` flag.

The Codex upstream side supports three modes through `codex.transport` or `CCP_CODEX_TRANSPORT`:

| Mode | Behavior | When to use |
| --- | --- | --- |
| `auto` | Try Codex WebSocket first. If WebSocket setup fails or no first event arrives promptly, fall back to HTTP SSE and suppress more WebSocket attempts for 120 seconds. | Default mode for app users. |
| `http` / `sse` | Use upstream HTTP SSE only. | Conservative mode for CI, debugging, restricted corporate networks, or any environment where WebSocket behavior is unreliable. |
| `websocket` / `ws` | Use upstream WebSocket only. | Diagnostics or explicit performance experiments where hard failure is preferred over fallback. |

Both upstream modes are reduced into the same internal byte stream and then translated into Anthropic-compatible events. The proxy only falls back before an upstream response stream is committed. Once streaming has started, it surfaces stream errors instead of replaying the request, because replaying a partially served agent turn can duplicate tool calls or chargeable work.

DeepSeek uses HTTPS only. It forwards Anthropic-shaped requests to `deepseek.base_url` plus `/v1/messages`; there is no WebSocket mode or transport fallback for DeepSeek.

Custom OpenAI-compatible endpoints use HTTPS or HTTP only. `custom_openai.base_url` may point at either a server root (for example `http://127.0.0.1:8000`), a `/v1` base, or the full endpoint path. `custom_openai.protocol` selects either `/v1/responses` (`responses`) or `/v1/chat/completions` (`chat-completions`). There is no WebSocket mode or transport fallback for custom endpoints.

## Fallback Strategy

- Transport fallback: `auto` demotes WebSocket to HTTP SSE for a short cooldown after setup failure or a silent first-event timeout. This avoids a per-request WebSocket timeout tax when a network, proxy, or upstream deployment rejects upgrades or accepts the socket without producing response events.
- Auth fallback: a Codex 401 forces one token refresh and one retry.
- DeepSeek auth and capacity errors are not retried. The proxy surfaces upstream `401`, `402`, `422`, `429`, `500`, `503`, and `Retry-After` directly to Claude Code.
- Custom OpenAI auth is optional. When configured, the proxy sends `Authorization: Bearer <key>`; when absent, requests are sent without an authorization header for local or unauthenticated gateways. Custom endpoint upstream errors and `Retry-After` are surfaced directly.
- Launch fallback: the managed `claude` shim only injects proxy environment variables while the app PID is alive and `/healthz` succeeds. If the app is gone, it launches the original Claude command without proxy variables. If the app is alive but the helper is unhealthy, it fails fast so new sessions do not start with inconsistent routing.
- Capacity fallback: 429, 403, 400, and `Retry-After` are passed through to Claude Code. The proxy does not queue, fan out, or retry rate-limited work because that would hide subscription limits and can amplify load.

Recommended setup: leave app users on `http`; use `auto` only for controlled reliability tests; use `websocket` only when validating WebSocket behavior directly.

## Auth

- Browser login uses OAuth PKCE against `auth.openai.com`.
- Tokens are stored in `~/Library/Application Support/CCCodexProxy/auth.json` with user-only file permissions.
- Access-token refresh is single-flight inside `AuthManager`.
- A 401 response from Codex forces one refresh and one retry.
- DeepSeek uses an API key from `DEEPSEEK_API_KEY` or `~/Library/Application Support/CCCodexProxy/deepseek-api-key`.
- Custom OpenAI uses an optional API key from `CUSTOM_OPENAI_API_KEY` or `~/Library/Application Support/CCCodexProxy/custom-openai-api-key`.
- Provider API keys are stored with user-only file permissions and are never written to Claude Code environment variables, shim state, admin JSON, or logs.

## Translation

- Claude Code messages, system prompts, tools, tool calls, tool results, images, JSON output formats, and reasoning effort are translated into the Codex Responses request shape.
- Custom OpenAI Responses mode reuses the same Responses request translation and stream reducer as Codex, but uses static bearer-token-or-no-auth HTTP instead of ChatGPT OAuth/WebSocket transport.
- Custom OpenAI Chat Completions mode maps Anthropic messages into `chat/completions` messages, translates tools into OpenAI function tools, maps tool calls/results, and converts OpenAI chat responses or stream deltas back into Anthropic content blocks.
- Hosted web search maps to Codex `web_search`.
- Unsupported reasoning stream events are dropped.
- Image blocks inside tool results become text placeholders because this proxy serializes function outputs as text for Codex compatibility.
- DeepSeek does not use the Codex translator. It receives the Anthropic request body directly after model resolution, local rejection of unsupported image/document blocks, and normalization of `output_config.effort` to DeepSeek's effective effort scale.

### Claude Code To Responses Mapping

The proxy intentionally implements the subset of Anthropic Messages semantics that Claude Code relies on for local agent work.

| Claude Code / Anthropic field | Codex Responses field | Notes |
| --- | --- | --- |
| `model` | `model` | Claude-facing aliases such as `claude-opus-4-8` and `claude-haiku-4-5` resolve to the active route's configured upstream model. Claude `[1m]` and proxy `-fast` hints are stripped before upstream. |
| top-level `system` | `instructions` | String and text-block arrays are joined into one instruction string. |
| message role `system` | developer `input[]` message | Mid-conversation system messages are preserved as Responses developer messages; they are not sent as role `system`. |
| user/assistant text blocks | `input[].content[].input_text` / `output_text` | Assistant history is preserved as Responses input items. |
| image blocks | `input_image.image_url` | Supports base64 data URLs and URL images. |
| `tool_use` | `function_call` | Preserves call id, tool name, and JSON arguments. |
| `tool_result` | `function_call_output` | Text is forwarded; image results become placeholders. |
| `tools[]` | `tools[]` with `type: "function"` | Anthropic `input_schema` becomes Responses `parameters`; `strict` is disabled for Claude Code compatibility. |
| `type: web_search_*`, `name: web_search` | `web_search` | Hosted web-search bridge. Sends `external_web_access: false`, `search_content_types: ["text", "image"]`, and non-empty `allowed_domains`/`blocked_domains` as `filters`. Anthropic `max_uses`, `response_inclusion`, `user_location`, and `search_context_size` are not forwarded. |
| `tool_choice` | `tool_choice` | `auto`, `none`, `any`, forced function tools, and forced `web_search` map to Responses equivalents. |
| `max_tokens` | omitted | The Codex backend rejects explicit output-limit parameters; Claude Code's field is not forwarded upstream. |
| `temperature`, `top_p` | omitted | The ChatGPT Codex backend is stricter than the public Responses API and rejects these sampling parameters on this path. |
| `metadata` | omitted | Anthropic request metadata is local client metadata; it is not forwarded as Responses `metadata` or `client_metadata`. |
| `output_config.effort` | `reasoning.effort` | `auto` omits the field; `max`/`ultracode` map to `xhigh`; `none`, `minimal`, `low`, `medium`, `high`, and `xhigh` are forwarded. Unknown values are omitted. |
| non-auto reasoning effort | `include: ["reasoning.encrypted_content"]` | Matches the Codex backend request shape used for reasoning continuity. |
| `thinking.budget_tokens` | `reasoning.effort` | Deprecated Claude fixed thinking budgets are mapped as a fallback: `0` -> `none`, up to 4k -> `low`, up to 32k -> `medium`, above 32k -> `high`. |
| `output_config.format.type=json_schema` | `text.format` | JSON schema output formatting with `strict: true`; object schemas are normalized so all properties are required. |
| `x-claude-code-session-id` | upstream session headers | Used to keep Codex cache/session behavior stable across a Claude Code conversation. The upstream session header uses a compact hash of the Claude session id so backend prompt-cache keys stay within length limits. It also keys local route pins so an idle session keeps using the provider/profile selected on its first request. The proxy does not send `prompt_cache_key` in the body on the ChatGPT Codex path. |

### DeepSeek Mapping

| Claude Code / Anthropic field | DeepSeek field | Notes |
| --- | --- | --- |
| `model` | `model` | Claude-facing aliases are resolved before forwarding, then the configured DeepSeek upstream model is sent. |
| messages, system, tools, tool choice, output config | same Anthropic field | Forwarded directly to DeepSeek's Anthropic-compatible API, except `output_config.effort` is normalized. |
| `output_config.effort` | `output_config.effort` | `auto` remains `auto`; `max` and `ultracode` become `max`; all other string effort values become `high`; absent or non-string values are left unchanged. |
| image/document blocks | rejected locally | DeepSeek's Anthropic-compatible API does not support those content blocks. |
| `stream` | `stream` | Streaming SSE and non-streaming JSON are passed through. |

### Context Compaction

Claude Code owns conversation compaction when it talks to a gateway. The proxy supports that path by exposing `/v1/messages/count_tokens` and by installing `CLAUDE_CODE_AUTO_COMPACT_WINDOW` so Claude Code compacts near the Codex context window rather than relying on a model-name heuristic. Token counting is local-only and based on the translated Codex Responses request shape; it does not call upstream token-count APIs.

OpenAI Responses also has server-side compaction features, but this proxy does not call `/responses/compact`, forward `context_management`, block requests on token thresholds, or synthesize compacted context. Claude Code sends a complete Anthropic-shaped transcript after its own compaction, and the proxy translates that transcript as the source of truth. When the already-local transcript shrinks sharply, the Codex route only advances the generated upstream session id (`session_id`, `x-client-request-id`, and `x-codex-window-id`) so the ChatGPT Codex backend does not continue from a larger pre-compaction server-side conversation.

Claude Code `/clear`, `/reset`, and `/new` are treated as new-conversation commands, not model prompts. On the Codex route, the proxy maps them to Codex `/new` semantics by advancing the generated upstream session id and returning an empty Anthropic response locally. It does not send `/clear`, `/reset`, or `/new` text to the upstream model.

### Status Line Metrics

Claude Code status-line scripts receive their JSON from Claude Code, not from this proxy. The proxy influences those metrics only through:

- `/v1/messages/count_tokens`, which drives Claude Code context estimates and compaction timing.
- Anthropic response `usage`, which maps Codex `input_tokens`, `output_tokens`, and `input_tokens_details.cached_tokens` into `input_tokens`, `output_tokens`, and `cache_read_input_tokens`.
- Installed model environment variables, which determine the active primary and small model names Claude Code reports.

ChatGPT Codex subscription cost and rate-limit state are not exposed through a stable Anthropic-compatible contract, so this proxy does not synthesize Claude Code `cost` or `rate_limits` values.

## Session Route Pins

With the default `pinOnFirstRequest` routing policy, a request carrying `x-claude-code-session-id` is pinned to the active route profile on first use. Pins are persisted in `route-pins.json`, have a configurable TTL, and are bounded with least-recently-seen eviction. This lets long-idle Claude Code sessions keep provider/model routing stable across profile switches or helper restarts. It is not byte-level stream resume: once a response stream has started, the proxy will not replay the upstream request after a disconnect because that could duplicate tool calls or chargeable work.

## Configuration

- App config: `~/Library/Application Support/CCCodexProxy/config.json`
- Model profiles: `~/Library/Application Support/CCCodexProxy/model-profiles.json`
- Admin token: `~/Library/Application Support/CCCodexProxy/admin-token`
- Claude shim state: `~/Library/Application Support/CCCodexProxy/claude-shim.json`
- Auth: `~/Library/Application Support/CCCodexProxy/auth.json`
- Session route pins: `~/Library/Application Support/CCCodexProxy/route-pins.json`
- DeepSeek API key: `~/Library/Application Support/CCCodexProxy/deepseek-api-key`
- Custom OpenAI API key: `~/Library/Application Support/CCCodexProxy/custom-openai-api-key`
- Logs: `~/Library/Logs/CCCodexProxy/proxy.log` — a single size-capped file. The proxy never creates rotated log archives; when the file would exceed `log.max_bytes` / `CCP_LOG_MAX_BYTES`, it truncates the same file and continues writing there.

Model names are intentionally data-driven and provider-scoped. If ChatGPT Codex, DeepSeek, or commonly used custom endpoint model identifiers change, update `model-profiles.json` instead of rebuilding. Custom OpenAI also accepts arbitrary model names after stripping Claude Code's `[1m]` context hint, so local gateways can be used before adding explicit profiles.

## Robustness Targets

- 100 concurrent local Claude Code-like sessions complete against a mock upstream.
- 250-session stress runs record latency, cancellation, memory, and file descriptor behavior.
- Long-idle sessions with stable `x-claude-code-session-id` values resume on their original route profile after helper restart.
- Streaming responses keep downstream clients alive with Claude-compatible `ping` events while upstream idle warnings make silent upstream stalls visible.
- Live upstream limits are treated as external constraints and surfaced to clients.
