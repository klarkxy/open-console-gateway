[简体中文](byok-applications.zh-CN.md)

# Local BYOK applications

> Scope: retained current-implementation or published OCG2 operation reference, not the OCG3 design. See [architecture](../architecture.md) for the CPA migration target; UI steps do not apply to this CLI phase.

Applications has four fixed native adapters: Codex, Kimi Code, MiniMax Code, and ZCode. This is independent custom-provider configuration, distinct from proxying a client’s native login. DSH keeps its plugin workflow.

The authenticated V4 endpoints are `GET|POST|DELETE /dashboard/api/v4/applications/byok/{client}` and `POST .../{client}/recover`. Writes require the current revision, process generation, and inspected file fingerprint. Configure accepts no Key, model selection, metadata overrides, or default-model choice. Under the settings lock it exports every exact public ID from the same publisher used by authenticated `/v1/models`, rejects an empty catalog, then creates or reuses the enabled ordinary Key named `codex`, `kimi-code`, `minimax-code`, or `zcode`. DSH defaults to `dsh`; its optional `keyId` remains compatible with existing API callers. New Key creation immediately advances the revision, including when a later native write fails. GET never creates Keys or builds a model picker catalog. Removal and recovery do not depend on a still-existing Key or model. Native CLI and Tauri register the shared host; builds without `dsh-local-host` return `unsupported_runtime`.

There is no count cap, tool-capability filter, or required metadata gate. Omit unknown native optional limits rather than inventing values. Native catalogs contain the complete publication at configure time; the existing update action refreshes them. DSH continues dynamic model discovery.

## Format baselines

The source comparison was performed on 2026-09-28. These commits describe the formats used by the adapters; they are not claims that every installed desktop version loads them.

| Client | Source baseline | OCG-owned configuration |
| --- | --- | --- |
| Codex | [0.153.4 schema](https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/core/config.schema.json) | `model_providers.ocg`, Responses transport, private Codex `ModelsResponse` catalog |
| Kimi Code | [configuration service](https://github.com/MoonshotAI/kimi-code/blob/4fbe065442179435c43d3c3dc8d11bb408b3fd30/packages/agent-core-v2/src/app/config/configService.ts) | TOML `providers.ocg` and `models."ocg/<public id>"`; request model remains the exact public ID |
| MiniMax Code | [local provider writer](https://github.com/MiniMax-AI/minimax-code/blob/2aed5ca703c3359dd028af51e6c4cbc6a5e15c46/packages/config/src/local-model-provider-write.ts) | YAML `custom_provider.ocg`, Chat Completions, compatible file lock |
| ZCode | [file codec](https://github.com/zai-org/ZCode/blob/29628c9acdb81b703bbd4080c207a0e7ce5e276e/packages/provider-node/src/provider-config-file-codec.ts) | `schemaVersion: 1`, Personal Provider and sparse provider-model rules, compatible owner-marker lock |

Codex's catalog is a global selection. Configure activates its OCG catalog. Inside the existing file lock and fingerprint boundary, the host keeps a still-published OCG default or selects the first exported model. Do not use nested `profiles` or a root `profile` selector merely because an older schema includes them: current [configuration guidance](https://learn.chatgpt.com/docs/config-file/config-advanced) describes separate profile files, and the current App Server rejects those legacy keys.

The 0.153.4 `ModelsResponse` deserializer requires `base_instructions` or `model_messages.instructions_template` on every entry. A structurally valid `ModelInfo` alone is insufficient. Use the unmodified generic fallback prompt pinned under `resources/codex-byok/`, sourced from [`codex-rs/models-manager/prompt.md`](https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/models-manager/prompt.md). Keep its Apache license and attribution in both native CLI and desktop bundles. Upgrades must validate the complete catalog loader, including this outer deserialization requirement.

MiniMax's default selection uses `custom_provider:ocg/<public id>`. Preserve slashes inside the public ID: its [model-key parser](https://github.com/MiniMax-AI/minimax-code/blob/2aed5ca703c3359dd028af51e6c4cbc6a5e15c46/packages/local-runtime-v2/src/service/model-system/resolution/model-key.ts) splits only at the first slash; the [provider prefix](https://github.com/MiniMax-AI/minimax-code/blob/2aed5ca703c3359dd028af51e6c4cbc6a5e15c46/packages/config/src/model-availability.ts) is `custom_provider:`.

## Ownership and recovery

Adapters own format-specific provider/model fields and record their last applied values. Preserve unrelated content, reject unowned namespace collisions, and never delete a model while retaining a default that refers to it. Restore a default only while it still matches OCG's applied selection.

The host keeps private origin backups, ownership receipts, and an operation journal under its data directory. Recovery validates current file states and backup hashes before restoring any file, including mixed states in Codex's two-file operation. Restore receipt state with file bytes, and retire ownership after removal. Secret-bearing files use restrictive permissions; diagnostic responses must not contain configuration excerpts or Keys. Symlinks/reparse points must not let target, catalog, receipt, or backup operations escape their inspected paths.

Codex and Kimi require the operator to close the client before writes; their in-process writers do not provide a shared external lock. MiniMax and ZCode use their upstream lock conventions. Do not reclaim unknown or live locks. JSON/YAML serialization preserves unrelated values; MiniMax YAML comments are retained only in the original backup. TOML edits preserve comments.

The host writes one operational line per event to process stderr through the shared `runtime_log` console sink: a finished mutation, a refusal with its reason kind, a stale fingerprint, an unowned `ocg` collision, an external edit of owned fields, a pending interrupted write, and a completed rollback. Messages name the client and the file only, and the sink never receives a Key, a request body, or a credential-bearing URL. A refused mutation reports the `ByokErrorKind`, not the response text, so an unsanitized message cannot reach the log. Dashboard-visible receipts stay the user-facing surface.

## Validation

Run `pnpm run contract:v4:check`, `pnpm run build:web`, the BYOK frontend domain/store/component tests, and the Applications behavior tests. Run Rust filters `dashboard_v4::byok_applications` and `byok_application_host` with the native feature, plus relevant DSH regression tests. Build the native CLI before `node scripts/byok-applications-smoke.mjs`; it uses isolated homes and synthetic credentials. Test the no-default-features CLI capability separately.

A parser test, saved configuration, client load, and successful inference are distinct evidence. Validate emitted files against the target source schemas when upgrading an adapter. Real desktop activation, tools, attachments, and multi-turn inference still require explicit client-runtime verification; a successful save must not be presented as proof of those behaviors.

[Maintainer guide](../MAINTAINER.md) · [User workflow](../user/applications.md)
