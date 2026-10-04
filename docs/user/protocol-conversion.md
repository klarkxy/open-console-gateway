[简体中文](protocol-conversion.zh-CN.md)

# Protocol Conversion

Open Console Gateway speaks four client protocols on one port, then translates
each request into whatever the upstream Plan actually understands. The
conversion layer is deterministic: it resolves the Alias, checks account
eligibility, applies the adapter ceiling and saved provider contract, checks
the per-model/per-protocol effective state, and only then passthroughs or
converts. Explicit upstream protocol disablement takes precedence over
baseline support.

Protocol selection uses the saved per-model preferred protocol and the
protocols that are already enabled, configured, and granted to the selected
Key. `MODEL_PROTOCOLS` stays an offline seed and shared-alias reference. The
tables below are that reference. They are not the live selector.

An explicit catalog refresh imports the official model list and protocol
baseline: Go and Zen use the Go documentation, Command Code uses its provider
documentation, and MiniMax CN / Kimi Code CN use their documented Chat and
Messages family. A failed fetch, or a model the document omits, adds no
protocol evidence and leaves the saved preference in place. Saved overrides
and probe evidence remain constrained by the sealed adapter. See
[Providers](providers.md) for refresh and enablement controls. Inference does
not refresh a catalog or send a request to discover another protocol.

For each attempt the gateway walks three local candidates before the send,
in this order: the saved preferred protocol when it is in the enabled,
configured, and granted set; then the client protocol when that protocol is
also in the set; then the remaining granted protocols in the saved model
order. Gemini is a client format and is never one of those upstream
protocols. A candidate whose conversion cannot preserve the required request
fields yields to the next candidate. The first candidate that preserves those
fields is the protocol for that attempt. The gateway does not negotiate the
protocol over HTTP, probe another protocol, or switch protocol after an
upstream HTTP 400. Credential and provider retries keep their existing
policy, and a later attempt makes its own local choice before it sends. The
**response body**, or the SSE stream, is converted back to the client
protocol when the upstream protocol differs. Without a saved preference, the
authorized client protocol is checked first; if it cannot preserve the
request or is not authorized, selection continues in the saved authorized
order. It is not rewritten to Chat Completions.

The same order covers every Providers-catalog supplier, including
user-defined Configurable HTTP mappings, Custom API, New API and Sub2API
linked Keys, and CPA. A site that can accept several upstream protocols still
uses the protocols saved and granted for that model. The gateway does not add
a protocol, a grant, or an endpoint to match the client. Conversion covers
text, system instructions, images, tool calls and results, reasoning content,
completion status, errors, and usage fields. SSE usage, errors, and terminal
state are parsed in event order, including responses that mix LF and CRLF
event separators.

The published profile on `GET /v1/models` is described in
[Model metadata](model-metadata.md). `ocg.protocols` names authorized upstream
protocols for eligible routes. It is derived for that response and is not a
stored negotiation table.

## Native opaque history

Signed Messages thinking, redacted thinking `data`, and encrypted Responses
content keep a stateless marker. The prefix is 79 ASCII bytes:
`ocg-replay-v1:`, 64 hexadecimal characters, and `:`. The original opaque
bytes follow and are not rewritten. The gateway hashes the exact observed
route with SHA-256: adapter, upstream protocol, upstream model, actual
request URL, credential id, credential version, destination, authorization
connection, binding, and authentication scheme. The marker contains no Key,
no other secret, no database record, and no HMAC. The marker establishes
equality with that configured observed route. The upstream still validates
the signed or encrypted content.

The gateway checks the marker before the HTTP request. A wrong route,
unmarked opaque history that is not empty, an unknown version, or a nested
or malformed marker is rejected. The gateway does not delete those fields to
let the request continue, and it does not guess a compatible reading of an
older conversation. Ordinary text and tool calls remain portable. On a
Messages request that stays on Messages, thinking whose signature is missing,
null, or empty stays in the same block after validation and system-role
hoisting. That native history is not silently deleted. A legacy helper that
converts without the route marker still removes those unsigned blocks. A
native client may turn unsigned thinking into plain text through its own
SDK. JSON and SSE both keep the signed field with the route that produced
it. If redaction would corrupt signed native content, the gateway returns an
error instead of a history that was rewritten and still succeeds.

