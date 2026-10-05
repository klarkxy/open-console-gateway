[简体中文](overview.zh-CN.md)

# What Open Console Gateway Does

This generation is OCG3. `ocg3` and `open-console-gateway` are two generation branches of one project; the product name stays Open Console Gateway. The operator command is the headless `ocg` CLI (`ocg.exe` on Windows, `ocg` on Linux and macOS). Execution is one local CPA owned by that process. Custom HTTP providers remain provider routes on that local CPA. The full CLI is the current deliverable and its acceptance is pending. The native GPUI/Ely GUI is postponed. Commands are in the [CLI guide](cli.md). The design is in the [architecture](../architecture.md).

Public inference in the current source is handed to that owned local CPA through the CPA ingress. That ingress is not an accepted runtime. This page does not record a successful run or a completed migration.

## Published open-console-gateway generation

The explanation below is the published `open-console-gateway` generation. It is the retained product description for that generation, not the current `ocg` package.

In that generation, Open Console Gateway was a local gateway that stores provider API keys in a SQLite database — including built-in Provider keys, trusted Custom API destinations, and user-defined Provider definitions — and exposes a loopback gateway at `http://127.0.0.1:9042/v1`. A Provider and a Plan are one product identity, keyed only by `provider_id`; each account card belongs to one such Provider. Clients send **aliases** from the local registry or eligible Custom model IDs. Routing in that generation included OpenCode Go, Zen Free, Command Code GOAT, MiniMax CN Token Plan, Kimi Code CN, Ollama Cloud, Custom API, the CPA subscription pool, and saved user-defined Providers. The Vue 3 dashboard was at `/dashboard/`, and that SPA talked JSON at `/dashboard/api/v4` only (`/dashboard/api/v3` is a 410 tombstone). Each node stores its own data locally.

The published gateway explanation has four jobs, in roughly the order an operator would expect:

1. Authenticate the client with the **Key** issued by the dashboard.
2. Resolve the requested model against the local Alias registry (and eligible Custom declared IDs), then pick a usable account card after capability filtering, the adapter ceiling, the saved provider contract, and the per-model protocol effective state.
3. Convert the request to the selected Plan's effective upstream protocol, and the response back to the client protocol. Protocol selection uses the saved contract.
4. Log the request (`requested_model`, `resolved_alias`, `upstream_model`), write usage and any cooldown to SQLite, and surface everything in the dashboard.

### Shape of a node

Desktop, CLI, and Docker of that generation each ran one `ocg-core` process on `127.0.0.1:9042`. The dashboard opened in the system browser. Clients called `/v1` in OpenAI, Anthropic, or Gemini format. Those desktop and container packages are that generation's published packages.

---

[User guide index](../USER.md) · [简体中文](overview.zh-CN.md) · [Docs index](../README.md)
