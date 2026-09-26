[简体中文](logs-settings.zh-CN.md)

# Logs And Settings

## Logs

The **Logs** view opens on **Request Logs** and is the rolling receipt tape for
requests the gateway forwards plus explicit provider protocol probes: timestamp,
selected provider, route account, credential account, model, status
code, the upstream error if any, and the streamed usage when the upstream emits
a usage chunk. Probe rows carry zero token values and no applicable cost, have no client Key
attribution, and never appear in Runtime Logs. Filters cover provider, route
account, credential account, model, status, time range, and client Key.
Authenticated parse, validation, or routing failures that happen before account
selection also appear here with unresolved/Gateway attribution. Runtime Logs are
reserved for process and control-plane events.
Each stored row keeps the request identity separate from the upstream
identity:

- `requested_model` — the public name or Alias the client sent
- `resolved_alias` — the resolved public Alias when one exists
- `upstream_model` — the exact model ID actually sent to that account's upstream

plus `provider_id`. The model filter exact-matches
any of those identities or the `model` column. Native cost
(`native_cost_value`, `native_cost_unit`, `native_cost_currency`) is optional
and present only when the provider supplies enough pricing evidence.

Each row also stores raw supplier cost, quota debit, and effective paid cost
when the selected provider supplies enough pricing evidence. An allowance only
changes the quota-debit multiplier.

- Chat streaming requests set `stream_options.include_usage` so OpenAI-compatible
  upstreams emit a usage chunk. Rows with `success_no_usage` mean the stream
  still finished without one. A usage chunk makes token counts accurate; the
  summary shows total tokens (input + output). Quota use is estimated from the
  selected provider's verified pricing snapshot: OpenCode Go uses its active
  snapshot, while Command Code GOAT uses its separately refreshed model prices
  and multipliers. Ollama Cloud prices from the manual `https://ollama.com/pricing`
  snapshot with quota multiplier `1.0`; those priced rows feed one monthly
  USD-credits window and may exceed the soft Pro/Max/Team limit without changing
  routing. Existing rows are not retroactively repriced. Registered
  Zen free models (`big-pickle`, `mimo-v2.5-free`, and other ids on the Zen
  allowlist) record tokens with `cost_state=free` and do not enter Go quota
  totals. Custom API rows record `cost_state=unknown` with no provider quota
  debit. The list shows time, attempt, model alias, and status. Expand a row to
  see the plan, account, request ID, and diagnostic
  detail. A completed request without a price is shown as success; missing
  prices are ordinary and are not a separate status. Filtering by success
  includes those rows. The stored status can still be `success_unpriced`, and
  its quota cost stays empty.
- An `outcome_unknown` row means the upstream may already have completed and
  charged the request, but the gateway lost the response or timed out. Such a
  request is not replayed automatically and its local cost remains unknown.
- The **Key** filter narrows rows and the summary totals to one client key.
  Options come from the log table itself, so disabled, deleted, and otherwise
  unknown keys stay filterable. **Unattributed** selects rows with no client-Key
  attribution; a background task attributes them to the primary key as an
  approximation.

## Settings

Routing mode and conversation sticky are configured on **Accounts**, above the account list; see [Accounts](accounts.md).

The **Settings** view holds the gateway's persistent configuration:

- **Gateway Port** — the port the gateway binds (default `9042`). Desktop builds
  also accept the read-only `OCG_GATEWAY_PORT` runtime override; while it is set,
  the Settings field is disabled and the saved value is unchanged.
