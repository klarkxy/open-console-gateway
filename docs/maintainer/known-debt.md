[简体中文](known-debt.zh-CN.md)

# Known Debt And Non-Goals

This page describes the current implementation and project scope. Non-goals are
design boundaries, not requirements for a contributor's personal workflow.
Scope changes should explain their compatibility and maintenance impact in the
proposal or pull request, with corresponding code and documentation changes.

## Known Debt

- Auto-start is capability-gated: Windows x64, macOS, and Linux x64
  release/installed Tauri processes inject the login-start sync hook.
  Development builds, the CLI, and Docker dashboards do not expose the
  switch. Dock visibility is macOS Tauri only.
- Existing generated Tauri schema files are noisy in diffs; touch them when
  the Tauri config actually changed.
- Streaming cost is exact only when upstream emits usage chunks. Chat streams
  request `stream_options.include_usage`. Without a chunk, Go rows end as
  `success_no_usage`; Zen success without usage stays `success` / `free`.
- Legacy `profiles/<account_id>` WebView profiles stay on the old engine after
  upgrade, so users sign in again. The old path is retained for safe
  reset/delete cleanup.
- The Responses endpoint is stateless. `previous_response_id`, `conversation`,
  `store: true`, and `background: true` return `400`. See `protocol.rs` and
  [Limits](../user/limits.md).
- Gemini is a client compatibility format. Forwarding, `400`, and `501`
  behavior is listed in [Limits](../user/limits.md) and
  [Protocol conversion](../user/protocol-conversion.md).
- Claude Desktop advertises three fixed Claude aliases, mapped to the
  supported actual models.
- Command Code GOAT has no machine-readable usage endpoint. Its public model
  directory cannot validate a stored Key, so authentication failure is only
  known from real inference 401/403. Custom API remains a distinct live route
  under the trusted-administrator boundary (`custom.rs` + `custom_http.rs`).
- Per-model/per-protocol overrides are on V3. Custom account-level
  per-protocol probing has no V3 counterpart; the historical V2
  account-owned probe path is 410. Custom verify and model discovery are the
  live Custom operational paths.
- The V4 operation digest key (`dashboard_operation_digest_key`) still lives
  in the same SQLite file as the account Keys (AES-256-GCM `v2:` ciphertext,
  with legacy XOR readable until open-time repair). Authenticated local Key
  storage does not move this digest key; it remains beside Keys.
- The `accounts` row still holds cooldown columns and Key material. The
  identity model is satellite-based until the write switch in a later phase.

## Deliberate Non-Goals

- Dynamic adapter/plugin loading, user-defined adapter implementations, or
  adapters that own SQLite, `CoreState`, or a raw `reqwest::Client`. Typed
  user-defined Provider definitions remain supported data bound to the sealed
  Configurable HTTP adapter.
- Remote node sync, an Admin API, or a multi-tenant control plane.
- Tauri `invoke` as a dashboard data path; WebView commands stay removed.
- Request-time upstream discovery on `GET /v1/models` or
  `GET /dashboard/api/v3/application-models`.
- An authoritative GOAT usage API, or treating its public directory as Key
  verification.
- `/embeddings`, Gemini `embedContent` (501), or Gemini `countTokens` as a
  real upstream count (501 so Gemini CLI can fall back locally).
- Gemini as an upstream protocol.
- Automatic pricing or Zen catalog polling.
- Cross-engine reuse of legacy WebView profiles.
- Database downgrade support, or opening a newer schema with an older binary.
- Windows/Linux ARM64 desktop packages, 32-bit x86, RPM, Snap, app-store
  packages, Windows Authenticode, or Apple notarization. This does not exclude
  the supported Linux ARM64 container image.
- A second Cosign image signature on top of GitHub provenance.
---

[Maintainer guide index](../MAINTAINER.md) · [简体中文](known-debt.zh-CN.md) · [Docs index](../README.md)
