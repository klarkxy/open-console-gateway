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
- The browser session uses `browser-profiles/<account_id>`; legacy
  `profiles/<account_id>` profiles are not reused, so affected users sign in
  again. The legacy path is retained for safe reset/delete cleanup.
- The Responses endpoint is stateless. `previous_response_id`, `conversation`,
  `store: true`, and `background: true` return `400`. See `protocol.rs` and
  [Limits](../user/limits.md).
- Gemini is a client compatibility format. Forwarding, `400`, and `501`
  behavior is listed in [Limits](../user/limits.md) and
  [Protocol conversion](../user/protocol-conversion.md).
- Command Code GOAT account usage comes from the undocumented first-party
  `/alpha/billing/credits` endpoint used by the official CLI. Its response
  stability is not guaranteed by the public Provider API, so manual refresh
  validates exact GOAT caps and fails closed on schema or plan drift. Its
  public model directory still cannot validate a stored Key, so authentication
  failure is only known from real inference 401/403. Custom API uses the shared HTTP adapter under the trusted-administrator URL boundary.
- Per-model/per-protocol overrides still use remounted Account-path handlers
  under `/dashboard/api/v4`. Custom account-level per-protocol probing has no
  dedicated endpoint; that probe path returns 410. Custom verify and model
  discovery are the live Custom operational paths.
- The V4 operation digest key (`dashboard_operation_digest_key`) lives in the
  same SQLite file as the credential Keys (AES-256-GCM `v2:` ciphertext).
- V4 destination PATCH/DELETE use a shared transactional HTTP configuration service. Operational Account DTOs and legacy IDs remain compatibility boundaries for management and old imports; normal request planning reads destinations and execution credentials directly.


## Deliberate Non-Goals

- Dynamic adapter/plugin loading, user-defined adapter implementations, or
  adapters that own SQLite, `CoreState`, or a raw `reqwest::Client`. Typed
  user-defined Provider definitions remain supported data bound to the sealed
  Configurable HTTP adapter.
- Remote node sync, an Admin API, or a multi-tenant control plane.
- Tauri `invoke` or WebView commands as a dashboard data path.
- Request-time upstream discovery on `GET /v1/models`. `/dashboard/api/v3`
  is a 410 tombstone.
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
