[简体中文](byok-applications.zh-CN.md)

# Local BYOK Applications

Applications has four fixed native adapters: Codex, Kimi Code, MiniMax Code,
and ZCode. This is independent custom-provider configuration, not the
native-login proxy described in the older
[Codex proposal](codex-integration-proposal.md). DSH keeps its plugin workflow.

The authenticated V4 endpoints are
`GET|POST|DELETE /dashboard/api/v4/applications/byok/{client}` and
`POST .../{client}/recover`. Writes require the current revision, process
generation, and inspected file fingerprint. Configure accepts no Key, model
selection, metadata overrides, or default-model choice.

Under the settings lock, configure exports every exact public ID from the same
publisher used by authenticated `/v1/models`, rejects an empty catalog, then
creates or reuses the enabled ordinary Key named `codex`, `kimi-code`,
`minimax-code`, or `zcode`. DSH defaults to `dsh`; its optional `keyId` remains
compatible with existing API callers. New Key creation immediately advances the
revision, including when a later native write fails.

GET never creates Keys or builds a model picker catalog. Removal and recovery do
not depend on a still-existing Key or model. Native CLI and Tauri register the
shared host; builds without `dsh-local-host` return `unsupported_runtime`.

There is no count cap, tool-capability filter, or required metadata gate. Omit
unknown native optional limits rather than inventing values. Native catalogs
contain the complete publication at configure time; the existing update action
refreshes them. DSH continues dynamic model discovery.

## Format Baselines

The source comparison was performed on 2026-09-28. These commits describe the
formats used by the adapters; they are not claims that every installed desktop
version loads them.

| Client | Source baseline | OCG-owned configuration |
| --- | --- | --- |
| Codex | [0.153.4 schema](https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/core/config.schema.json) | `model_providers.ocg`, fixed Responses transport, private Codex `ModelsResponse` catalog |
| Kimi Code | [configuration service](https://github.com/MoonshotAI/kimi-code/blob/4fbe065442179435c43d3c3dc8d11bb408b3fd30/packages/agent-core-v2/src/app/config/configService.ts) | Grouped TOML providers `ocg-chat`, `ocg-responses`, and `ocg-messages`; public model ids stay `ocg/<public id>` |
| MiniMax Code | [local provider writer](https://github.com/MiniMax-AI/minimax-code/blob/2aed5ca703c3359dd028af51e6c4cbc6a5e15c46/packages/config/src/local-model-provider-write.ts) | Grouped YAML `custom_provider.ocg-chat`, `ocg-responses`, and `ocg-messages`, compatible file lock |
| ZCode | [file codec](https://github.com/zai-org/ZCode/blob/29628c9acdb81b703bbd4080c207a0e7ce5e276e/packages/provider-node/src/provider-config-file-codec.ts) | ZCode file `schemaVersion` 1, grouped provider rules `ocg-chat`, `ocg-responses`, and `ocg-messages`, sparse model rules, compatible owner-marker lock |

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
object is carried in the ownership receipt and removed when unused. An
unowned `ocg` name collision is still refused. Public Kimi model ids remain
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

## Ownership And Recovery

Adapters own format-specific provider/model fields and record their last applied
values. Preserve unrelated content, reject unowned namespace collisions, and
never delete a model while retaining a default that refers to it. A manual
ZCode rule stays in place. OCG reports a conflict when that rule uses the same
managed provider and model as a rule being written, or when it still names a
managed provider this write would remove. Fields OCG does not own on a managed
model rule are retained. Existing CAS checks and ownership-collision protection
remain. Restore a default only while it still matches OCG's applied selection.

The host keeps private origin backups, ownership receipts, and an operation
journal under its data directory. Recovery validates current file states and
backup hashes before restoring any file, including mixed states in Codex's
two-file operation. Restore receipt state with file bytes, and retire ownership
after removal. Secret-bearing files use restrictive permissions; diagnostic
responses must not contain configuration excerpts or Keys. Symlinks/reparse
points must not let target, catalog, receipt, or backup operations escape their
inspected paths.

Codex and Kimi require the operator to close the client before writes; their
in-process writers do not provide a shared external lock. MiniMax and ZCode use
their upstream lock conventions. Do not reclaim unknown or live locks.
JSON/YAML serialization preserves unrelated values; MiniMax YAML comments are
retained only in the original backup. TOML edits preserve comments.

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