- **Outbound proxy** — shared by every account. Automatic, manual, and force
  direct apply one process-wide policy; **Per-model list** (below) splits chat
  forwarding by model instead.
  `Automatic (system / environment)` reads `HTTP_PROXY`, `HTTPS_PROXY`,
  `ALL_PROXY`, and `NO_PROXY`; Windows also reads the system proxy and connects
  directly when none is configured. `Manual HTTP proxy` strictly routes all
  HTTP/HTTPS targets through one `http://` or `https://` proxy such as
  `http://127.0.0.1:7890`; a proxy failure never silently falls back to a direct
  connection. `Force direct connection` ignores system and environment proxy
  configuration. Proxy URLs cannot contain credentials. For these three modes,
  the policy covers model forwarding (OpenCode Go, Zen Free, Command Code
  GOAT, MiniMax CN, Kimi Code CN, and Custom API),
  account-key tests and Custom verification, official OpenCode Go usage API,
  pricing refreshes, release checks, and signed desktop installer downloads;
  authenticated `GET /v1/models` and protected
  `GET /dashboard/api/v4/application-models` are local lists and do not use
  this outbound path. The browser sidecar is outside its scope. **Test
  connection** uses the unsaved form values against the sealed OpenCode Go
  origin. Any
  HTTP status proves network reachability, without running model inference or
  incurring model usage. In list mode it probes only the
  direction's default leg, not a listed model's real forwarding path.
- **Per-model list** (fourth proxy mode) — routes chat forwarding per model
  instead of process-wide. Pick a direction and check exact upstream model IDs
  from enabled Provider contracts, eligible Custom capabilities, user-defined
  Provider mappings, and the active CPA catalog. Public Aliases are not added;
  the list accepts no patterns or free-text. An old saved ID that disappears
  from those sources remains inert and is removed on the next save. With the
  **whitelist** direction, listed models
  connect through the proxy URL while every unlisted model
  connects directly (ignoring system/environment proxies, exactly like force
  direct). The **blacklist** direction inverts this: listed models connect
  directly and everything else uses the proxy URL. Both directions require the
  proxy URL; an empty list or an empty URL cannot be saved. Non-chat outbound
  traffic (pricing refreshes, official usage sync, update checks, and signed
  downloads) always follows the direction's default leg: direct for a
  whitelist, the proxy URL for a blacklist — so switching from `Manual HTTP
  proxy` to a whitelist changes that traffic to direct. The account-key test
  and **Test connection** likewise probe the default leg, so they do not
  represent the real forwarding path of a listed model. Free-channel models
  can be listed, but Zen free quota is shared by egress IP, so routing them
  through a proxy changes which quota they draw from. Every forward-log row
  (successes included) records the leg it used — `proxy`, `direct`, or `auto`
  — in its expanded details; rows without a recorded leg show "not recorded".
  A config saved in list mode cannot be opened by a build from before list
  mode existed; switch back to manual or direct mode before rolling back to
  such a build.
- **Downstream Access Root** — see [Connection Center](dashboard.md#connection-center).
- **Auto-start on login** — installed Windows x64, macOS, and Linux x64
  desktop builds expose this switch. Development builds, the CLI, and Docker
  dashboards hide it. Linux AppImage startup entries point to the AppImage file;
  keep that file at the saved location, or toggle auto-start again after moving it.
- **Dock icon** — only the macOS desktop build exposes this switch. Turning
  it off keeps the menu-bar icon available. Windows, Linux, CLI, and Docker
  dashboards hide it.
- **Connect / non-stream / stream-idle timeouts** — default to 30, 900, and
  300 seconds. The non-stream value is a whole-request deadline; the stream
  idle value is enforced between response chunks. A saved tuple that exactly
  matches the complete former defaults (30/120/300) migrates to the new
  defaults on startup; customized tuples are preserved.
- **Check for updates / Update now** — updater-enabled installed desktop
  builds check the latest GitHub Release and can download, verify, and
  replace the existing copy in place. The data directory and auto-start
  setting stay. Development builds, the CLI, and Docker keep the
  release-link/manual-upgrade path. The host must be able to reach GitHub; a
  failed check or install does not affect gateway forwarding.
- **Zen Free** — enable or disable it from its account card. Use
  **Providers** to refresh the Free catalog, inspect protocol evidence, and
  toggle Chat Completions / Responses / Messages.

Settings are written to SQLite and reloaded on the next start. The Settings
resource never includes Key plaintext. Saves use the same `expectedRevision` /
`processGeneration` tokens as other Dashboard V4 writes. The update check is
on-demand.

---

[User guide index](../USER.md) · [简体中文](logs-settings.zh-CN.md) · [Docs index](../README.md)
