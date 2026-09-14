[简体中文](overview.zh-CN.md)

# What Open Console Gateway Does

Open Console Gateway is a local gateway that stores provider API keys in a SQLite
database — including built-in Provider keys, trusted Custom API destinations,
and user-defined Provider definitions — and exposes a loopback gateway at
`http://127.0.0.1:9042/v1`. A Provider and a Plan are one product identity,
keyed only by `provider_id`; each account card belongs to one such Provider.
Clients send **aliases** from the local registry or eligible Custom model IDs;
live routing includes OpenCode Go, Zen Free, Command Code GOAT, MiniMax CN
Token Plan, Kimi Code CN, Custom API, and saved user-defined Providers. The Vue 3 dashboard is at
`/dashboard/` and the current SPA talks JSON at `/dashboard/api/v3`. Each node
stores its own data locally.

The gateway does four jobs, in roughly the order you would expect:

1. Authenticate the client with the **Key** issued by the dashboard.
2. Resolve the requested model against the local Alias registry (and eligible
   Custom declared IDs), then pick a usable account card after capability
   filtering, the adapter ceiling, the saved provider contract, and the
   per-model protocol effective state.
3. Convert the request to the selected Plan's effective upstream protocol,
   and the response back to the client protocol. Protocol selection uses the
   saved contract.
4. Log the request (`requested_model`, `resolved_alias`, `upstream_model`),
   write usage and any cooldown to SQLite, and surface everything in the
   dashboard.

## Shape of a node

Desktop, CLI, and Docker all run one `ocg-core` process on `127.0.0.1:9042`.
The dashboard opens in your system browser; clients hit `/v1` in OpenAI,
Anthropic, or Gemini format.

The consolidated [architecture diagram](architecture.md#one-local-node) shows
the control plane, inference plane, SQLite state, and the separate client-Key
and account-credential paths.

---

[User guide index](../USER.md) · [简体中文](overview.zh-CN.md) · [Docs index](../README.md)
