[简体中文](limits.zh-CN.md)

# Limits

This page lists explicit errors and unimplemented surfaces. The preferred and
supported protocol matrix lives in
[Protocol conversion](protocol-conversion.md).

- `/embeddings` is not implemented. Gemini `embedContent` is routed but
  returns a Google-style `501 UNIMPLEMENTED` response.
- Gemini `countTokens` also returns `501`; Gemini CLI is expected to fall
  back to local token estimation. Only `generateContent` and
  `streamGenerateContent` are forwarding actions.
- Non-empty Gemini `safetySettings` return `400` because a different upstream
  protocol cannot preserve their safety semantics. `null` and an empty array
  are accepted because they impose no policy.
- Gemini `cachedContent`, `fileData`, Google Search tools, `urlContext`, Code
  Execution, multimodal function-response parts, function response
  schemas/behavior, `VALIDATED` function calling, candidate counts other than
  one, and response modalities other than `TEXT` return `400`. Use base64
  `inlineData` for PNG, JPEG, GIF, or WebP images.
- Gemini `topK` and `thinkingConfig` are accepted only as cross-protocol
  compatibility hints. A native Chat Completions or Messages upstream may
  ignore them or implement different semantics; exact Gemini-equivalent
  sampling and thinking behavior is not guaranteed.
- Other non-null generation options that cannot be preserved, including
  `seed`, presence/frequency penalties, log-probability controls, and media
  resolution, return `400` instead of being silently discarded.
- Responses is stateless: requests must set `store: false`.
  `previous_response_id`, `conversation`, `store: true`, and
  `background: true` return `400` instead of being silently ignored.
- Responses image URLs and data URLs are supported; `input_image.file_id`
  returns `400` because the gateway has no Files API.
- Structured output and custom-tool grammar formats return `400` when
  cross-protocol conversion cannot preserve their constraints.
- Responses hosted tools such as `web_search`, `web_search_preview`, and
  `tool_search` cannot run on OpenCode-Go. Their declarations are dropped in
  automatic tool mode; explicitly forcing one returns a `400` error.
  Function, custom, and namespace tools are converted normally.
- Streaming token counts are accurate only when upstream emits usage chunks;
  Chat streams request `stream_options.include_usage`. Cost uses the active
  OpenCode Go pricing snapshot. Without usage, logs end as `success_no_usage`.
- Browser onboarding provides only manual page interaction; it does not
  register Google accounts, solve verification challenges, pay, scrape
  pages, or extract keys automatically.
- The installed Windows x64, macOS, and Linux x64 desktop dashboards can start
  Open Console Gateway in the tray when the user logs in. Development builds, CLI, and
  Docker do not expose that dashboard `auto_start` switch. Docker Compose
  separately uses `restart: unless-stopped`, so its service can restart with
  the Docker daemon.
- The macOS desktop dashboard can hide the Dock icon while retaining the
  menu-bar icon. Windows, Linux, CLI, and Docker do not expose the
  `show_dock_icon` switch.
- Windows / Linux ARM64 and 32-bit x86 builds are not published. RPM, Snap,
  app-store packages, Windows Authenticode signing, and Apple notarization
  are not implemented. That covers desktop installers only; the container
  images (`ghcr.io/klarkxy/opencode-go-mgr` and its `-browser` sidecar)
  publish `linux/amd64` and `linux/arm64`. Updater-enabled installed desktop
  builds can install signed releases from Settings; 1.4.1, development
  builds, the CLI, and Docker use the direct/manual upgrade path.
- Command Code GOAT is a live fixed-origin route. Its public `/models` catalog
  is refreshed explicitly on **Providers**; GOAT preset rows default on and
  additional rows default off. GOAT catalog refresh updates the model
  directory; Key auth is observed from inference 401/403. Its verified price
  snapshot estimates new request costs, with a saved editable multiplier per
  priced model. The account card can explicitly calibrate local
  `$14 / $35 / $70` windows from the first-party `/alpha/billing/credits`
  endpoint used by Command Code's official CLI, then continues accumulating
  priced OCG logs. The endpoint is not documented in the public Provider API,
  GOAT is never auto-synced, and manual correction remains available. Custom
  API is live under the trusted-administrator
  boundary in [Accounts](accounts.md); it is unpriced, has no official usage
  path, and its catalog, protocol, and pricing controls live on **Providers**
  as isolated `CustomEndpoint` scopes.
- Ollama Cloud monthly USD-credits usage is a soft estimate from locally priced
  logs. Used credit may exceed the Pro `$60` / Max `$300` / Team `$1000`
  limit; the dashboard clamps the bar at 100% and shows overage. Meter fullness
  never writes cooldown, disables the account, or changes routing. New Ollama
  accounts require an explicit Pro/Max/Team tier and purchase date. Migrated
  accounts with no billing row stay routeable without a meter. Actual upstream
  `429` still uses the generic cooldown/fallback path.
- Zen Free routing uses the card's enable switch and list position. There is
  no Deny / Explicit / Prefer policy.
- Unknown model names return `400` on every supported client format. Clients
  should send published aliases or eligible Custom IDs from authenticated
  `GET /v1/models` that currently have an effective enabled protocol.
  Protected `GET /dashboard/api/v3/application-models` is Go aliases ∩ active
  pricing, not that full client list.

---

[User guide index](../USER.md) · [简体中文](limits.zh-CN.md) · [Docs index](../README.md)
