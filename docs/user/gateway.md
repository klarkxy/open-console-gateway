[简体中文](gateway.zh-CN.md)

# Gateway Behavior

Open Console Gateway exposes one HTTP surface on `127.0.0.1:9042` that speaks four client protocols and routes requests to whichever eligible OpenCode Go, Zen Free, Command Code GOAT, MiniMax CN, Kimi Code CN, Ollama Cloud, or Custom API account wins selection.

Ollama Cloud is a routable sealed fixed-origin Plan (`https://ollama.com`): Chat Completions only, Bearer. Its saved and raw catalog IDs do not join `GET /v1/models` or the Go Alias registry. An actual upstream 429 uses the generic cooldown and fallback path.

## Endpoints

The gateway listens on `http://<bind>:<port>` and exposes these endpoints:

| Method | Path | Purpose |
| --- | --- | --- |
| `POST` | `/v1/chat/completions` | OpenAI Chat Completions |
| `POST` | `/v1/responses` | OpenAI Responses |
| `POST` | `/v1/messages` | Anthropic Messages |
| `GET`  | `/v1/models` | Authenticated local list: routeable code-owned Go and sealed CN aliases, saved user-defined Provider public models, and eligible Custom IDs that currently have an effective enabled protocol, minus public names turned off on **Aliases** |
| `POST` | `/v1beta/models/{model}:generateContent` | Gemini non-stream generation (`/v1/models/...` is also accepted) |
| `POST` | `/v1beta/models/{model}:streamGenerateContent` | Gemini SSE generation (`/v1/models/...` is also accepted) |
| `POST` | `/v1beta/models/{model}:countTokens` | Returns `501`; Gemini CLI can fall back to local estimation |
| `POST` | `/v1beta/models/{model}:embedContent` | Returns `501`; embeddings are not supported |
| `GET`  | `/dashboard/` | Vue 3 dashboard (HTML) |
| `*`    | `/dashboard/api/v3/...` | 410 tombstone (`dashboardV3Removed`) |
| `*`    | `/dashboard/api/v4/...` | Live dashboard JSON control plane (CAS mutations, destinations, credentials, remounted operational handlers, DSH) |
| `*`    | `/dashboard/api/...` | V2 REST tombstone (authenticated 410 `dashboardV2Removed`), except the labeled V2 auth and browser-WebSocket compatibility routes |

Default bind is `127.0.0.1:9042`. Override with `serve --host 0.0.0.0` and `serve --port <port>` in the CLI. The desktop app also binds loopback and uses Tauri's single-instance lock so two tray icons do not fight over the port. There is no HTTP health endpoint; Docker only checks TCP `9042` from inside the container.

## Authentication

Gateway API endpoints need the **Key** in one of three header forms: `Authorization: Bearer <key>`, `x-api-key: <key>`, or `x-goog-api-key: <key>`. The gateway strips client authentication before forwarding and injects the selected account's credential instead. OpenCode Go sends `x-api-key` to Messages upstreams and `Authorization: Bearer` to Chat Completions / Responses. Configurable HTTP sends the authentication header saved for each route: Bearer, `x-api-key`, or `api-key`. The gateway does not forward dashboard or client credentials.

Dashboard auth depends on the listener bind. The current SPA uses `/dashboard/api/v4/auth/status`, `/dashboard/api/v4/auth/register`, `/dashboard/api/v4/auth/login`, and `/dashboard/api/v4/auth/logout`. Register, login, and logout need the same `expectedRevision` / `processGeneration` tokens as other V4 writes. The matching `/dashboard/api/auth/...` routes are labeled V2 compatibility routes for cached older pages.

- **Loopback binds (the default).** Dashboard API requests require a `Host` of `localhost` or a literal loopback IP; browser `Origin`, when present, must match that host and port. Cross-site requests are rejected, including registration and login. Valid local requests skip dashboard login unless they carry `Forwarded`, `x-forwarded-for`, `x-forwarded-proto`, `x-forwarded-host`, or `x-real-ip`; any of those headers requires login. Public-host reverse proxies must target a non-loopback listener. The client still needs the **Key** to reach the upstream endpoints. This is what the desktop app and the default CLI use.
- **Non-loopback binds.** A single administrator account, stored as an Argon2 password hash in SQLite, governs the dashboard. Sign-in returns an HttpOnly session cookie. Standard reverse-proxy forwarding headers on a non-loopback bind still require the cookie. In Docker, the first administrator can be bootstrapped with `OCG_ADMIN_USERNAME` and `OCG_ADMIN_PASSWORD`; otherwise the first registration wins.

