[简体中文](cpa-migration.zh-CN.md)

# Migration boundaries for CPA execution

This page applies the [local-CPA-only architecture](../architecture.md). One OCG-owned local CPA, with no dedicated external CPA product mode, is the design. The command is `ocg`, the package is `ocg-cli`, and the default data root is `~/.ocg3`. Whole CLI acceptance is pending. This page does not record a runtime success.

## Historical baseline, retained source, and the CPA ingress

The previous generation executed inference in its own gateway kernel: selection, send, translation, fallback, and ordinary backoff lived in that kernel. That kernel is the historical baseline this generation replaces. It is not the current public forward path.

The tree still contains older execution code and a dedicated remote CPA integration: `OCG_CPA_BASE_URL`, an owned-versus-integration target selector, a saved base URL with a remote management credential, the fixed Docker origin `http://cpa:8317`, the reserved singleton account, and routes under `/external-integrations/cpa`. That retained source is not the public inference path and not a fallback. Historical remote settings stay stored. They are not auto-activated and they are not deleted. Moving them onto the local runtime is an explicit migration. Removing the retained code waits until its consumers have moved.

Public generation and count handlers in `crates/ocg-core/src/gateway/handler.rs` forward through `cpa_ingress::forward_public`. That ingress is new source. It is not accepted. Whole CLI acceptance, including compile and runtime of this generation, is pending.

The product target is one local CPA process owned by this OCG installation. Private inference and management stay on that child. There is no external CPA mode, no target selector, no remote management credential, and no separate remote readiness or catalog contract. The historical kernel is not a fallback when the child is unavailable. A supported custom HTTP provider remains a provider capability and is executed through the local CPA. Removing the remote CPA product mode does not retire custom HTTP, sealed providers, OAuth and import, client Keys, or application hosts. The full headless CLI is the current deliverable. The accepted GPUI/Ely GUI stays postponed. Operator commands are in the [CLI guide](../user/cli.md). Development notes are in [development](development.md). Current routes are in the [HTTP contract](dashboard-api.md).

The existing crate dependency graph is not a permanent boundary for the target. Retain necessary product, control, storage, and security code while the owned local CPA owns sending and execution. This branch has no Vue/Tauri workspace and no native GPUI GUI. The adopted desktop target is still a GPUI/Ely client of shared control operations. The headless CLI does not depend on that GUI, and the GUI does not start before headless acceptance.

## Generation branches