The marker protects that opaque history only when the native client forwards it. External clients own conversation serialization. Their SDK can turn a native opaque field into plain text, or drop it, when the provider, API, or model identity changes. OCG cannot recover a missing field that never arrived, and it cannot detect that drop. Only the DSH plugin has a strict preflight that stops the base adapter from rewriting a foreign signed assistant message. Switching protocol group or client configuration does not migrate an old conversation. A new conversation, or history that has already been resolved, is required when that native identity changes. For marked history that does arrive, the same configured route is the bound of the guarantee: a direct change of upstream model, endpoint, or credential version is rejected before HTTP.

| Reference alias preference | Models |
| --- | --- |
| OpenAI Chat Completions | `glm-5.3-flash`, `glm-5.3`, `glm-5.2`, `glm-5.1`, `glm-5`, `kimi-k3`, `kimi-k2.7-code`, `kimi-k2.6`, `kimi-k2.5`, `deepseek-v4-pro`, `deepseek-v4-flash`, `deepseek-v4-flash-vision-exp`, `mimo-v2.5`, `mimo-v2.5-pro`, `hy3`, `longcat-2.0`, `big-pickle`, `deepseek-v4-flash-free`, `mimo-v2.5-free`, `nemotron-3-ultra-free`, `nemotron-3.5-lightning-free`, `ling-3.0-flash-fin-free`, `hy4-preview` |
| OpenAI Responses | `grok-4.6`, `grok-4.5`, `gpt-5.6-luna`, `muse-spark-1.2`, `muse-spark-1.2-contributor`, `muse-spark-1.2-contributor-free`, `muse-spark-1.3-contributor-free` |
| Anthropic Messages | `minimax-m3`, `minimax-m2.7`, `minimax-m2.7-highspeed`, `minimax-m2.5`, `minimax-m2.5-highspeed`, `qwen3.8-max`, `qwen3.8-flash`, `qwen3.7-max`, `qwen3.7-plus`, `qwen3.6-plus`, `qwen3.5-plus` |

Alias-profile reference (the checked-in 2026-09-06 preferences and 2026-08-27
Go `live_supported` paths). ✓ marks support in that code profile; it does not
promise current direct passthrough. Provider catalogs and effective contracts
decide whether a model and protocol are routable. The reference profiles live
in `MODEL_PROTOCOLS` in `crates/ocg-domain/src/protocol.rs`.

`reasoning.effort` aliases apply only on an OpenCode Go route that carries
this compatibility policy, before forwarding or conversion:
`muse-spark-1.2`, `muse-spark-1.2-contributor`,
`muse-spark-1.2-contributor-free`, and `muse-spark-1.3-contributor-free`
map `max` → `xhigh` (upstream rejects `max`). A user-defined HTTP route
keeps the original value even when the upstream model name matches. Other
models pass `reasoning.effort` through unchanged.

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
`streamGenerateContent`. See [Aliases](gateway.md#aliases).

Gateway protocol endpoints accept JSON request bodies up to 64 MiB by default.
Set `OCG_MAX_REQUEST_BODY_BYTES` to a positive integer in bytes (for example,
`134217728` for 128 MiB) before starting the desktop app, CLI, or container to
override this limit. Restart the process after changing it. Invalid, zero, or
out-of-range values produce a warning and fall back to 64 MiB. The setting is
environment-only and does not change Dashboard request limits.

This is a transport limit, not a context-window limit. Requests above it return
`413 Payload Too Large`. Larger limits allow more memory to be buffered per
concurrent request, including before authentication. If a reverse proxy sits
in front of Open Console Gateway, configure its limit to be at least as large,
or it may reject the request before the gateway sees it.

## Responses Is Stateless

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

## Gemini Is A Client-Only Format

Gemini is a client format: the gateway converts `contents`,
text-only `systemInstruction`, supported `inlineData` images,
`functionDeclarations`, function calls/results, JSON-schema output,
generation options, Google error envelopes, usage metadata, and SSE frames to
and from the upstream protocol chosen for that attempt. Both
the `v1beta` and `v1` URL forms are accepted.

Unconvertible fields return `400`:

- Non-empty `safetySettings` return `400 INVALID_ARGUMENT`, because a
  different upstream protocol cannot preserve their safety semantics.
  Omitted, `null`, and `[]` are accepted. Do not treat `safetySettings` as a
  hint the upstream will enforce.
- A non-null `generationConfig.topK` or `generationConfig.thinkingConfig`
  is rejected before HTTP. Conversion has no exact form for either value.
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

---

[User guide index](../USER.md) · [简体中文](protocol-conversion.zh-CN.md) · [Docs index](../README.md)
