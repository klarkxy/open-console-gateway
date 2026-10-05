[简体中文](applications.zh-CN.md)

# Applications

## Codex, Kimi Code, MiniMax Code, and ZCode

These tabs add an **Open Console Gateway** model provider to an existing local client configuration. Codex means local Codex workflows in the CLI or desktop app; it does not redirect ordinary ChatGPT Chat or cloud Work.

1. Run the OCG desktop app or native CLI on the same computer and under the same user as the client. Select the client tab and inspect the exact configuration path. The path belongs to the OCG host, which may differ from the computer displaying the dashboard.
2. For Codex and Kimi, close the client CLI and desktop app before you confirm. These clients do not cooperate with an OCG file lock; do not edit the same file during the operation.
3. Review the target paths and confirm **Configure**. OCG creates or reuses an enabled ordinary **Key** named `codex`, `kimi-code`, `minimax-code`, or `zcode`, and exports every model returned by authenticated `/v1/models`. That response is already the executable published catalog: each row has schemaVersion 2 and a validated derived protocol profile. There is no separate model selection or capability filter. The same strict parser still rejects externally malformed catalog input; this export does not loosen parsing. The Key is written to the client's local configuration with restricted file permissions; the result does not show its value.
4. Start the client or open a new session, select an OCG model, and send a request. Confirm it in OCG **Logs**. A saved configuration does not prove that the client loaded it or that tools, attachments, and multi-turn conversations work.

The gateway route marker protects opaque native history only when that client forwards it. Codex, Kimi Code, MiniMax Code, and ZCode own their conversation serialization. Their SDK can turn a native opaque field into plain text, or drop it, when the provider, API, or model identity changes. OCG cannot recover a field that never arrives, and it cannot detect that drop. Switching a protocol group or rewriting the client configuration does not migrate an old conversation. Start a new conversation, or send history that has already been resolved, when that identity changes. When marked history does arrive, the same configured route is the bound of the guarantee: a direct change of upstream model, endpoint, or credential version is rejected before HTTP. The DSH plugin is the client that checks a present signed envelope before its base adapter can rewrite it.

**Update configuration** exports the entire current model list again. These native catalogs are snapshots taken when you configure; update after changing published models. **Remove** removes or restores only the OCG-managed portions. It does not delete the client's login, other providers, or the OCG Key. Removal does not require the original Key or model to remain available. When a user has changed a managed value, OCG reports a conflict instead of overwriting it. A manual ZCode rule stays in place. OCG reports a conflict when that rule uses the same managed provider and model as a rule being written, or when it still names a managed provider this write would remove. Fields OCG does not own on a managed model rule are retained. Existing CAS checks and ownership-collision protection remain.

Configuration keeps the current OCG model when it is still published; otherwise it activates the first exported model. Choose another model in the client itself. Codex activates the OCG catalog as its global catalog; it does not merge it into the native ChatGPT catalog. Unknown model limits are left unspecified. Model availability does not guarantee that every model supports a coding client's tools.

Declared reasoning choices are exported to each client's native model selector: Codex receives `supported_reasoning_levels`, Kimi receives `support_efforts` and the explicit disable spelling in `off_effort`, MiniMax receives `thinking.effortOptions`, and ZCode receives `reasoningLevel` options. The exported values retain their declared request spellings. Those spellings are the published Chat selector wires. Where a client menu exists, Chat Completions and Responses may carry the same categorical efforts unchanged. They are never a Messages thinking budget or an adaptive effort. A Responses vendor still applies its own contract to a historical Chat spelling. ZCode writes `reasoningLevel` on the Chat group only. When a reasoning-capable model has no known choices, OCG does not write a guessed Low, Medium, or High choice, and `reasoning: true` does not manufacture a menu or a budget. Omitting that config menu does not describe the native client's own controls. Kimi, MiniMax, and similar SDKs may apply a preset or default of their own. OCG does not promise that a vendor accepts those defaults. OCG still exports the model. On one format, native controls pass unchanged. A control that cannot be preserved on another format is rejected before HTTP, and the upstream backend decides whether it accepts the native controls. Update the application configuration and reload the client after the published choices change. See [model metadata and reasoning tiers](model-metadata.md) for the source and declaration rules.

