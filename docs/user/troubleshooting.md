[简体中文](troubleshooting.zh-CN.md)

# Troubleshooting

Troubleshooting Open Console Gateway usually starts with discovering that something else
is already squatting on `127.0.0.1:9042`. The entries below cover stale SPAs,
conflicting writes, accounts that are cooling down, and Plans that look ready
but are still `pending` drafts — the gateway stays pessimistic so you do not
get billed for a bad guess.

- **On Windows, launching appears to do nothing and no tray icon is visible.**
  Launching again attempts to restore the existing desktop instance's tray
  and open its dashboard. A new instance creates its tray before starting
  the Gateway and displays startup failures in a dialog. If
  `ocg-manager-cli.exe` owns the port, the dialog shows its PID and executable
  path and lets you stop it and retry. Current requests are interrupted;
  accounts, configuration, and data are not deleted. Declining preserves
  the old service. Other port owners are diagnosed but never stopped; close
  that application first. Standalone CLI operation remains trayless.
  Windows may put the icon in its overflow menu; the application cannot
  force it to stay pinned on the taskbar. If an existing desktop instance
  is hung, end it in Task Manager and launch again. For source development
  only, `scripts/free-dev-port.mjs` clears stale Vite
  processes on port `30001`; it does not release `9042` or the desktop
  single-instance lock.
- **`401 Unauthorized` from the upstream.** Zen Free temporarily cools its
  anonymous channel and tries the next compatible card.
  OpenCode Go rotates and records `auth_error` only for a structured
  `CreditsError`; re-save the same Key after renewal to clear it. `ModelError`,
  unknown, and malformed OpenCode 401 responses remain unchanged. Custom API
  `401` rotates to the next eligible card and records `auth_error`. To check an OpenCode Go
  key directly, use CLI `key ping <id>` or send a real client request.
  Managed-account Key verification and Custom **Test connection**
  record `auth_error` on 401 in those flows.
- **The dashboard says the page version does not match the service.** A cached
  older SPA hit the tombstoned `/dashboard/api` REST and received HTTP 410.
  Refresh the page; if that is not enough, install the
  matching desktop, CLI, or Docker build.
- **A dashboard save failed with a conflict / 409.** Another tab in the same
  running process wrote first. The SPA refreshes the affected data from the
  server's `revisionConflict` code but does not replay the change. Review the
  current value, then submit again.
- **Local bar at 100% but requests still succeed.** That is a *false* circuit
  breaker — local accounting only. Continue using the account; the gateway
  will keep forwarding.
- **Local bar at 100% and the gateway returns `429`.** That is a *true*
  circuit breaker. Wait for `cooldown_until`, or reset the cooldown manually
  in the **Accounts** view.
- **Gateway returns `429` with "all accounts cooling down".** Every enabled
  account is in cooldown. Either wait for the soonest reset, or add / enable
  another account.
- **Gateway returns `400` for a model name.** Send a published alias or an
  eligible Custom ID from authenticated `GET /v1/models`. Names with `/`,
  `_`, or whitespace are raw IDs, not kebab aliases. Unknown names and
  overlapping raw IDs fail closed and never call upstream.
- **Command Code GOAT does not produce a route.** Confirm the account is
  enabled, ready, and has a non-empty Key, then check that the model's supported
  protocol is enabled in the **Providers** matrix. Public `/models` refresh does
  not validate the Key; an invalid Key surfaces as inference 401/403.
- **A saved Custom API still does not route.** Confirm the card is enabled and
  ready, its Key is non-empty, and the requested model is declared. Verification does
  not flip the switch. Verify sends one minimal request in the selected
  protocol to the resolved inference Endpoint and expects a `2xx` JSON response.
  Changing the API URL, Key, declared models, or protocol re-pends verification
  while preserving the card's current enabled state.
- **Gemini requests fail with `400` over `safetySettings`.** The gateway
  cannot map Google's safety thresholds to a Chat/Messages upstream, so it
  rejects non-empty arrays. Remove the field and retry; the Chat/Messages
  upstream applies its own policy.
- **Docker first-run registration does not pick up my
  `OCG_ADMIN_PASSWORD`.** These variables are only honored when the database
  has no administrator yet; use the stored administrator account. Recreate
  `ocg-data` and `ocg-browser-profiles` only for an intentional full reset
  after a verified backup — doing so erases every account, credential,
  setting, cookie, and browser profile.
- **SmartScreen / Gatekeeper warns about the installer or the DMG.** The
  current Windows builds are unsigned and the macOS app is ad-hoc signed. Use
  **Open Anyway** for the first launch; the warning is not a sign of
  tampering.

---

[User guide index](../USER.md) · [简体中文](troubleshooting.zh-CN.md) · [Docs index](../README.md)
