[简体中文](byok-applications.zh-CN.md)

# Local BYOK Applications

Applications has five fixed native adapters: Codex, Kimi Code, MiniMax Code,
ZCode, and VS Code Copilot. This is independent custom-provider configuration,
not the native-login proxy described in the older
[Codex proposal](codex-integration-proposal.md). DSH keeps its plugin workflow.

The authenticated V4 endpoints are
`GET|POST|DELETE /dashboard/api/v4/applications/byok/{client}` and
`POST .../{client}/recover`. All five native clients also support a read-only
fresh preview at `POST /dashboard/api/v4/applications/byok/{client}/preview`,
with optional `targetPath` and `copilotTokenBudget`. The preview returns current
revision, process generation, inspected file fingerprint, and an optional
`preview` plan on `ByokApplication` containing `planFingerprint`, added/removed/updated
model IDs, previous and new default model IDs, and
`requiresTakeover`, `requiresOverwrite`, or
`removedModelsWithCustomizations`. Preview does not create a Key, write files,
create a receipt, create native configuration directories, or mutate the
upstream catalog. Configure submits the preview fingerprint plus explicit
`acknowledgeTakeover`, `acknowledgeOverwrite`, and `acknowledgeRemoval` values
where required; commit revalidates all preflight values and the plan fingerprint.
Legacy V4 callers may omit the preview fingerprint for an ordinary conflict-free
write, while retaining revision/process-generation and byte-fingerprint checks.
Takeover, overwrite, and customized deletion always require a matching preview.
The application page always sends the reviewed plan fingerprint.

Writes require the current revision, process generation, and inspected file
fingerprint. Configure accepts no Key, model selection, metadata overrides, or
default-model choice. Copilot additionally accepts explicit input/output client
configuration budgets for the export clone; these do not mutate published
metadata.

Under the settings lock, configure exports every exact public ID from the same
publisher used by authenticated `/v1/models`, rejects an empty catalog, then creates or reuses the enabled ordinary Key named
`codex`, `kimi-code`, `minimax-code`, `zcode`, or `copilot`. DSH defaults to
`dsh`; its optional `keyId` remains compatible with existing API callers.
Preflight completes before Key creation. If native file I/O fails after Key
creation, the one enabled named Key remains for retry and the response reports
the partial outcome; no automatic Key deletion occurs.

GET never creates Keys or builds a model picker catalog. Removal and recovery do
not depend on a still-existing Key or model, and an empty catalog can still
remove existing application configuration. Native CLI and Tauri register the
shared host; builds without `dsh-local-host` return `unsupported_runtime`.

There is no count cap, tool-capability filter, or required metadata gate. Omit
unknown native optional limits rather than inventing values. Copilot's required
token fields use explicit client budgets constrained by known limits, as below.
Native catalogs contain the complete publication at configure and refresh time;
there is no model picker. DSH continues dynamic model discovery. Fresh-page
activation and every action response refresh status so the page does not retain
stale ownership or recovery state.

## Format Baselines

The first four source comparisons were performed on 2026-09-28; Copilot was
checked against VS Code 1.141 Stable on 2026-10-07. These commits describe the
formats used by the adapters; they are not claims that every installed desktop
version loads them.