Kimi lowercases ordinary effort selections and treats `on` and `off` as native controls. OCG exports ordinary choices only when Kimi can send their declared spelling unchanged; the explicit disable mapping remains separate in `off_effort`.

MiniMax normalizes the disable spelling `off` to `none`. OCG therefore omits a literal `off` choice when that would change the declared upstream parameter; an explicitly declared `none` remains available. The export does not force a binary thinking mode or invent a default effort.

Disabled Keys stay disabled. Configuring again creates an enabled Key when none with the matching name exists. A Key created before a later client-file error remains available in Connection Center, and retrying reuses it. An empty model catalog is rejected before creating a Key or writing client files.

The generated Codex catalog includes the official generic coding instructions from Codex 0.153.4, which requires an instruction source for each custom model. Source attribution and the Apache 2.0 license are included with OCG.

OCG keeps private recovery data before replacing configuration. If an operation is interrupted, the tab offers recovery when the saved journal can safely restore the affected files. It refuses to restore over intervening user changes. Recovery files may contain credentials; keep them private. Do not copy their contents into bug reports.

MiniMax YAML configuration is reformatted when saved. Unrelated values remain, and the original comments stay available in the first backup.

The tabs resolve client-specific home/data-directory overrides. You can select another configuration path for a custom profile. Codex and Kimi use `config.toml`; MiniMax uses `config.yaml`; current ZCode uses the versioned `provider_config.json` Personal Provider format. Legacy or malformed files are not overwritten. CLI and desktop share a configuration only when they use the same location and compatible format. Docker and builds without the native capability cannot configure clients on the browser computer; use [manual client configuration](add-application.md).

The **DSH** tab below connects DSH itself to the gateway through the OCG plugin.

## DSH

The **DSH** tab installs or removes the OCG-owned plugin through the
**running address** shown on the page.

In DSH's plugin list, the integration appears as **Open Console Gateway** with
the OCG icon and an English or Chinese description matching DSH's language.
Reinstall through OCG to update an older plugin package, then restart DSH if
prompted.

The page also detects profiles one level below the current user's
`~/.dsh/profiles` and `~/.dsh-*/profiles`. It lists directories with a valid
DSH profile manifest and skips linked directories. When `DSH_HOME` is
explicitly set for the OCG Host, detection follows that existing Home instead.
The default `web` target remains available even before its profile manifest is
created. Selecting a profile chooses its local DSH Home and session context.
Check the running address before confirming installation or uninstallation.

A discovered profile points at that Home's DSH session files and suggests a
loopback address (`web` → `http://127.0.0.1:3080`, official Desktop →
`http://127.0.0.1:19387`). The address is editable for a custom port. The
selected profile is the local Home and session context used to mint the
in-memory cookie. The **actual mutation target is the running origin shown in
the confirmation dialog**, not the profile directory. A successful
plugin-manager call does not prove which folder is on disk; DSH's
`$events.home` is the OS home. OCG does not scan ports. The running-address
path writes only OCG-owned package and handoff files; it does not edit that
profile's `package.json`.

1. Run either the installed Open Console Gateway Desktop app or the native
   `ocg-manager-cli serve` build on the same machine and as the same OS user as
   DSH.
2. Open **Applications > DSH**, select the intended Home and profile, check the
   running address.
3. Select **Install**. Review the displayed running address and local targets,
   then confirm. Confirmation uses the local DSH session already on this machine
   to operate on that address; it is not a new permission wizard.
4. Start or restart DSH when the page asks you to do so.

New installations can load immediately. Replacing a loaded package may require
a restart. Failed or unconfirmed operations are not reported as successful;
refresh the status before deciding whether to try again.
Reinstall and uninstall target `@open-console-gateway/dsh-plugin` by name at
the displayed address, including a same-name package from another source.
The confirmation dialog states this scope; the local profile is not proof of
the running package's source.

Ordinary DSH Web and official Desktop use the same running HTTP plugin-manager
interfaces. OCG does not fall back to the DSH desktop CLI for those targets, or
for any connected runtime URL. Offline or unsupported local-session format
failures stay visible; they do not invoke a CLI. DSH Editor-owned profiles keep
their existing offline CLI flow unless you supply a runtime URL, in which case
the same HTTP path is used.

