[简体中文](logs-settings.zh-CN.md)

# Logs And Settings

## Logs

The **Logs** view has three different records. They are not three views of one feed.

- **Logical requests** (the tab that opens by default) groups gateway forwarding
  and explicit protocol probes into one row per logical request. A non-blank
  request id uses the key `request:` plus that exact id. A null or blank id stays
  an independent historical row, `legacy:` plus its own row id, and is not merged
  with any other row. The status is the latest attempt. `streaming` and
  `outcome_unknown` stay unresolved; an earlier failure does not become the
  result while a later attempt is still unresolved. A filter matches any real
  attempt, then the expanded details list every attempt. Expand with the button
  on the row (keyboard accessible). Pre-upstream `attempt = 0` counts as zero
  upstream attempts and still counts as one logical request. Token totals are
  input + output. Cached tokens are already included in input and are not added
  again. The summary's logical-request count and upstream-attempt count are
  separate.
- **User operations** are receipts for actions taken in the dashboard, CLI, or
  desktop app: what was asked, the source, the subject, and the outcome
  (`success`, `rejected`, `failed`, `partial`, `compensated`, or `pending`).
  `pending` means no final receipt has been stored. It does not mean the action
  is still running after a restart. The actor is shown only when one was already
  known; an empty actor is left blank. Recording is best-effort: a crash after
  the change can leave this list missing that operation. These rows ignore log
  severity.
- **Historical mixed logs** are the old mixed gateway rows. New live writes to
  that feed have stopped. This tab is retained history, not a live program
  diagnostic. It is limited to the latest 200 rows after filters. Program
  diagnostics are not shown here.

Logical requests keep the request identity separate from the upstream identity:

- `requested_model` — the public name or Alias the client sent
- `resolved_alias` — the resolved public Alias when one exists
- `upstream_model` — the exact model ID actually sent to that account's upstream

plus `provider_id`. The model filter exact-matches
any of those identities or the `model` column.

- Chat streaming requests set `stream_options.include_usage` so
  OpenAI-compatible upstreams emit a usage chunk. A finished request without
  one still counts as success. The summary shows total tokens (input + output)
  when usage arrived.
  The gateway does not estimate a price for a new request. A cost that was not
  recorded stays unknown and is not shown as zero or free. Older rows keep a
  cost only when one was stored at the time, and they are not recalculated.

  The list shows time, attempt, model alias, and status. Expand a row to see
  the plan, account, request ID, and diagnostic detail. Filtering by success
  includes rows that have no recorded cost.
- An `outcome_unknown` row means the upstream may already have completed and
  charged the request, but the gateway lost the response or timed out. Such a
  request is not replayed automatically, and Open Console Gateway does not
  invent a local price for it.
- The **Key** filter narrows logical requests and the summary to one receiving
  Key. Options come from the log table itself, so disabled, deleted, and
  otherwise unknown keys stay filterable and keep their labels. **Unattributed**
  selects rows with no client-Key attribution; a background task attributes them
  to the primary key as an approximation.
- Route account and credential account stay separate columns and filters.
  `requested_model`, `resolved_alias`, and `upstream_model` stay visible as
  stored. The list does not invent a model, account, or person.
- Search by request ID to find its logical request and every recorded attempt.
- Old `/logs/forward` and `/logs/gateway` reads remain for compatibility.
  The logical-request page is not built by regrouping those rows in the browser,
  and attempt details are not paginated ahead of the server grouping.

### Program diagnostics

Program diagnostics go to stderr. Desktop and normal CLI startup also
write `<data-dir>/logs/program.log`. The active file and `program.log.1`
through `program.log.4` are each at most 10 MiB. `RUST_LOG` is the filter
(normal start `warn,ocg=info`, development scripts `warn,ocg=debug`).
`OCG_PROGRAM_LOG_FILE=off` disables the file. Docker stays stderr-only, as do
supervisors, browser workers, and secondary desktop instances. Operation and
logical-request rows do not consult this filter.

### What is kept

Operation rows and logical-request rows are not purged by a new automatic job.
History stays in the database and the database grows until you remove that data
yourself. A missing operation after a crash is not reconstructed. Separately,
`diagnostic_json` on existing forward and gateway rows is cleared when the database opens once it is older than 30 days;
the base row, request id, and compact error remain. That expiry is not a purge
of operation rows or logical requests.

Explicit debug attachments (opt-in capture of redacted request content, keyed
by request id and attempt) are a fourth mechanism, not one of the three Logs
tabs. Captures are limited to 2 MiB each, 100 MiB total, 7 days, and 1,000 files.
Startup and writes remove expired or excess OCG captures; a stopped program
does not run a cleanup timer. Oversized content is replaced with length, hash,
and an omission marker. The directory and files are private. Capture failure
does not fail the request.

## Settings

Routing mode and conversation sticky are configured on **Accounts**, above the
account list; see [Accounts](accounts.md).

The **Settings** view holds the gateway's persistent configuration:

