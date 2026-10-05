[简体中文](external-integrations.zh-CN.md)

# External Integrations

> Historical scope: published **Extensions → CPA** dashboard of the `open-console-gateway` generation. Not a current remote-CPA connection guide. This page adds no API and does not record a test or a completed migration.

This generation is the headless `ocg` CLI (`ocg.exe` on Windows, `ocg` on Linux and macOS). `ocg3` and `open-console-gateway` are two generation branches of one project; the product name stays Open Console Gateway. Execution is one local CPA owned by that process. Custom HTTP providers remain provider routes on that local CPA. They are not a second CPA and not a managed CPA instance. The default data root is `~/.ocg3`. A launch does not open, copy, move, delete, or adopt `~/.ocg-mgr` or `~/.ocg-mgr-cli`. The GPUI/Ely GUI is postponed. Whole CLI acceptance is pending. Commands are in the [CLI guide](cli.md). Design is in the [architecture](../architecture.md).

Source still mounts the existing `/dashboard/api/v4/external-integrations/cpa` prefix. Owned-runtime actions and the older connection fields share that prefix. This page adds no route.

The sections below record that published dashboard. They are not instructions to attach a separate CPA.

## CPA

In that dashboard, CPA (CLI Proxy API) was the subscription runtime for Codex, Claude, Antigravity, Kimi, and xAI account flows. OAuth browser sessions, tokens, auth files, and internal scheduling stayed in CPA. For an OCG-owned child, the management secret reached the child only as `MANAGEMENT_PASSWORD`. CPA's own config required client `api-keys`, so an inference key and direct-client keys were present in the child config under the OCG data directory. Creating a client key returned that secret once; list views stayed fingerprinted. The same page also stored a base URL, a management key, and an inference key for a separately run CPA. That separate connection is not part of this generation.

The page listed three deployments:

