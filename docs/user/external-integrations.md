[简体中文](external-integrations.zh-CN.md)

# External Integrations

External integrations are optional, locally supported services that extend OCG
Manager. The dashboard keeps its eight core views; supported surfaces appear
in the general **Extensions** group below the Settings divider.

## CPA

CPA (CLI Proxy API) is a local subscription runtime. Open Console Gateway can manage
its supported Codex, Claude, Antigravity, Kimi, and xAI account flows and route
the resulting subscription pool, but CPA remains the owner of OAuth browser
sessions, tokens, auth files, and internal scheduling. OCG stores only its
local connection configuration, the two CPA access credentials, and a local
model snapshot. The Management Key stays encrypted in OCG storage and, for a
managed child, reaches CPA only as `MANAGEMENT_PASSWORD`. CPA itself requires
client `api-keys` in its config, so the protected Inference Key and any
direct-client keys are necessarily present in CPA's local config under the OCG
data directory. Creating a client key returns that secret once from V3;
list views stay fingerprinted.

Use one of these local deployments:

- **Windows x64, macOS, or Linux x64 desktop app or CLI:** OCG can download
  the official CLIProxyAPI asset for that OS and CPU, keep it under the OCG
  data directory, and start it as an OCG-owned child. Start is manual; the
  child stops when OCG exits. OCG never stops a CPA process it did not start.
  Other OS/CPU combinations have no official asset and fail the install with
  an explicit reason.
- **Desktop or CLI:** run CPA on the same machine and configure a loopback URL
  such as `http://127.0.0.1:8317`.
- **Docker:** enable the optional Compose sibling described in
  [Docker](docker.md). OCG uses the read-only `http://cpa:8317` service URL;
  the dashboard does not accept a LAN, Internet, or cross-node CPA address.

URLs with embedded credentials, queries, fragments, redirects, or non-loopback
hosts are rejected. Do not reuse an Open Console Gateway Key as either CPA key.

### Connect and operate

1. On Windows x64, macOS, or Linux x64 (desktop app or CLI), install or start
   the managed CPA runtime
   from **Extensions → CPA**, or install and start CPA yourself on loopback
   and save its **Management Key** and **Inference Key**. The managed runtime
   generates those keys. Extra direct-client keys sit on **Overview**, are
   shown fingerprinted, and a newly created secret is returned once. The
   OCG-protected Inference Key cannot be deleted. The Management Key is not
   written into CPA's `config.yaml`; the Inference Key and direct-client keys
   are, because CPA requires `api-keys` in that file.
2. Open **Extensions → CPA**, save the local address and both keys when you
   connect an external CPA, then run the connection test. It reports reachability, supported CPA version,
   Management authentication, and Inference authentication separately.
   OCG requires CPA 7.1.0 or newer; later major versions continue through the
   same typed response and exact-account validation.
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
Cancelling does not delete credentials already saved by CPA. External CPA
connections use browser login: the current CPA version has no Codex device-login
Management API.

### Import a local CLI login

The CPA account page offers **new login** and **import from local CLI**. Detection
checks file presence only; clicking a provider's import button reads that one
credential file and sends an allowlisted conversion to CPA. This works with both
managed CPA and a connected local CPA. Open OCG's local dashboard on the machine
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

Static external integrations appear in **Extensions**. Contributions include
a typed Dashboard V3 adapter and a documented local boundary for code review. The
contribution path is in [Extending Open Console Gateway](../maintainer/extending.md).

---

[User guide index](../USER.md) · [简体中文](external-integrations.zh-CN.md) · [Docs index](../README.md)
