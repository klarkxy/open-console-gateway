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
- A non-null Gemini `generationConfig.topK` or `generationConfig.thinkingConfig`
  is rejected before HTTP. Conversion has no exact form for either value.
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
  Chat streams request `stream_options.include_usage`. The gateway does not
  estimate a price for a new request. A cost that was not recorded stays
  unknown and is not shown as zero or free. Without usage, logs end as
  `success_no_usage`.
- Browser onboarding provides only manual page interaction; it does not
  register Google accounts, solve verification challenges, pay, scrape
  pages, or extract keys automatically.
- The installed Windows x64, macOS, and Linux x64 desktop dashboards can start
  Open Console Gateway in the tray when the user logs in. Development builds,
  CLI, and Docker do not expose that dashboard `auto_start` switch. Docker
  Compose separately uses `restart: unless-stopped`, so its service can restart
  with the Docker daemon.
- The macOS desktop dashboard can hide the Dock icon while retaining the
  menu-bar icon. Windows, Linux, CLI, and Docker do not expose the
  `show_dock_icon` switch.
- Windows / Linux ARM64 and 32-bit x86 builds are not published. RPM, Snap,
  app-store packages, Windows Authenticode signing, and Apple notarization
  are not implemented. That covers desktop installers only; the container
  images (`ghcr.io/klarkxy/opencode-go-mgr` and its `-browser` sidecar)
  publish `linux/amd64` and `linux/arm64`. Updater-enabled installed desktop
  builds can install signed releases from Settings; development
  builds, the CLI, and Docker use the direct/manual upgrade path.
- Command Code GOAT is a live fixed-origin route. Its public `/models` catalog
  is refreshed explicitly on **Providers**, and also when **Refresh quota**
  runs on **Accounts**; GOAT preset and newly discovered rows default on when
  a supported protocol is known, except that GOAT's first snapshot starts only
  plan-included models on and keeps the rest of that snapshot off until enabled
  manually.

  GOAT catalog refresh updates the model directory; Key auth is observed from
  inference 401/403. The account card can explicitly **Refresh quota** to read
  official percentage windows from the first-party `/alpha/billing/credits`
  endpoint used by Command Code's official CLI. When that reading includes a
  percentage, the window uses it against a full window of 100 and keeps its
  reset. A dollar amount is not relabeled as a percentage. Later requests do
  not add a price onto the percentage. The endpoint is not documented in the
  public Provider API. GOAT is never auto-synced. You can save a manual
  percentage afterwards. With no official reading and no manual percentage,
  the window stays unavailable and is not shown as 0.

  Custom API is live under the trusted-administrator boundary in
  [Accounts](accounts.md). A missing cost stays unknown and is not shown as
  zero or free. A manual credit balance is not reduced by a completed
  request. There is no generic official usage path. Its catalog and protocol
  controls live on **Providers**.
- Account cards show three kinds of evidence when the Provider has it: timed
  quota windows, an official balance, and a manual credit balance. A missing
  observation stays unavailable and is not shown as 0. Official Go and GOAT
  windows use the observed percentage against 100. OpenCode Go, GOAT, and
  Ollama can save a manual percentage. Go uses the 5-hour, weekly, and monthly
  windows. Ollama uses the month window. Zen, MiniMax, Kimi, Custom, and CPA
  do not show that editor. A plan whose metadata explicitly turns manual
  calibration off does not show it either. The first saved percentage is only
  that quota window. Opening the page again, or reading it again while it
  stays open, shows that percentage and does not fill in a full billing
  status.
  A manual credit balance, a cash balance, and a platform site's
  observed consumption history stay separate. A manual credit balance
  keeps separate buckets, grants, monthly renewal, and expiry; **Calibrate
  usage** corrects the saved balance by hand. A completed request does not
  reduce that balance, and the product does not apply per-token rates or a
  currency conversion. Unknown stays unknown and is not recorded as zero or
  free. An empty or unknown balance does not disable a Key or change routing.
  Official remaining balance is the observed wallet. It is not a price-based
  monthly or lifetime spend.
- Ollama Cloud has no official usage API in this product and does not estimate
  a monthly credit meter from request prices. The account form still presents
  Pro, Max, or Team and a purchase date. A manual percentage does not require a
  price.
  A month percentage can be saved before a tier is chosen. A week percentage is
  not accepted.
  Without an official or manual usage observation, usage stays unavailable and
  is not shown as 0. Existing accounts stay routeable. Previously stored
  billing rows stay on disk and are not recalculated. A full window, where one
  exists, never writes cooldown, disables the account, or changes routing. An
  actual upstream `429` uses the generic cooldown/fallback path.
- Zen Free routing uses the card's enable switch and list position.
- Unknown model names return `400` on every supported client format. Clients
  should send published aliases or eligible Custom IDs from authenticated
  `GET /v1/models` that currently carry a validated derived protocol profile
  from the same qualified snapshot.
  Protected `GET /dashboard/api/v4/application-models` lists Go names that
  resolve in the saved catalog and have an enabled protocol. It does not
  consult a price snapshot, and it is not that full client list.

---

[User guide index](../USER.md) · [简体中文](limits.zh-CN.md) · [Docs index](../README.md)
