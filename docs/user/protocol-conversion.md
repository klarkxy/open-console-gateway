[简体中文](protocol-conversion.zh-CN.md)

# Protocol Conversion

Open Console Gateway speaks five client protocols on one port, then translates each
request into whatever the upstream Plan actually understands. The conversion
layer is deterministic: it resolves the Alias, checks account
eligibility, applies the adapter ceiling and saved provider contract, checks
the per-model/per-protocol effective state, and only then passthroughs or
converts. Explicit upstream protocol disablement takes precedence over
baseline support.

Protocol selection uses the saved provider contract. Explicit catalog refresh
imports the official model list and protocol baseline: Go and Zen use the Go
documentation, Command Code uses its provider documentation, and MiniMax CN / Kimi
Code CN use their documented Chat and Messages family. Go, Zen, and Command Code
fall back to Chat when the document omits a model or cannot be fetched. Saved
overrides and probe evidence remain constrained by the sealed adapter. See
[Providers](providers.md) for refresh and enablement controls; inference never
refreshes a catalog or tries another upstream protocol to discover support.

If the client protocol is enabled in the current model contract, request and
response pass through. Otherwise the gateway converts the **request body** to
the enabled preferred protocol, or the first remaining enabled protocol in adapter
fallback order, and the **response body** — or SSE stream — back to the client
protocol. Gemini always converts to an enabled upstream protocol. This rule
applies to every Providers-catalog supplier, including user-defined
Configurable HTTP mappings (one protocol per mapping). Custom API
does the same to the account's declared upstream protocol, then honors that
endpoint's contract and per-model overrides. CPA is unchanged and is not
part of this conversion-default control. Conversion covers text, system
instructions, images, tool calls and results, reasoning content, completion
status, errors, and usage fields. SSE usage, errors, and terminal state are
parsed in event order, including responses that mix LF and CRLF event separators.

The tables below describe code-owned alias profiles, not current provider
availability. Refresh-written official baselines and saved enablement determine
the actual default and available protocols shown on **Providers**.

| Reference alias preference | Models |
| --- | --- |
| OpenAI Chat Completions | `glm-5.3-flash`, `glm-5.3`, `glm-5.2`, `glm-5.1`, `glm-5`, `kimi-k3`, `kimi-k2.7-code`, `kimi-k2.6`, `kimi-k2.5`, `deepseek-v4-pro`, `deepseek-v4-flash`, `deepseek-v4-flash-vision-exp`, `mimo-v2.5`, `mimo-v2.5-pro`, `hy3`, `longcat-2.0`, `big-pickle`, `deepseek-v4-flash-free`, `mimo-v2.5-free`, `nemotron-3-ultra-free`, `nemotron-3.5-lightning-free`, `ling-3.0-flash-fin-free`, `hy4-preview` |
| OpenAI Responses | `grok-4.6`, `grok-4.5`, `gpt-5.6-luna`, `muse-spark-1.2`, `muse-spark-1.2-contributor`, `muse-spark-1.2-contributor-free`, `muse-spark-1.3-contributor-free` |
| Anthropic Messages | `minimax-m3`, `minimax-m2.7`, `minimax-m2.7-highspeed`, `minimax-m2.5`, `minimax-m2.5-highspeed`, `qwen3.8-max`, `qwen3.8-flash`, `qwen3.7-max`, `qwen3.7-plus`, `qwen3.6-plus`, `qwen3.5-plus` |

Alias-profile reference (the checked-in 2026-09-06 preferences and 2026-08-27
Go `live_supported` paths). ✓ marks support in that code profile; it does not
promise current direct passthrough. Provider catalogs and effective contracts
decide whether a model and protocol are routable. The reference profiles live in
`MODEL_PROTOCOLS` in `crates/ocg-domain/src/protocol.rs`.

`reasoning.effort` aliases (applied before forwarding or conversion):
`muse-spark-1.2`, `muse-spark-1.2-contributor`,
`muse-spark-1.2-contributor-free`, and `muse-spark-1.3-contributor-free`
map `max` → `xhigh` (upstream rejects `max`). Other models pass
`reasoning.effort` through unchanged.