- **Gateway Port** — the port the gateway binds (default `9042`). Desktop
  builds also accept the read-only `OCG_GATEWAY_PORT` runtime override; while
  it is set, the Settings field is disabled and the saved value is unchanged.

  Saving the port is finished when the gateway confirms the save. If the page
  cannot reload the saved settings, the port is still saved. Reload to read it
  again. Do not submit the change again only because that reload failed. If
  the gateway cannot open the new port, the save did not succeed. The page
  does not move to that port and does not treat the change as applied.

  When this browser is connected directly to the gateway and the listen port
  changes, the page offers a link to the new address. Open the link yourself.
  The page does not go there on its own, including when the new port is on this
  computer. The link keeps the same scheme, host name, path, query, and hash.
  If you opened the dashboard through a reverse proxy, the page stays on that
  proxy.
- **Outbound proxy** — shared by every account. Automatic, manual, and force
  direct apply one process-wide policy; **Per-model list** (below) splits chat
  forwarding by model instead.
  `Automatic (system / environment)` reads `HTTP_PROXY`, `HTTPS_PROXY`,
  `ALL_PROXY`, and `NO_PROXY`; Windows also reads the system proxy and
  connects directly when none is configured. `Manual HTTP proxy` strictly
  routes all HTTP/HTTPS targets through one `http://` or `https://` proxy such
  as `http://127.0.0.1:7890`; a proxy failure never silently falls back to a
  direct connection. `Force direct connection` ignores system and environment
  proxy configuration. Proxy URLs cannot contain credentials.

  For these three modes, the policy covers model forwarding (OpenCode Go, Zen
  Free, Command Code GOAT, MiniMax CN, Kimi Code CN, and Custom API),
  account-key tests and Custom verification, official OpenCode Go usage API,
  release checks, and signed desktop installer downloads; authenticated
  `GET /v1/models` and protected
  `GET /dashboard/api/v4/application-models` are local lists and do not use
  this outbound path. The browser sidecar is outside its scope. A managed CPA
  runtime follows the same policy: manual mode becomes its
  `requests.proxy-url`, force direct becomes `"direct"`, automatic leaves it
  on environment proxies, and per-model list applies the direction's default
  leg; changing the policy rewrites the CPA config and restarts a running
  managed CPA.

  **Test connection** uses the unsaved form values against the sealed
  OpenCode Go origin. Any HTTP status proves network reachability, without
  running model inference or incurring model usage. In list mode it probes
  only the direction's default leg, not a listed model's real forwarding path.
- **Per-model list** (fourth proxy mode) — routes chat forwarding per model
  instead of process-wide. Pick a direction and check exact upstream model IDs
  from enabled Provider contracts, eligible Custom capabilities, user-defined
  Provider mappings, and the active CPA catalog. Public Aliases are not added;
  the list accepts no patterns or free-text. An old saved ID that disappears
  from those sources remains inert and is removed on the next save.

  With the **whitelist** direction, listed models connect through the proxy
  URL while every unlisted model connects directly (ignoring
  system/environment proxies, exactly like force direct). The **blacklist**
  direction inverts this: listed models connect directly and everything else
  uses the proxy URL. Both directions require the proxy URL; an empty list or
  an empty URL cannot be saved.

  Non-chat outbound traffic (official usage sync, update checks, and signed
  downloads) always follows the direction's default leg: direct for a
  whitelist, the proxy URL for a blacklist — so switching from
  `Manual HTTP proxy` to a whitelist changes that traffic to direct. The
  account-key test and **Test connection** likewise probe the default leg, so
  they do not represent the real forwarding path of a listed model.

  Free-channel models can be listed, but Zen free quota is shared by egress
  IP, so routing them through a proxy changes which quota they draw from.
  Every forward-log row (successes included) records the leg it used —
  `proxy`, `direct`, or `auto` — in its expanded details; rows without a
  recorded leg show "not recorded". A config saved in list mode cannot be
  opened by a build from before list mode existed; switch back to manual or
  direct mode before rolling back to such a build.
- **Downstream Access Root** — see
  [Connection Center](dashboard.md#connection-center).
- **Auto-start on login** — installed Windows x64, macOS, and Linux x64
  desktop builds expose this switch. Development builds, the CLI, and Docker
  dashboards hide it. Linux AppImage startup entries point to the AppImage
  file; keep that file at the saved location, or toggle auto-start again after
  moving it.
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
- **Program log level** — Settings does not offer a live program-log control.
  Set `RUST_LOG` before startup to filter program diagnostics. Operation and
  logical-request records ignore it. The old `OCG_LOG_LEVEL` variable does not
  affect these live records.
- **Zen Free** — enable or disable it from its account card. Use
  **Providers** to refresh the Free catalog, inspect protocol evidence, and
  toggle Chat Completions / Responses / Messages.

Settings are written to SQLite and reloaded on the next start. The Settings
resource never includes Key plaintext. Saves use the same `expectedRevision` /
`processGeneration` tokens as other Dashboard V4 writes. The update check is
on-demand.

---

[User guide index](../USER.md) · [简体中文](logs-settings.zh-CN.md) · [Docs index](../README.md)