## Aliases

Clients send **aliases**: stable lowercase kebab-case names from the local registry. Built-in Alias authority is code-owned: the static OpenCode Go protocol table plus sealed exact MiniMax CN, Kimi CN, and selected GOAT long-name maps. Case-folded Alias spellings such as `GLM-5.2` are accepted.

Authenticated `GET /v1/models` returns the currently routeable code-owned Aliases in registry order, then appends saved user-defined Provider public models and eligible Custom capability IDs that do not collide with those Aliases (`owned_by` is `custom`) and also have an effective enabled protocol. Public names turned off on **Aliases** are omitted from this list and remain routable. The list uses saved local state. Explicit catalog refreshes update saved Provider mappings and contracts. The list read does not write a forward log. Saved Zen `-free` rows keep the exact raw pin and publish the suffix-stripped Alias; saved Command rows may join any code-owned Alias; saved MiniMax/Kimi rows activate only exact sealed CN mappings. Command ids that contain `/` publish a unique last-segment lowercase kebab Alias; slash-free unmatched Command rows and unmatched MiniMax/Kimi rows cannot create arbitrary Aliases. Eligible Custom IDs come from enabled + ready Custom accounts that have a key (verification is optional).

Protected `GET /dashboard/api/v4/application-models` is a different local list: currently routeable OpenCode Go aliases intersected with the active OpenCode Go pricing snapshot. Highspeed variants inherit the base row. An empty intersection returns `[]`. The list excludes Custom IDs and uses saved local state.

`/v1/models` may publish shared Zen, Command Code, MiniMax, or Kimi mappings through a code-owned Alias, and may publish provider-only sealed Aliases. Command drops the Provider namespace, removes `-paid` / `-free` only when the shorter Alias is already authorized, and maps `nvidia/nemotron-3-ultra-550b-a55b` to `nemotron-3-ultra`; semantic qualifiers are not truncated by length. It publishes an Alias only while the exact saved catalog row exists and at least one Provider mapping has an enabled protocol. A Command/MiniMax/Kimi catalog ID with no code-owned Alias match remains available only as its exact raw ID and is not advertised as a new Alias. Eligible Custom declared IDs may appear even when they contain `/`; they are not folded into kebab aliases. `application-models` remains the narrower Go-and-pricing list.

A raw upstream ID with exactly one registry mapping is pinned to that mapping — no cross-Plan fallback or Zen prefer overlay — and routability is checked afterward. Built-in raw IDs are exact and case-sensitive. Names containing `/`, `_`, or whitespace are also never folded into kebab aliases (`glm/5.2` is not `glm-5.2`). Custom capability IDs keep their existing case-folded matching behavior. An exact raw ID that matches more than one mapping, including an eligible Custom capability and another Plan, returns `400` with code `ambiguous_model_id` and does not call upstream. Unknown names — neither an authorized alias, an exact saved built-in raw ID, nor an eligible Custom ID — return `400` on every supported client format: Chat Completions, Responses, Messages, and Gemini `generateContent` / `streamGenerateContent`. The canonical kebab alias `deepseek-v4-flash` can select among enabled Go, Zen, and Command Code mappings because it exists in the static Go table and has a matching Zen `-free` twin; the unique raw ID `deepseek/deepseek-v4-flash` pins only to Command Code. A Zen `foo-free` row always keeps the exact raw pin and publishes Alias `foo` from the `-free` suffix.

Forward logs separate the request identity from the upstream identity:

- `requested_model` — the public name or Alias the client sent
- `resolved_alias` — the resolved public Alias when one exists
- `upstream_model` — the exact model ID actually sent to that account's upstream

plus `provider_id`. Native cost fields are optional.

---

[User guide index](../USER.md) · [简体中文](gateway.zh-CN.md) · [Docs index](../README.md)