| Model | Preferred | Chat | Responses | Messages |
| --- | --- | :---: | :---: | :---: |
| `grok-4.6` | Responses | | ✓ | |
| `grok-4.5` | Responses | | ✓ | |
| `glm-5.3-flash` | Chat | ✓ | | |
| `glm-5.3` | Chat | ✓ | | |
| `glm-5.2` | Chat | ✓ | | |
| `glm-5.1` | Chat | ✓ | | |
| `glm-5` | Chat | ✓ | | |
| `gpt-5.6-luna` | Responses | | ✓ | |
| `muse-spark-1.2` | Responses | | ✓ | |
| `muse-spark-1.2-contributor` | Responses | | ✓ | |
| `muse-spark-1.2-contributor-free` | Responses | | ✓ | |
| `muse-spark-1.3-contributor-free` | Responses | | ✓ | |
| `kimi-k3` | Chat | ✓ | | ✓ |
| `kimi-k2.7-code` | Chat | ✓ | | |
| `kimi-k2.6` | Chat | ✓ | | |
| `kimi-k2.5` | Chat | ✓ | | |
| `deepseek-v4-pro` | Chat | ✓ | ✓ | ✓ |
| `deepseek-v4-flash` | Chat | ✓ | ✓ | ✓ |
| `deepseek-v4-flash-vision-exp` | Chat | ✓ | ✓ | ✓ |
| `mimo-v2.5` | Chat | ✓ | | |
| `mimo-v2.5-pro` | Chat | ✓ | | |
| `hy3` | Chat | ✓ | | |
| `longcat-2.0` | Chat | ✓ | | |
| `big-pickle` | Chat | ✓ | | |
| `deepseek-v4-flash-free` | Chat | | | |
| `mimo-v2.5-free` | Chat | ✓ | | |
| `nemotron-3-ultra-free` | Chat | ✓ | | |
| `nemotron-3.5-lightning-free` | Chat | ✓ | | |
| `ling-3.0-flash-fin-free` | Chat | ✓ | | |
| `hy4-preview` | Chat | ✓ | | |
| `minimax-m3` | Messages | ✓ | | ✓ |
| `minimax-m2.7` | Messages | | | ✓ |
| `minimax-m2.7-highspeed` | Messages | | | |
| `minimax-m2.5` | Messages | ✓ | | ✓ |
| `minimax-m2.5-highspeed` | Messages | | | |
| `qwen3.8-max` | Messages | ✓ | | ✓ |
| `qwen3.8-flash` | Messages | | | ✓ |
| `qwen3.7-max` | Messages | ✓ | | ✓ |
| `qwen3.7-plus` | Messages | ✓ | | ✓ |
| `qwen3.6-plus` | Messages | ✓ | | ✓ |
| `qwen3.5-plus` | Messages | ✓ | | ✓ |

Unknown model names return `400` on every supported client format — Chat
Completions, Responses, Messages, and Gemini `generateContent` /
`streamGenerateContent` — and unknown Claude Desktop aliases do too. See
[Aliases](gateway.md#aliases).

Gateway protocol endpoints accept JSON request bodies up to 64 MiB by default.
Set `OCG_MAX_REQUEST_BODY_BYTES` to a positive integer in bytes (for example,
`134217728` for 128 MiB) before starting the desktop app, CLI, or container to
override this limit. Restart the process after changing it. Invalid, zero, or
out-of-range values produce a warning and fall back to 64 MiB. The setting is
environment-only and does not change Dashboard request limits.

This is a transport limit, not a context-window limit. Requests above it return
`413 Payload Too Large`. Larger limits allow more memory to be buffered per
concurrent request, including before authentication. If a reverse proxy sits in
front of Open Console Gateway, configure its limit to be at least as large or it
may reject the request before the gateway sees it.

## Responses is stateless

The following fields return `400` instead of being silently ignored:

- `previous_response_id`
- `conversation`
- `store: true` or any `store` value other than `false`
- `background: true`
- `input_image.file_id` (the gateway has no Files API)

Function, custom, and namespace tools convert normally. Hosted tools such as
`web_search`, `web_search_preview`, and `tool_search` cannot run on a converted
OpenCode-Go path. If they are the only tools or are forced, the gateway
returns `400` before outbound instead of stripping them and continuing.
When function tools remain, hosted declarations may still drop under the
versioned `legacy_compat` profile, and that downgrade is recorded; stored
protocol configuration is not rewritten. Native Responses passthrough keeps
hosted tools.

## Gemini is a client-only format

Gemini is a client format: the gateway converts `contents`,
text-only `systemInstruction`, supported `inlineData` images,
`functionDeclarations`, function calls/results, JSON-schema output,
generation options, Google error envelopes, usage metadata, and SSE frames to
and from the known model's native Chat Completions or Messages protocol. Both
the `v1beta` and `v1` URL forms are accepted.

Unconvertible fields return `400`:

- Non-empty `safetySettings` return `400 INVALID_ARGUMENT`, because a
  different upstream protocol cannot preserve their safety semantics.
  Omitted, `null`, and `[]` are accepted. Do not treat `safetySettings` as a
  hint the upstream will enforce.
- `generationConfig.topK` and `generationConfig.thinkingConfig` are accepted
  as cross-protocol compatibility hints only; sampling, reasoning budgets,
  and thought display are not guaranteed equivalent to a native Gemini
  backend and depend on the selected OpenCode-Go model.
- Other non-null generation options that cannot be preserved — including
  `seed`, presence/frequency penalties, log-probability controls, and media
  resolution — return `400` instead of being silently discarded.
- `cachedContent`, `fileData`, Google Search, URL Context, Code Execution,
  multimodal function-response parts, function response schemas/behavior,
  `VALIDATED` function calling, candidate counts other than one, and response
  modalities other than `TEXT` return `400`. Use base64 `inlineData` for PNG,
  JPEG, GIF, or WebP images.
- `countTokens` and `embedContent` return `501 UNIMPLEMENTED`; Gemini CLI can
  fall back to local token estimation, and the gateway has no embeddings
  route.

## Claude Desktop aliases

The dedicated entry accepts only the advertised aliases
`claude-sonnet-4-6`, `claude-opus-4-6`, and `claude-haiku-4-5-20251001`.
Before entering the existing Messages conversion path, the gateway rewrites
the alias to the actual model in the stored `sonnet` / `opus` / `haiku`
mapping. Model capabilities, tool support, and context limits in the response
still follow the actual model. The mapping is serialized inside `AppConfig`
and updated through `PUT /dashboard/api/v3/claude-desktop/models`; omitted
roles inherit the first configured role, and the dashboard returns the
resolved three-role mapping.

---

[User guide index](../USER.md) · [简体中文](protocol-conversion.zh-CN.md) · [Docs index](../README.md)