| Client | Source baseline | OCG-owned configuration |
| --- | --- | --- |
| Codex | [0.153.4 schema](https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/core/config.schema.json) | `model_providers.ocg`, fixed Responses transport, private Codex `ModelsResponse` catalog |
| Kimi Code | [configuration service](https://github.com/MoonshotAI/kimi-code/blob/4fbe065442179435c43d3c3dc8d11bb408b3fd30/packages/agent-core-v2/src/app/config/configService.ts) | Grouped TOML providers `ocg-chat`, `ocg-responses`, and `ocg-messages`; public model ids stay `ocg/<public id>` |
| MiniMax Code | [local provider writer](https://github.com/MiniMax-AI/minimax-code/blob/2aed5ca703c3359dd028af51e6c4cbc6a5e15c46/packages/config/src/local-model-provider-write.ts) | Grouped YAML `custom_provider.ocg-chat`, `ocg-responses`, and `ocg-messages`, compatible file lock |
| ZCode | [file codec](https://github.com/zai-org/ZCode/blob/29628c9acdb81b703bbd4080c207a0e7ce5e276e/packages/provider-node/src/provider-config-file-codec.ts) | ZCode file `schemaVersion` 1, grouped provider rules `ocg-chat`, `ocg-responses`, and `ocg-messages`, sparse model rules, compatible owner-marker lock |
| VS Code Copilot | [1.141 Custom Endpoint provider](https://github.com/microsoft/vscode/blob/1.141.0/extensions/copilot/src/extension/byok/vscode-node/customEndpointProvider.ts) | JSONC `chatLanguageModels.json`, one `Open Console Gateway` provider with `vendor: "customendpoint"` and explicit per-model endpoints |

Codex's catalog is a global selection. Configure activates its OCG catalog.
Inside the existing file lock and fingerprint boundary, the host keeps a
still-published OCG default or selects the first exported model. Do not use
nested `profiles` or a root `profile` selector merely because an older schema
includes them: current
[configuration guidance](https://learn.chatgpt.com/docs/config-file/config-advanced)
describes separate profile files, and the current App Server rejects those
legacy keys.

The 0.153.4 `ModelsResponse` deserializer requires `base_instructions` or
`model_messages.instructions_template` on every entry. A structurally valid
`ModelInfo` alone is insufficient. Use the unmodified generic fallback prompt
pinned under `resources/codex-byok/`, sourced from
[`codex-rs/models-manager/prompt.md`](https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/models-manager/prompt.md).
Keep its Apache license and attribution in both native CLI and desktop bundles.
Upgrades must validate the complete catalog loader, including this outer
deserialization requirement.

Kimi Code, MiniMax Code, and ZCode group models by the published preferred
protocol into `ocg-chat`, `ocg-responses`, and `ocg-messages`. Each used
protocol has one provider entry and the same Gateway Key. Messages uses the
gateway root after a terminal `/v1` is removed; a deployment subpath stays.
Chat and Responses keep the `/v1` base. The written protocol is the published
preference when that client can speak it. Responses is not a fallback, and
Chat is not added when the profile does not list it. A legacy `ocg` provider
object is carried in the ownership receipt and removed when unused. A unique existing OCG namespace can be explicitly adopted after preview; ambiguous collisions remain refused. Public Kimi model ids remain
`ocg/<public id>`, while each model's provider field names the grouped entry.
Kimi provider types are `openai`, `openai_responses`, and `anthropic`. ZCode
api types are `openai-chat-completions`, `openai-responses`, and
`anthropic-messages`. The adapter writes the Responses group when the profile
selects Responses. Installed ZCode 3.14.3, in the public ASAR
`out/host/index.js`, enumerates `openai-chat-completions`,
`openai-responses`, and `anthropic-messages`, and its request resolver maps
`openai-responses` through `openai` to `/responses`. That covers the installed
request implementation. It does not cover a full Desktop UI run or a native
parse-config run.

Where a client menu exists for Chat Completions or Responses, it may carry
the published `reasoningEfforts` spellings unchanged. Those spellings stay
exact categorical wires. They are never a Messages thinking budget or an
adaptive effort, and a Responses vendor still applies its own contract to a
historical Chat spelling. Native Messages reasoning support can remain true. OCG omits the Messages
config menu and does not turn `reasoning: true` into a budget. Kimi, MiniMax,
and similar SDKs may apply their own preset or default control. The omitted
menu does not describe that control, and OCG does not promise the vendor
accepts it. On one format, native controls pass unchanged. A cross-format
control the conversion cannot preserve is rejected before HTTP, and the
upstream backend decides whether it accepts the native controls. ZCode writes
`reasoningLevel` on the Chat group only.
DSH selects its API per model; these three clients select one provider group
per protocol. Codex stays on `model_providers.ocg` and its Responses
transport. That client choice does not change the gateway's per-attempt
upstream selection.

These clients own conversation serialization. Their SDK can turn a native
opaque field into plain text, or drop it, when the provider, API, or model
identity changes. OCG cannot recover a field that never arrives, and it cannot
detect that drop. Switching a protocol group or rewriting the saved
configuration does not migrate an old conversation. A new conversation, or
history that has already been resolved, is required when that identity
changes. When marked history does arrive, the same configured route is the
bound of the guarantee: a direct change of upstream model, endpoint, or
credential version is rejected before HTTP. The DSH plugin is the client with
a strict preflight before its base adapter.

MiniMax's default selection uses `custom_provider:<group>/<public id>`, where
`<group>` is the routed provider (`ocg-chat`, `ocg-responses`, or
`ocg-messages`). The longest managed id is matched first, so `ocg-chat` is
not read as legacy `ocg`. Preserve slashes inside the public ID: its
[model-key parser](https://github.com/MiniMax-AI/minimax-code/blob/2aed5ca703c3359dd028af51e6c4cbc6a5e15c46/packages/local-runtime-v2/src/service/model-system/resolution/model-key.ts)
splits only at the first slash; the
[provider prefix](https://github.com/MiniMax-AI/minimax-code/blob/2aed5ca703c3359dd028af51e6c4cbc6a5e15c46/packages/config/src/model-availability.ts)
is `custom_provider:`.

## VS Code Copilot Contract

Use client ID `copilot` on the existing V4 `byok/{client}` routes and ordinary
Key name `copilot`. Configure, update, remove, and recover use the shared host,
CAS, inspected-file fingerprint, ownership receipt, private backup, and journal
boundaries. Do not add extension installation or writes to `settings.json`.

The target is a JSONC top-level array. Manage exactly one provider object named
`Open Console Gateway`, with `vendor: "customendpoint"` and `models: [...]`.
Preserve unrelated providers and comments. Offer reviewed takeover for a unique existing OCG provider and reviewed replacement for changed managed values; reject duplicate managed names, malformed entries, and noncanonical Authorization header casing inside the OCG provider or its models. Do not select a default
model. Removal and recovery retain the same collision and intervening-edit
checks, without depending on the original Key or catalog still existing.

Export the entire routable public-model snapshot. Set each model's `apiType`
and full `/v1/chat/completions`, `/v1/responses`, or `/v1/messages` URL from its
saved published preferred protocol, preserving deployment subpaths. Omit the
provider-level `url`: 1.141's discovered-model path can skip IDs unknown to its
catalog, while the explicit `models` path accepts configured rows. Omit
`apiKey` and write literal `requestHeaders.Authorization: "Bearer <OCG Key>"`
on every model. This is a single external-file operation; it does not seed VS
Code secret storage. The Key is plaintext in private client configuration and
private backups, never in dashboard responses or diagnostics. The native UI's
`${input:...}` secret references and `${apiKey}` header substitution are an
optional manual alternative; do not claim OCG writes them.

VS Code requires usable input/output token fields. The confirmation draft
starts at 100,000 input and 8,192 output tokens, is editable, persisted, and
shown again on later previews. Treat these as exported client configuration
budgets, not discovered vendor limits. An ordinary refresh preserves compatible
per-model custom budgets; apply a new global budget only when the user edits it.
Known
input/output limits constrain the corresponding exported values; if their sum
exceeds a known context window, scale both values proportionally. Unknown limits
use the explicit user budget. Apply the values only to export clones, without
saving metadata or claiming new upstream capabilities. They guide VS Code's
context management and output reservation; actual request parameters and
upstream limits depend on the client and model. In particular, configured
`maxOutputTokens` is not a universal hard limit on HTTP output.

Export required `toolCalling` and `vision` booleans conservatively as false
when unknown. All models remain in the Chat export; the Agent picker requires
known tool calling. The existing model metadata page supplies verified
declarations. For Chat Completions and Responses, deduplicate verified
reasoning wire values without changing their spelling, write
`supportsReasoningEffort`, and set the matching `reasoningEffortFormat`.
Do not manufacture a Messages reasoning menu or thinking budget.

Resolve portable data (`VSCODE_PORTABLE`) first, then `VSCODE_APPDATA` and
the product directory name, then the platform application-data root:
Windows `%APPDATA%`, macOS `~/Library/Application Support`, or Linux
`$XDG_CONFIG_HOME` with `~/.config` fallback. The default is
`Code/User/chatLanguageModels.json`; use an existing `Code - Insiders/User`
when Stable's directory is absent. Named profiles, portable installations, and
`--user-data-dir` can use the existing explicit-path override. VS Code has no
upstream shared file lock: fully close it before configure, update, remove,
or recover, then reopen and select the provider in the model picker.

The advanced adapter format is pinned to 1.141.0 (Stable commit
`2a59476c9bfcb90b3ddc372c36762471b7dfad1c`). Custom Endpoint first shipped in
[Stable 1.122](https://code.visualstudio.com/updates/v1_122#_custom-endpoint-provider-in-stable); this does not establish that earlier releases implement every
field used here. Inspection of an installed 1.142 Insiders build is separate
from this pinned format baseline and from live client acceptance. The
[official language model guide](https://code.visualstudio.com/docs/agent-customization/language-models)
covers Chat, Agent, inline chat, and utility tasks. This integration does not
provide inline completions or Next Edit Suggestions. Agent Host BYOK is
experimental and requires `chat.agentHost.byokModels.enabled`; document the
setting without enabling it automatically.

## Ownership And Recovery

New ownership receipts use version 2 so an older host cannot remove an adopted provider as if OCG created it. Version 1 receipts remain readable and upgrade only on a successful write; inspection does not rewrite them. This is a private receipt format change, with no database migration. Legacy receipts do not contain generation provenance: first updates retain compatible client budgets and require explicit removal review for customized rows. A legacy Codex catalog may supply a generation baseline only when its bytes or canonical pretty serialization match the recorded hash; otherwise changed content requires review. Legacy Copilot per-model budgets are preserved when the saved global budget is unavailable; OCG does not infer that original global value.

Adapters own explicit format-specific routing, identity, and metadata fields and
record their last applied values. Preserve unrelated content, unknown extras,
and client preferences. A matching byte fingerprint still protects concurrency;
Codex catalog whitespace is semantically equal. A unique OCG namespace with a
lost receipt may be reviewed for takeover; confirmation saves the current configuration as its private baseline.
Duplicate or malformed entries and foreign references remain blocked. A true
owned edit requires reviewed explicit choice to **Apply OCG changes** or
**Cancel**; there is no automatic 409 retry. A removed customized model row
requires acknowledgement for whole-row deletion. A manual ZCode rule stays in
place, and fields OCG does not own on a managed model rule are retained.
Restore a default only while it still matches OCG's applied selection.

When inspection marks a block as adopted, the removal UI uses **Undo takeover**.
Undo restores only managed fields changed by OCG while they still match the last
applied values. It preserves subsequent extras and preferences inside the
adopted block and unrelated content. Competing owned edits produce a conflict;
undo cannot restore unknown state from before the first OCG change.

The host keeps private origin backups, ownership receipts, and an operation
journal under its data directory. The journal records partial writes and
rollback state. Recovery validates current file states and
backup hashes before restoring any file, including mixed states in Codex's
two-file operation. Restore receipt state with file bytes, and retire ownership
after removal. Secret-bearing files use restrictive permissions; diagnostic
responses must not contain configuration excerpts or Keys. Symlinks/reparse
points must not let target, catalog, receipt, or backup operations escape their
inspected paths.

Codex, Kimi, and VS Code require the operator to close the client before writes; their
in-process writers do not provide a shared external lock. MiniMax and ZCode use
their upstream lock conventions. Do not reclaim unknown or live locks.
JSON/YAML serialization preserves unrelated values; MiniMax YAML comments are
retained only in the original backup. TOML and Copilot JSONC edits preserve comments outside regenerated managed rows; comments inside a rebuilt provider or model row may be reformatted or removed. Private backups retain the original bytes.

The host writes one operational line per event to process stderr through the
shared `runtime_log` console sink: a finished mutation, a refusal with its
reason kind, a stale fingerprint, an unowned `ocg` collision, an external edit
of owned fields, a pending interrupted write, and a completed rollback. Messages
name the client and the file only, and the sink never receives a Key, a request
body, or a credential-bearing URL. A refused mutation reports the
`ByokErrorKind`, not the response text, so an unsanitized message cannot reach
the log. Dashboard-visible receipts stay the user-facing surface.

## Validation

Run `pnpm run contract:v4:check`, `pnpm run build:web`, the BYOK frontend
domain/store/component tests, and the Applications behavior tests. Run Rust
filters `dashboard_v4::byok_applications` and `byok_application_host` with the
native feature, plus relevant DSH regression tests. Build the native CLI before
`node scripts/byok-applications-smoke.mjs`; it uses isolated homes and
synthetic credentials. Test the no-default-features CLI capability separately.

A parser test, saved configuration, client load, and successful inference are
distinct evidence. Validate emitted files against the target source schemas
when upgrading an adapter. Real desktop activation, tools, attachments, and
multi-turn inference still require explicit client-runtime verification; a
successful save must not be presented as proof of those behaviors.

---

[Maintainer guide index](../MAINTAINER.md) · [简体中文](byok-applications.zh-CN.md) · [Docs index](../README.md)
