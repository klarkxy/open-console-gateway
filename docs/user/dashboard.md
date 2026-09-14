[简体中文](dashboard.zh-CN.md)

# The Dashboard

The dashboard is the gateway's own single-page Vue 3 interface. **Dashboard**, **Access Keys**, **Accounts**, **Providers**, **Aliases**, **Logs**, and **Settings** are its seven fixed core views in the left rail (or horizontal menu below 1024px). A divider below Settings starts the optional **Extensions** group; CPA is its local-only entry. On a Windows x64, macOS, or Linux x64 desktop app or CLI, that page can also install and manually start an OCG-owned CPA runtime; other platforms keep connect-only CPA. Theme and language switches and a sign-out button live in the header. It speaks ten languages — 简体中文, 繁體中文, English, 日本語, 한국어, Español, Français, Deutsch, Português (Brasil), and Русский — with 简体中文 as the default. Your choice persists in `localStorage` under `ocg-manager.locale`; when persistence is unavailable, the in-memory locale still works for the session.

## Finding and editing configuration

The Add account search filters plans, presets, saved Providers, and platform
types together. Changing the search does not discard the selected form.
On narrow screens, provider tables scroll inside their panel while the scope
selector and catalog actions remain visible. Logs keep status, model, and time
filters in view; **More filters** reveals the rest and shows how many of those
filters are active. CPA pages guide you back to Overview when setup is needed.

## Dashboard V3

The current SPA talks only to **`/dashboard/api/v3`**. All views — Connection Center, Access Keys, Accounts, Providers, Aliases, Logs, Settings — plus login, register, and logout use that path. Writes carry `expectedRevision` and `processGeneration` for CAS; if another tab saves first, the server returns HTTP 409 with code `revisionConflict`. The SPA refreshes its control tokens and the affected resource, but never auto-replays the rejected change; review the current value and submit again. These tokens are process-local, so separate processes sharing one data directory are not a coordinated CAS domain. The OpenCode Go pricing snapshot uses its own `pricingRevision`, independent of the settings tokens.

Plaintext Keys travel only inside the Connection Center payload (`GET /dashboard/api/v3/connection`). The Settings resource never contains Key values. The browser keeps secrets in memory; signing out or a 401 session expiry wipes them immediately.

Views are cached while you switch tabs (`KeepAlive`) and refresh their server data when you return. The Dashboard view also refreshes when the browser tab comes back to the foreground. Catalogs, pricing, and provider directories are not polled automatically; official usage sync runs on the server. The Settings page may poll signed desktop install progress until the process restarts.

Cached pages that still call retired `/dashboard/api` REST receive HTTP 410 with code `dashboardV2Removed` and a prompt to refresh, then upgrade if needed. Anonymous retired REST is rejected with 401 before that 410. Two V2 families remain as compatibility exceptions for cached older pages: the auth endpoints (`/dashboard/api/auth/status`, `/dashboard/api/auth/register`, `/dashboard/api/auth/login`, `/dashboard/api/auth/logout`) and `/dashboard/api/browser/sessions/{token}/ws`. The current dashboard uses the V3 equivalents.

To test an OpenCode Go key, use CLI `key ping` or send a real client request. Custom cards still have **Verify connection**, and managed signup still performs Key verification.

## Connection Center

The first panel above the fold — and the only one that stays pinned to the top — is the **Connection Center**. It contains:

- The **Key**, with regenerate, one-click copy, and a **Manage access keys** action that opens the Access Keys view. Regenerating invalidates only the selected key's previous value; other keys keep working. When more than one enabled key exists, a selector switches the displayed masked value, copy target, and regenerate target. Copying places the full plaintext value on the clipboard; clear clipboard history after use on shared or public computers. Create, rename, enable, disable, and delete live on **Access Keys**, not here. The primary key is rotated the same way as a sub key; there is no custom-value field.
- The **API Base URL** (e.g. `http://127.0.0.1:9042/v1`) with one-click copy, plus the full Chat Completions, Responses, and Messages endpoints.
- An **HTTP warning** that appears whenever the resolved root URL is a non-loopback `http://` URL, warning that the Key and request contents would be transmitted in clear text.

The **Downstream Access Root** setting in **Settings** controls only the URLs the dashboard shows. Its effective value is selected in this order:

1. A non-empty `OCG_CLIENT_ROOT_URL` environment variable.
2. The manually saved dashboard value.
3. An automatic fallback: the current dashboard origin in production, or `http://127.0.0.1:<Gateway port>` in development.

While the environment variable is active, the input is read-only; changes take effect after restart and are never written to SQLite. The automatic value is shown in the input but is not saved.

Set an externally reachable root such as `https://ocg.example.com` when clients reach the gateway through a reverse proxy or a different host. A trailing `/v1` is accepted and removed automatically. This setting does **not** change the gateway bind address, configure DNS, or create a reverse proxy — those must already route to the running gateway. Plain HTTP is allowed for LAN deployments, but it exposes the Key and request contents to the network.

## Access Keys

The **Access Keys** view is the home for client-facing credentials. Primary and sub Keys live together in `access_keys` (schema v27). Create, rename, enable, disable, regenerate, and delete go through Dashboard V3; a successful change bumps the settings revision. Mutation acknowledgements do not include plaintext, so the page reloads Connection Center to show the new value.

- The **primary key** is always active and cannot be disabled or deleted; rotate it with the reset control. Its id is `00000000-0000-0000-0000-000000000001`. There is no custom-value field.
- **Sub keys** are additional credentials you create, name, rename, enable/disable, regenerate, or delete — useful for handing one key to each device. Deleting a sub key is a soft delete: it stops authenticating immediately and its plaintext is cleared, but forward logs keep resolving to its name. A sub key value may never equal the primary key value or another sub key value, and at most 64 non-deleted sub keys are supported.

Connection Center only copies enabled keys. Usage by key is filtered on the Logs view. See [Manual client setup](add-application.md) to connect a client.

---

[User guide index](../USER.md) · [简体中文](dashboard.zh-CN.md) · [Docs index](../README.md)