This Web/Desktop path is current native same-machine compatibility with DSH's
existing browser-session grant and HTTP protocol. It is not a supported public
external-auth API and adds no pairing file, identity route, or companion plugin
step. An unsupported grant format fails clearly.

Installation is not restricted by a global DSH CLI version. The page shows a
runtime version only when that running address reports one. OCG adds only the
owned `@open-console-gateway/dsh-plugin` package through the live manager,
preserving other bundles. It does not replace the whole configuration or
install a separate copy of DSH runtime dependencies. Failures and conflicts are
reported with their actual cause.

OCG keeps the package cache for the plugin it is installing. When that cache
is missing files and every file that is already there still matches, OCG
rebuilds the cache and continues. It leaves a cache unchanged when a file was
edited, an unexpected file or directory is present, or the cache directory is
reached through a link. The page shows the reason OCG returned. It says the
DSH state changed only when the settings revision actually conflicted.

Installation creates or reuses an enabled ordinary Key named `dsh`.
The Desktop host resolves its value, writes it to a private one-time handoff
file for the selected target, and asks the displayed running address to install
the materialized package. When DSH loads the plugin, the plugin imports that
value into DSH's own credential service and removes the handoff file. The Key
is never placed in the generated package source or command arguments. DSH
Editor-owned profiles also register the package in Editor's user-plugin state
so it survives Editor profile rebuilds; close Editor before installing, then
restart it.

The plugin registers **Open Console Gateway** as `ocg`. It loads authenticated
`GET /v1/models` on first use, when the model list is read, or when a requested
model is absent from the loaded directory. Calls to known models reuse that
snapshot without timed refresh; see [model metadata](model-metadata.md).
The advertised picker is that executable catalog, including eligible
Custom IDs; it is not the narrower dashboard `application-models` list.
Internal `ocg-rejected` placeholders stay off the selectable list so exact
resolve and prepare can still fail closed. Mixed invalid rows are excluded; an
all-invalid catalog is empty and does not fall back to Chat. The plugin parser
stays strict on malformed rows. Model visibility changes therefore do not require
reinstalling the plugin. Whether a model accepts image attachments in DSH is
decided entirely by [model metadata](model-metadata.md): only models whose
catalog discovery or manual declaration lists the `image` input modality are
marked as image-capable. There are no model-ID exceptions; every other model
stays marked text-only until its image support is verified.

An **Installed** status proves the package registration and credential handoff
were prepared. It does not prove that DSH has restarted, loaded the plugin, or
completed a real model call. After restart, choose an OCG model in DSH, send a
request, and confirm it in OCG **Logs**. When installation is blocked
(unsupported environment, missing DSH, a failed plugin command, or a conflict),
the page also shows the host's specific reason next to the status.

Installation spans two local stores, so rollback has one deliberate limit. If
DSH imports the Key and a later install step fails, OCG can restore the plugin
registration and any still-pending handoff, but it cannot prove that DSH's
credential write did not already commit. To retry, reopen the page and use
**Reinstall**, then start DSH once and confirm a request
in **Logs**. To remove the integration from a running Web or Desktop address,
use **Uninstall** on this page. That removes only
`@open-console-gateway/dsh-plugin` and leaves every other bundle, DSH
credentials, and the OCG Key in place. For an Editor-owned profile without a
runtime URL, remove the package through Editor's plugin management as well so
its startup restore does not reinstall it. If you are deliberately abandoning a
pending activation, remove the `credential-handoff` and matching `.claimed-*`
files shown by the page only while DSH is stopped. Removing the plugin does not
disable the OCG Key itself; rotate or disable that Key in OCG when required.

The native headless CLI supports this installation against the DSH on its own
host. The official Docker image reports local installation as unsupported: a
container cannot install into the browser user's or Docker host's DSH. It may
still serve DSH through ordinary Gateway configuration.

## Model Details In DSH

See [model metadata and reasoning tiers](model-metadata.md) for discovery,
route-specific declarations, and upgrading the installed OCG plugin.

---

[User guide index](../USER.md) · [简体中文](applications.zh-CN.md) · [Docs index](../README.md)
