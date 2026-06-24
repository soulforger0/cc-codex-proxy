# Architecture

CC Codex Proxy has one supported path:

Claude Code -> `127.0.0.1` Anthropic-compatible proxy -> ChatGPT subscription Codex Responses backend.

The proxy does not implement a generic provider abstraction. It keeps small internal module boundaries for auth, request translation, upstream transport, stream reduction, and Claude settings so each part can be tested in isolation.

## Runtime

- The shipped `CCCodexProxy.app` embeds the Rust proxy helper at `Contents/Helpers/cc-codex-proxy`.
- The menu bar process starts/stops that bundled helper; app users do not need a separate CLI install.
- On launch, the app temporarily replaces the shell-resolved `claude` command with a managed shim. The shim applies proxy environment variables only when the app PID is alive and `/healthz` succeeds; otherwise it either falls back to the original Claude command or reports that the proxy is stopped.
- `cc-codex-proxy serve` binds only to `127.0.0.1`.
- `/v1/messages` streams Anthropic SSE back to Claude Code without buffering the full upstream response.
- Non-streaming requests are accumulated only after the upstream stream completes.
- Dropping the downstream response body drops the upstream request stream, so client disconnects cancel in-flight work promptly.
- Upstream 429/403/400 responses are returned to Claude Code as failures. The proxy does not queue or retry 429s.

## Auth

- Browser login uses OAuth PKCE against `auth.openai.com`.
- Tokens are stored in `~/Library/Application Support/CCCodexProxy/auth.json` with user-only file permissions.
- Access-token refresh is single-flight inside `AuthManager`.
- A 401 response from Codex forces one refresh and one retry.

## Translation

- Claude Code messages, system prompts, tools, tool calls, tool results, images, JSON output formats, and reasoning effort are translated into the Codex Responses request shape.
- Hosted web search maps to Codex `web_search`.
- Unsupported reasoning stream events are dropped.
- Image blocks inside tool results become text placeholders because this proxy serializes function outputs as text for Codex compatibility.

### Claude Code To Responses Mapping

The proxy intentionally implements the subset of Anthropic Messages semantics that Claude Code relies on for local agent work.

| Claude Code / Anthropic field | Codex Responses field | Notes |
| --- | --- | --- |
| `model` | `model` | Resolved through `model-profiles.json`; Claude `[1m]` and proxy `-fast` hints are stripped before upstream. |
| `system` | `instructions` | String and text-block arrays are joined into one instruction string. |
| user/assistant text blocks | `input[].content[].input_text` / `output_text` | Assistant history is preserved as Responses input items. |
| image blocks | `input_image.image_url` | Supports base64 data URLs and URL images. |
| `tool_use` | `function_call` | Preserves call id, tool name, and JSON arguments. |
| `tool_result` | `function_call_output` | Text is forwarded; image results become placeholders. |
| `tools[]` | `tools[]` with `type: "function"` | Anthropic `input_schema` becomes Responses `parameters`; `strict` is disabled for Claude Code compatibility. |
| `type: web_search_*`, `name: web_search` | `web_search` | Hosted web-search bridge. `allowed_domains`/`blocked_domains` map to `filters`; `user_location` is forwarded. Anthropic `max_uses` and `response_inclusion` have no direct Responses equivalent and are not forwarded. |
| `tool_choice` | `tool_choice` | `auto`, `none`, `any`, forced function tools, and forced `web_search` map to Responses equivalents. |
| `max_tokens` | `max_output_tokens` | Output limit only. |
| `temperature`, `top_p` | same names | Forwarded when Claude Code sends them. |
| `metadata` | `metadata` | Forwarded unchanged. |
| `output_config.effort` | `reasoning.effort` | `auto` omits the field; `max`/`ultracode` map to `xhigh`; `none`, `minimal`, `low`, `medium`, `high`, and `xhigh` are forwarded. Unknown values are omitted. |
| `thinking.budget_tokens` | `reasoning.effort` | Deprecated Claude fixed thinking budgets are mapped as a fallback: `0` -> `none`, up to 4k -> `low`, up to 32k -> `medium`, above 32k -> `high`. |
| `output_config.format.type=json_schema` | `text.format` | JSON schema output formatting. |
| `x-claude-code-session-id` | `prompt_cache_key` and upstream session headers | Used to keep Codex cache/session behavior stable across a Claude Code conversation. |

### Context Compaction

Claude Code owns conversation compaction when it talks to a gateway. The proxy supports that path by exposing `/v1/messages/count_tokens` and by installing `CLAUDE_CODE_AUTO_COMPACT_WINDOW` so Claude Code compacts near the Codex context window rather than relying on a model-name heuristic.

OpenAI Responses also has server-side compaction features, but this proxy does not currently call `/responses/compact`. Claude Code sends a complete Anthropic-shaped transcript after its own compaction, and the proxy translates that transcript as the source of truth.

### Status Line Metrics

Claude Code status-line scripts receive their JSON from Claude Code, not from this proxy. The proxy influences those metrics only through:

- `/v1/messages/count_tokens`, which drives Claude Code context estimates and compaction timing.
- Anthropic response `usage`, which maps Codex `input_tokens`, `output_tokens`, and `input_tokens_details.cached_tokens` into `input_tokens`, `output_tokens`, and `cache_read_input_tokens`.
- Installed model environment variables, which determine the active primary and small model names Claude Code reports.

ChatGPT Codex subscription cost and rate-limit state are not exposed through a stable Anthropic-compatible contract, so this proxy does not synthesize Claude Code `cost` or `rate_limits` values.

## Configuration

- App config: `~/Library/Application Support/CCCodexProxy/config.json`
- Model profiles: `~/Library/Application Support/CCCodexProxy/model-profiles.json`
- Admin token: `~/Library/Application Support/CCCodexProxy/admin-token`
- Claude shim state: `~/Library/Application Support/CCCodexProxy/claude-shim.json`
- Auth: `~/Library/Application Support/CCCodexProxy/auth.json`
- Logs: `~/Library/Logs/CCCodexProxy/proxy.log`

Model names are intentionally data-driven. If ChatGPT Codex model identifiers change, update `model-profiles.json` instead of rebuilding.

## Robustness Targets

- 100 concurrent local Claude Code-like sessions complete against a mock upstream.
- 250-session stress runs record latency, cancellation, memory, and file descriptor behavior.
- Live upstream limits are treated as external constraints and surfaced to clients.