Open Console Gateway remains the product identity. `ocg3` and `open-console-gateway` are two generation branches of that one project tree, as Python 3 and Python 2 are two generations of one language. This page belongs to the OCG3 generation. The owned-local-CPA design is not an ordinary compatible patch of the previous generation, and it is not an unrelated new project. The [architecture lineage section](../architecture.md#product-lineage-and-generation-branches) is the design source for this split.

The user command is `ocg` (`ocg.exe` on Windows, `ocg` on Linux and macOS). The Rust package is `ocg-cli` in `crates/ocg-cli`. That package name is not a second user command. There is no alias executable named `ocg-manager-cli`. Historical published names, package identifiers, and backup format tags remain historical references. The [CLI guide](../user/cli.md) describes this command and the `~/.ocg3` default.

The default CLI data root is `~/.ocg3`. A launch without an explicit directory uses that root. It does not open, copy, move, delete, or adopt the previous generation's default profile or data, including `~/.ocg-mgr-cli`. Explicit `--data-dir` remains supported and is validated normally. Bringing previous-generation files forward is a separate explicit migration. Backups remain the operator's copies. Serialization tags and encryption compatibility keep their existing names.

## Retain, replace, and retire

| Area | Migration treatment |
| --- | --- |
| User command and default data root | Command is `ocg` from package `ocg-cli`. Default root is `~/.ocg3`. Do not automatically reuse `~/.ocg-mgr-cli` or any other previous-generation default. Explicit `--data-dir` stays. Old files move only by explicit migration. Backups stay |
| CLI, Key management, access control, CAS | Retain shared control services; clients cannot bypass OCG to reach the owned local CPA |
| Plan/provider identities, credential bindings, model scopes, aliases, priorities | Retain product semantics and project route configuration for the owned local CPA; entry does not select execution Keys |
| Precise quotas and official usage parsing | Retain trusted evidence adapters; normalize scopes and deadlines and enforce them synchronously inside the owned local CPA |
| Provider HTTP, OAuth refresh, translation, SSE execution | Transfer execution to the owned local CPA; absent capabilities remain explicit product gaps |
| Supported custom HTTP providers and other real provider or application capabilities | Keep them. The local CPA executes custom HTTP. Ending the remote CPA product mode does not retire these capabilities or their CLI consumers |
| Former selector, forwarder, fallback, ordinary backoff, generic rule recovery | The owned local CPA owns the new path. The former kernel is not a fallback. Remove obsolete implementation and tests only after its consumers migrate. That removal has not happened |
| Local estimates, pricing, and credit settlement | Remove from execution admission and selection; explicitly export or retain existing data rather than dropping tables or treating estimates as authoritative quota |
| Owned local CPA installation, auth files, and process lifecycle | Required execution runtime. OCG manages only the child it owns. Private inference and management stay on that child |
| Dedicated remote CPA product mode | Absent from the target. Historical base URL, management cipher, inference material, and catalog rows stay stored. They are not auto-activated and they are not deleted. Explicit migration is required and is not implemented |
| Client/application integration and account tools | Preserve concrete CLI consumers or document differences; deleting design docs does not retire every capability |
| Native GPUI/Ely desktop GUI | Adopted future design in the architecture; A home, C Plans, B optional compact mode. Postponed until headless CLI acceptance. Not implemented |
| WebUI, WebView main interface, tray, former frontend redesign | Outside the adopted GUI scope; published-version material is historical |

## Implementation sequence

1. Project product configuration onto the one owned local CPA, including public models, priorities, permissions, proxies, and secret ownership. Do not require a remote CPA row or a global remote management client before the owned child can run. Map each former capability to a local-CPA equivalent or an explicit gap. Supported custom HTTP stays a provider capability on that path.
2. Integrate startup, configuration application, readiness, shutdown, and failure state for the owned child through existing CLI and control operations. Readiness is the owned child. A saved remote setting does not satisfy it.
3. Implement synchronous integration inside that child: concrete credential identity, quota evidence, and continue/stop decisions before another send. Select SDK hosting or a small patch during implementation. Do not promote the experimental probe to a product framework.
4. Public handlers forward client execution through `cpa_ingress`. Keep OCG authentication, public-model publication, total deadlines and cancellation, correlation, and redaction. Selection and retry have one owner. Unavailable means unavailable. The former kernel does not take the request. Acceptance of that path is part of the pending whole-CLI acceptance.
5. Report historical remote settings as requiring explicit migration. Leave them stored, closed, and redacted until that migration exists. After the local path replaces supported consumers, disconnect the former kernel and remove its obsolete code, tests, and dedicated data paths. That sequence is not finished.

Isolated comparisons may use separate directories during migration. A production request executes once, on the owned local CPA. Do not dual-send a real generation, and do not use the former kernel as a transparent failure fallback.

## Native GUI work

The [architecture](../architecture.md#9-native-desktop-gui) is the sole GUI design source. Layouts A, C, and B, GPUI, and Ely stay adopted. GUI work is postponed until the headless CLI acceptance is actually met. The future client attaches to the local OCG control service. It does not add an external CPA screen and does not take ownership of the database or of the local CPA process. Reuse implemented control operations while migration continues, and expose remaining target gaps instead of presenting them as available. GUI startup, authorization, reconnect, and full shared backup/restore still need lifecycle implementation. Native platform samples are later checkpoints. The GUI stays postponed.

## Compatibility, security, and rollback

Access Keys, public model names, and V4/CAS control contracts remain compatibility baselines. The user command is `ocg` and the default data root is `~/.ocg3`, as the [CLI guide](../user/cli.md) describes. A contract change updates the schema and the paired user guides together. Stored rows and previous-generation profiles stay until an explicit migration moves them.

Persistent identities, credentials and bindings, catalogs, permissions, secrets, and encryption identities must remain recoverable. A historical remote CPA configuration is part of that recoverable data. Recognize it and report that explicit migration is required. Do not auto-activate it: do not open the saved base URL, do not start management against it because the row or `OCG_CPA_BASE_URL` is present, and do not treat an omitted target as permission to use the remote path. Do not delete it on upgrade. Do not copy its secrets into an owned-native credential or into the local child's management secret. Do not print those secrets in status, logs, or a migration report. The explicit migration itself is a later operator action. It is not implemented here. Migration code and schemas determine database and import support, not obsolete documentation version numbers. See [storage migration](storage-migration.md) and [upgrade/backup](../user/upgrade-backup.md).

Keep compatible configuration and auth-directory backups before switching. A previous-generation data directory is left in place. Rollback restores matching runtime files, configuration, database, and encryption material; older binaries cannot read newer schemas. Do not hand CPA OAuth tokens to the former OCG refresh implementation. Remove the former kernel only after the new path replaces supported consumers. That removal has not happened. Binary acceptance of `ocg` remains pending with the rest of the CLI.

Preserve authentication/redaction, URL/proxy boundaries, directory permissions, concurrent configuration integrity, stale-result protection during rotation, persisted quota deadlines, streaming cancellation, and uncertain-result no-replay. These guarantees do not require every former state machine.

## Documentation versus implementation completion

The architecture states one owned local CPA and the Open Console Gateway lineage. This page separates the historical kernel, retained older source, and the current CPA ingress. That ingress is new source and is not accepted. Source defines the `ocg` command, the `ocg-cli` package, and the `~/.ocg3` default. Binary acceptance is pending.

Whole CLI acceptance requires a complete headless `ocg` CLI on the owned local CPA, precise quotas and selective no-replay working together, historical remote settings either still inert or explicitly migrated, previous-generation data left untouched unless an explicit migration moves it, usable backup and rollback, and removal of unconsumed former execution code. This checkpoint is not that acceptance and it is not a publication. It does not record a runtime success.