- **Owned child on Windows x64, macOS, or Linux x64 desktop or CLI.** The app could download the official CLIProxyAPI asset for that OS and CPU, keep it under the OCG data directory, and start it as an OCG-owned child. After a successful installation or manual start, OCG remembered that CPA should run. The child stopped when OCG exited, and the next OCG process started it again in the background. **Stop** cancelled that startup recovery, including after a failed recovery attempt. OCG never stopped a CPA process it did not start. Other OS/CPU combinations had no official asset, and the install failed with an explicit reason. That install path is not an accepted current runtime.
- **Separate loopback CPA.** The page told the operator to run CPA on the same machine and save a loopback URL such as `http://127.0.0.1:8317` with a management key and an inference key. That is not a current step.
- **Compose sibling.** The page told the operator to enable the optional profile in [Docker](docker.md#previous-generation-compose-cpa-profile) and use read-only `http://cpa:8317`. That is not a current step.

The connection form rejected URLs with embedded credentials, queries, fragments, redirects, or non-loopback hosts. An Open Console Gateway Key was not a CPA key.

Startup recovery reused the installed CPA version, configuration, and saved logins. It attempted startup once per OCG process. A failure stayed visible on the CPA page and did not block OCG or trigger a restart loop. An installation without a saved run intent stayed stopped until CPA was started. A separately connected CPA, including the Compose sibling, kept its own lifecycle.

A stored base URL, management cipher, or inference key from a separate connection stays stored. Starting the local runtime does not open that URL, does not copy those secrets into an owned credential or into the local child's management secret, and does not delete the row. Explicit migration is required and is not implemented. Do not print those secrets.

### Previous-generation connect and operate

1. On Windows x64, macOS, or Linux x64 (desktop app or CLI), the page could install or start the managed CPA runtime from **Extensions → CPA**. The managed runtime generated the management and inference keys. Extra direct-client keys sat on **Overview**, were shown fingerprinted, and a newly created secret was returned once. The OCG-protected Inference Key could not be deleted. The Management Key was not written into CPA's `config.yaml`; the Inference Key and direct-client keys were, because CPA required `api-keys` in that file.
2. The external-connection form saved a local address and both keys, then ran a connection test. The test reported reachability, supported CPA version, Management authentication, and Inference authentication separately. The published requirement was CPA 7.1.0 or newer; later major versions continued through the same typed response and exact-account validation. That form is not a current step.
3. A fresh managed installation can start successfully with an empty model
   catalog. This confirms CPA and its local authentication are working; it
   does not make any model routeable. Start an OAuth flow from CPA's account
   table. Browser-callback providers
   use CPA's loopback callback ports; Kimi and xAI use their device-code flow.
   OCG never runs an OAuth callback server and does not restore an old flow
   after a refresh or restart.
4. Open **Model catalog** and refresh it. The tab shows the saved snapshot as
   selectable cards grouped by the source CPA reported (`owned_by`). A
   highlighted card joins routing; unselected IDs stay in the snapshot but are
   not published. A first refresh, and models newly added by a later refresh,
   stay off until you select them. Catalogs without a saved selection
   keep routing every ID until you change them. A fresh install can start with
   an empty catalog; refresh after OAuth accounts exist. Then enable the CPA
   subscription pool. Its single **CPA subscription pool** card on Accounts can
   be ordered and enabled/disabled like other route candidates, but cannot
   expose a Key, be deleted, or stand in for individual CPA OAuth accounts.
   The card shows the managed runtime as running, stopped, not installed, or in
   an install/start phase; an external connection is labeled as such. A running
   or external pool is not grayed; a stopped, missing, or failed managed runtime
   is. For
   a managed runtime, extra direct-client keys live on Overview; daily use goes
   through the OCG Access Key.

If CPA and another catalog declare the same public model name, the Gateway
lists that name once and considers their routes in the configured account
order. Use the name shown on **Aliases**. Differently named mappings to the
same raw upstream ID remain ambiguous; slash-shaped raw IDs are not turned
into shared aliases.

Disabling the pool removes it from routing without forgetting CPA setup.
**Disconnect and clear** removes OCG's CPA configuration, the pool card, and
the local model snapshot after confirmation; it does not delete CPA's OAuth
files. A CPA fault simply removes that candidate from the current route, so
other eligible OCG accounts can still be selected.

Removing an OCG-managed CPA runtime is different: it deletes that owned
installation, its local runtime configuration, and the CPA OAuth credentials
under the managed `auth/` directory. It never deletes files belonging to an
externally operated CPA.

### Codex login methods

Codex offers **browser login** and **device login**. Device login requires an
installed, running OCG-managed CPA with `--codex-device-login` support (verified
with CPA 7.2.152). Open the authorization page and enter the displayed code;
enable device-code login in ChatGPT security or workspace settings.

Device login does not listen on port 1455, so it also works when Windows reserves
that port. OCG starts a separate contained CPA login process; CPA exchanges and
saves credentials without interrupting the gateway. Cancel, expiry (about 15
minutes), OCG exit, or a managed runtime lifecycle operation stops the helper.
Cancelling does not delete credentials already saved by CPA. A separately connected CPA used browser login: the CPA version documented with that page had no Codex device-login Management API for that connection.

### Import a local CLI login

The CPA account page offers **new login** and **import from local CLI**. Detection
checks file presence only; clicking a provider's import button reads that one
credential file and sends an allowlisted conversion to CPA. The published page offered that import for the managed child and for a separately connected local CPA. The separate connection is not a current step. Open OCG's local dashboard on the machine
that runs the CLI; remote dashboards cannot access these sources.

| CLI | Supported source | Boundary |
| --- | --- | --- |
| Codex | `$CODEX_HOME/auth.json`, default `~/.codex/auth.json` | ChatGPT OAuth with refresh token; API-key, external, and keychain-only logins are not imported |
| Claude Code | `$CLAUDE_CONFIG_DIR/.credentials.json`, default `~/.claude/.credentials.json` | OAuth with refresh token and `user:inference`; macOS Keychain requires fresh login unless the CLI already uses its file fallback |
| Kimi Code | `$KIMI_CODE_HOME/credentials/kimi-code.json`, default `~/.kimi-code/credentials/kimi-code.json` | Official Kimi Code OAuth file format |
| Grok CLI | `$GROK_HOME/auth.json`, default `~/.grok/auth.json` | Standard `https://auth.x.ai` OIDC entry with CPA's matching client ID; other keys/issuers are rejected |
| Antigravity | Not supported | No stable compatible local credential storage contract is established; use CPA login |

Sources that cannot be imported from this machine collapse into one tip; hover
for the detection reason and use Fresh sign-in above.

The **quota** on each account row is CPA's own local usage record for that OAuth
account, not the provider's official plan allowance. Empty records are hidden.
**Reset quota** clears CPA's counter only; it does not reset anything at the
provider.

Import is a one-time copy. OCG does not edit the CLI source, persist OAuth tokens
in its database, display them, or keep the two stores synchronized. CPA owns the
imported copy and refreshes it. Both copies share an authorization grant; refresh
or revocation can require another login. Existing matching imports are not
overwritten; remove an obsolete CPA entry explicitly before replacing it. Avoid
managing CPA accounts in another client while importing. When
CPA does not confirm an upload, OCG reports an unconfirmed result; refresh the
account list before retrying. Stable import filenames allow retries to reconcile
the same source identity or unchanged grant instead of blindly creating another file.

The formats were checked against CPA 7.2.152, official Codex storage, Claude
Code's documented file storage, Kimi Code commit `f9ca333`, and Grok CLI 1.0.13.
See [Codex storage](https://github.com/openai/codex/blob/main/codex-rs/login/src/auth/storage.rs),
[Claude storage](https://code.claude.com/docs/en/authentication#credential-management),
and [Kimi storage](https://github.com/MoonshotAI/kimi-code/blob/f9ca33376604ae91ea35a4ac1d6f1d4425a5aead/packages/oauth/src/storage.ts).

## Adding another integration

The published dashboard showed further static entries under **Extensions**. Contributions included
a typed Dashboard V3 adapter and a documented local boundary for code review. The
design source is the [architecture](../architecture.md). This page adds no integration and no route.

---

[User guide index](../USER.md) · [简体中文](external-integrations.zh-CN.md) · [Docs index](../README.md)
