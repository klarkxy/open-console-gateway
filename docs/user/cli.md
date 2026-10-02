[简体中文](cli.zh-CN.md)

# CLI

`ocg-manager-cli serve` is the persistent host that applies control-plane mutations. `api` is its HTTP client. It does not create or open the database, even when `--data-dir` is also present. `schema` is offline JSON help: it does not contact `serve` and does not open a data directory.

The legacy `status`, `key list`, and `key ping` helpers open the local data directory only while its host is stopped. They acquire the same directory lock before initialization. While `serve` is running, use `api` to read settings, account records, gateway status, or run model tests.

This page is the operator contract for `serve`, `api`, and `schema`. If `ocg-manager-cli api --help` does not list the flags below, that binary is older than this page. The React dashboard and the desktop shell are a later interface. The same listener can still serve an existing `dist/` placed next to the executable.

On Windows the executable is `ocg-manager-cli.exe`. On Linux, `chmod +x ocg-manager-cli` after extraction. The default data directory is `~/.ocg-mgr-cli` on every platform. `serve` accepts `--data-dir <path>`. Its cipher is `--encryption-key`, otherwise `OCG_MANAGER_ENCRYPTION_KEY`, otherwise `<data-dir>/.encryption-key`. When that file is absent on Windows, `serve` falls back to the machine cipher. Prefer the key file. A cipher passed on the command line can land in shell history.

## Commands

Start the host and leave it running. `--port` is stored in SQLite; a later `serve` without the flag reuses it. The default bind is `127.0.0.1`. On that bind, with no `Origin` and no forwarded headers, loopback callers are already local administrators. `initialized` stays false until you register.

```bash
ocg-manager-cli --data-dir ./ocg-data serve --port 9042
```

Stop it with Ctrl+C. That process restores an owned CPA runtime when it starts, and shuts that owned process down when it exits. OAuth sessions, browser sessions, the update phase, and the settings epoch live in this process. A second process that opens the same directory is not a substitute.

From another terminal, name the listener once:

```bash
ocg-manager-cli --endpoint http://127.0.0.1:9042 api METHOD PATH \
  --input request.json \
  --output response.json \
  --cas-current \
  --session-file session.json
```

`--input -` reads one JSON document from stdin. Omit `--input` for a GET. `--output` is optional and is the private copy of the response. `--cas-current` and `--session-file` are optional; the sections below say when to pass them.

Inference uses the same `api` verb and a bearer file. The file is the gateway key, one line, and it is not a process argument.

```bash
ocg-manager-cli --endpoint http://127.0.0.1:9042 api POST /v1/chat/completions \
  --input chat.json \
  --key-file gateway-key.txt \
  --output chat-out.json
```

`schema` does not contact `serve` and does not open a data directory. It writes the offline JSON schema to stdout.

```bash
ocg-manager-cli schema v4
ocg-manager-cli schema v3
```

Names in the capability table are `$defs` in that output. V4-only bodies are in `schema v4`. Bodies owned by the remounted V3 handlers are in `schema v3`.

`api` accepts `/dashboard/api/v4/...`, the six inference paths below, and the preserved auth paths `/dashboard/api/auth/status`, `/dashboard/api/auth/register`, `/dashboard/api/auth/login`, and `/dashboard/api/auth/logout`. It rejects an absolute URL, a protocol-relative path, a `/dashboard/api/v3` path, any path that climbs out with `..`, and the preserved browser socket `/dashboard/api/browser/sessions/{token}/ws`. Those rejects exit non-zero and do not send the request. Prefer the v4 auth paths; the preserved copies share the same session.

`api` sends an ordinary HTTP request and reads the response. It does not upgrade a WebSocket, so there is no `api GET .../ws` command. That includes `/dashboard/api/v4/browser/sessions/{token}/ws`: the V4 prefix is not a socket client. The server still mounts the session socket. A remote viewer is later work, or point an external WebSocket client at the server. The usable native browser workflow is `GET /dashboard/api/v4/browser/capabilities` and `POST /dashboard/api/v4/accounts/{id}/browser`.

## Empty directory to a first inference

Placeholders below are synthetic. They are shaped like the schema and like `scripts/cli-acceptance.mjs`. They are not a live provider.

`status.json` after a fresh loopback start has `local: true`, `authenticated: true`, and `initialized: false`, plus `revision` and `processGeneration`. That public body is what `--cas-current` reads. It carries no password, cookie, or key. `GET /dashboard/api/v4/contract` is session-gated and is the wrong call for a host that has not been registered on a non-local bind.

```bash
ocg-manager-cli --endpoint http://127.0.0.1:9042 api GET /dashboard/api/v4/auth/status --output status.json
ocg-manager-cli --endpoint http://127.0.0.1:9042 api GET /dashboard/api/v4/templates --output templates.json
```

`register.json`, an `AuthRegister` with the two expectation fields left out because `--cas-current` supplies them:

```json
{ "username": "local-admin", "password": "synthetic-admin-password" }
```

```bash
ocg-manager-cli --endpoint http://127.0.0.1:9042 api POST /dashboard/api/v4/auth/register \
  --input register.json --cas-current --session-file session.json --output register-out.json
```

The password stays in `register.json`. It is not written to stdout. `--session-file` stores the session cookie. On the default loopback listener, later calls are authorized without that cookie. On any other bind, pass the same `--session-file` on each call. Registration does not bump `revision`. A second register returns 409 because an administrator already exists.

`onboarding.json`, an `OnboardingCommitRequest` with the expectation fields left out. `templateId` must be one you saw in `templates.json`; `custom` is the built-in HTTP template.

```json
{
  "operationId": "11111111-1111-4111-8111-111111111111",
  "mode": "complete",
  "authorizeCurrentEndpoint": true,
  "connection": {
    "kind": "new",
    "templateId": "custom",
    "name": "example-upstream",
    "endpointUrl": "http://127.0.0.1:9/v1/chat/completions",
    "upstreamProtocol": "chat_completions",
    "authKind": "none"
  },
  "authorization": { "kind": "none" },
  "targets": [{ "publicModel": "example-model", "upstreamModel": "vendor-model" }]
}
```

```bash
ocg-manager-cli --endpoint http://127.0.0.1:9042 api POST /dashboard/api/v4/onboarding/commit \
  --input onboarding.json --cas-current --output onboard.json
```

Read the account back with `GET /dashboard/api/v4/accounts/{id}` or find the row in `GET /dashboard/api/v4/account-records`. Those two lists are different reads. Publish the alias if the commit left it unpublished:

```json
{ "publicModel": "example-model", "published": true }
```

```bash
ocg-manager-cli --endpoint http://127.0.0.1:9042 api PATCH /dashboard/api/v4/alias-publication \
  --input publish.json --cas-current --output publish-out.json
```

`GET /dashboard/api/v4/connection` returns `ConnectionInfo`, including `primaryKey`. Write it with `--output connection.json`, then copy `primaryKey` into `gateway-key.txt` yourself. Without `--output`, stdout omits that key.

```bash
ocg-manager-cli --endpoint http://127.0.0.1:9042 api GET /dashboard/api/v4/connection --output connection.json
ocg-manager-cli --endpoint http://127.0.0.1:9042 api GET /v1/models --key-file gateway-key.txt --output models.json
```

`chat.json`:

```json
{ "model": "example-model", "messages": [{ "role": "user", "content": "ping" }] }
```

```bash
ocg-manager-cli --endpoint http://127.0.0.1:9042 api POST /v1/chat/completions \
  --input chat.json --key-file gateway-key.txt --output chat-out.json
```

The other inference paths are `POST /v1/responses`, `POST /v1/messages`, `POST /v1beta/models/{model}:generateContent`, and `POST /v1/models/{model}:generateContent`, each with `--key-file`. A streamed chat completion (`"stream": true`) is copied through as server-sent events. The client does not buffer it into a JSON rewrite. `POST /v1/responses` with `"store": true` fails before anything is sent upstream.

## Expectations

`--cas-current` is one explicit injection. It reads `revision` and `processGeneration` from `GET /dashboard/api/v4/auth/status` and writes them into the body as `expectedRevision` and `processGeneration` only where those keys are absent. A value already in the file is sent as written, including a stale one. The flag does not fill `expectedPricingRevision` or `expectedProviderPricingRevision`.

A mutation that requires the pair fails when both the file and the flag omit it. `PUT /dashboard/api/v4/settings` with only `{ "conversationSticky": false }` is that failure. The settings document is unchanged.

Read, edit, write:

```bash
ocg-manager-cli --endpoint http://127.0.0.1:9042 api GET /dashboard/api/v4/accounts/ACCOUNT_ID --output account.json
```

`rename.json` is an `AccountUpdate`. With the expectation keys omitted, `--cas-current` fills the live pair:

```json
{ "name": "example-renamed" }
```

```bash
ocg-manager-cli --endpoint http://127.0.0.1:9042 api PATCH /dashboard/api/v4/accounts/ACCOUNT_ID \
  --input rename.json --cas-current --output rename-out.json
```

A file that still carries the pair from before that rename is a stale expectation. `--cas-current` does not replace it:

```json
{ "expectedRevision": 1, "processGeneration": 1, "name": "must-not-commit" }
```

That PATCH exits non-zero. The stored name remains `example-renamed`. The client does not send the PATCH again.

The same rule covers 409, 429, a timeout, and a listener that has already moved. Edit the file, or drop the old pair and pass `--cas-current` on a new invocation you choose. Credit grants, key regeneration, credential rotate, and CPA client-key creation are the calls where a timed-out first request may already have committed. Read the resource, then decide. Do not paste a secret into the command line to find out.

After `serve` restarts, `processGeneration` in `auth/status` is a new value. A body saved from the previous process fails and is left unsent a second time. SQLite rows from the first process are still there.

These bodies reject an injected expectation. Leave `--cas-current` off. Their `$defs` are `AccountExportRequest`, `AccountImportPreviewRequest`, `ProxyTestRequest`, `CustomModelDiscoveryRequest`, `ProviderDefinitionDiscoverRequest`, `ProviderDefinitionTestRequest`, `AccountModelTestRequest`, and `CpaTestRequest`. `POST /dashboard/api/v4/destinations/{id}/model-tests` is the other model test: it requires CAS and its body is `DestinationModelTestRequest`. Import requires CAS; its body is `AccountImportRequest`.

## One send, then poll

Async work stays in `serve`. Poll the matching GET. The POST that started the work is not repeated when the GET is slow, when it returns 409, or when the port moves.

```bash
ocg-manager-cli --endpoint http://127.0.0.1:9042 api POST /dashboard/api/v4/external-integrations/cpa/oauth/start \
  --input oauth.json --cas-current --output oauth-start.json
ocg-manager-cli --endpoint http://127.0.0.1:9042 api GET /dashboard/api/v4/external-integrations/cpa/oauth/status \
  --output oauth-status.json
```

The same pattern is `POST /accounts/{id}/usage/refresh` (`UsageRefreshUpdate`) then `GET /accounts/{id}/usage`, and `GET /external-integrations/cpa/runtime` plus `GET /external-integrations/cpa/runtime/logs` after a runtime install, start, stop, or rollback. `GET /settings/update-status` is the update-phase read. On this headless host the install POST does not start an installer; see the updater rows.

A port change is one settings write, then a new endpoint. `SettingsUpdate` may contain only the fields you intend to change:

```json
{ "gatewayPort": 9043 }
```

```bash
ocg-manager-cli --endpoint http://127.0.0.1:9042 api PUT /dashboard/api/v4/settings \
  --input port.json --cas-current --output port-out.json
ocg-manager-cli --endpoint http://127.0.0.1:9043 api GET /dashboard/api/v4/settings --output settings.json
```

The second command talks to the listener that just bound `9043`. The PUT is not sent again. If the rebind fails, the host compensates and the old endpoint is the one that still answers. Read `gatewayPort` there before you change `--endpoint`.

`routingMode` and `conversationSticky` are fields of the same `SettingsUpdate`. They are not a second routing API. `PUT /routing/cards` and `PUT /routing/temporary-unavailability` are the policy writes.

## stdout, stderr, and the output file

| Stream | What it is for |
| --- | --- |
| stdout | `schema` JSON. An `api` body when `--output` is omitted, with gateway-key material removed. |
| stderr | Why a command failed. The process also exits non-zero. |
| `--output` | The response body, including `ConnectionInfo.primaryKey` or `CpaRuntimeKeyCreated.secret` when the handler returned one. |
| `--session-file` | The cookie from register or login. |
| `--key-file` | The bearer you supply for `/v1`. The client reads the file and does not echo it. |

A failed command does not print the request password on stdout and does not issue the call again. Read stderr for a local rejection or HTTP failure. An HTTP failure exits non-zero and preserves any existing `--output` file; that file is a prior response, not the failed request's body.

Key-creation and key-regeneration responses do not contain the new plaintext. The plaintext gateway key is the `primaryKey` on `GET /connection`, and only in the `--output` file. Browser payloads omit worker URLs and control tokens. Log and summary payloads are already redacted by the handler.

## Capability examples

One concrete call per family is enough to find the route. `schema v3` and `schema v4` hold the rest of the fields. This table is not a route registry. Paths are under `http://127.0.0.1:9042` unless noted. "CAS" means pass `--cas-current` or write both expectation fields yourself.

| Family | Example | Schema `$defs` |
| --- | --- | --- |
| Auth | `GET /dashboard/api/v4/auth/status` | v3 `AuthStatus` |
| Auth | `POST /dashboard/api/v4/auth/register` with `--session-file` | v3 `AuthRegister` |
| Auth | `POST /dashboard/api/v4/auth/login` and `POST .../auth/logout` | v3 `AuthLogin`, `AuthLogout` |
| Accounts | `GET /dashboard/api/v4/templates` | v4 `TemplateList` |
| Accounts | `POST /dashboard/api/v4/onboarding/commit` | v4 `OnboardingCommitRequest` |
| Accounts | `GET /dashboard/api/v4/accounts` and `GET /dashboard/api/v4/accounts/{id}` | v4 identity list; v3 `AccountUpdate` on `PATCH` |
| Accounts | `GET /dashboard/api/v4/account-records` | v3 account list, a different read from `GET /accounts` |
| Accounts | `POST /dashboard/api/v4/accounts` and `POST /dashboard/api/v4/accounts/managed` | v3 `AccountCreate`, `AccountManagedCreate` |
| Credentials | `POST /dashboard/api/v4/identities/{id}/credentials` | v4 `IdentityCredentialCreateRequest` |
| Credentials | `POST /dashboard/api/v4/credentials/{id}/rotate` | v4 `CredentialRotateRequest` |
| Credentials | `POST /dashboard/api/v4/credentials/{id}/quota-retry` | v3 `MutationExpectation` |
| Bindings | `PATCH /dashboard/api/v4/bindings/{id}` | v4 `BindingPatchRequest` |
| Platform accounts | `GET` or `POST /dashboard/api/v4/platform-accounts`, `POST .../{id}/import-keys` | v3 `PlatformAccounts` on GET, `PlatformCreate` on POST; v4 `PlatformKeyImportRequest` |
| Destinations | `GET /dashboard/api/v4/destinations`, `PATCH /dashboard/api/v4/destinations/{id}` | v4 `DestinationList`, `DestinationPatchRequest` |
| Catalog | `PUT /dashboard/api/v4/destinations/{id}/catalog` and `POST .../catalog/refresh` | v4 `DestinationCatalogUpdate` |
| Catalog | `PUT /dashboard/api/v4/provider-contracts/provider/{id}/catalog/model` | v4 `CatalogModelEditRequest` |
| Catalog | `POST /dashboard/api/v4/provider-contracts/{scopeKind}/{scopeId}/catalog/add` and `.../remove` | v4 `CatalogModelsAddRequest`, `CatalogModelsRemoveRequest` |
| Model metadata | `GET` or `PUT /dashboard/api/v4/destinations/{id}/model-metadata` | v4 `DestinationModelMetadataUpdate` on `PUT` |
| Protocol | `PUT /dashboard/api/v4/provider-contracts/provider/{id}/model-protocol-overrides` | v3 `ModelProtocolOverridesUpdate` |
| Protocol | `POST /dashboard/api/v4/providers/{id}/protocol-probes` | v3 `ProtocolProbeRequest` |
| Providers | `GET` or `POST /dashboard/api/v4/providers`, `PATCH /dashboard/api/v4/providers/{id}` | v3 `ProviderDefinitionCreate`, `ProviderDefinitionUpdate` |
| Pricing | `GET /dashboard/api/v4/providers/{id}/pricing` | v3 `ProviderPricing` |
| Pricing | `POST /dashboard/api/v4/providers/{id}/pricing/refresh` | v3 `ProviderPricingRefreshUpdate` |
| Pricing | `PUT /dashboard/api/v4/providers/{id}/pricing/multipliers` | v3 `PricingMultipliersUpdate` |
| Alias publication | `GET` or `PATCH /dashboard/api/v4/alias-publication` | v4 `AliasPublicationUpdate` |
| Routing | `GET /dashboard/api/v4/routing/explain` | v4 `RoutingExplanation` |
| Routing | `GET` or `PUT /dashboard/api/v4/routing/cards` | v4 `RoutingCardUpdate` |
| Policies | `GET` or `PUT /dashboard/api/v4/routing/temporary-unavailability` | v4 `TemporaryPolicyUpdate` |
| Policies | `POST /dashboard/api/v4/routing/temporary-unavailability/restrictions/{id}/clear` | v4 `TemporaryPolicyClearRequest` |
| Keys | `GET /dashboard/api/v4/connection` with `--output` | v3 `ConnectionInfo` |
| Keys | `POST /dashboard/api/v4/keys` and `POST /dashboard/api/v4/keys/primary/regenerate` | v3 `KeyCreate`; `PATCH /keys/{id}` is `KeyUpdate` |
| Billing | `GET /dashboard/api/v4/accounts/{id}/billing` | v4 credit meter |
| Billing | `POST /dashboard/api/v4/accounts/{id}/billing/credits/grants` | v4 `CreditGrantRequest` |
| Billing | `PUT` or `DELETE /dashboard/api/v4/accounts/{id}/billing/credits`, `POST .../calibrate` | v4 `CreditConfigureRequest`, `CreditCalibrationRequest` |
| Usage | `GET` or `PATCH /dashboard/api/v4/accounts/{id}/usage`, `POST .../usage/refresh` | v3 `UsageRefreshUpdate` |
| Official APIs | `GET /dashboard/api/v4/accounts/{id}/official-api`, `POST .../official-api/balance` | v3 `MutationExpectation` on POST; v4 `OfficialApiStatus` |
| Official APIs | `GET` or `POST /dashboard/api/v4/providers/{id}/official-api/pricing` | v4 `OfficialApiPrices` |
| Settings | `GET` or `PUT /dashboard/api/v4/settings` | v3 `SettingsUpdate` |
| Proxy | `POST /dashboard/api/v4/settings/test-proxy` | v3 `ProxyTestRequest` |
| Portable backup | `POST /dashboard/api/v4/accounts/transfer/export` and `.../preview` | v3 `AccountExportRequest`, `AccountImportPreviewRequest` |
| Portable backup | `POST /dashboard/api/v4/accounts/transfer/import` | v3 `AccountImportRequest` |
| Full data backup | Stop `serve`, copy the directory, copy it back, start again | No CLI subcommand. See [Upgrade, backup, restore](upgrade-backup.md). |
| Logs | `GET /dashboard/api/v4/logs/gateway`, `/logs/forward`, `/logs/forward/models`, `/logs/forward/keys` | Redacted handler bodies |
| Logs | `GET /dashboard/api/v4/gateway/status`, `/dashboard/summary`, `/dashboard/daily-tokens-by-model` | Session-gated reads |
| Browser | `GET /dashboard/api/v4/browser/capabilities` | `mode` is `native`, `remote`, or `unsupported` |
| Browser | `POST /dashboard/api/v4/accounts/{id}/browser` | v3 `BrowserOpenRequest` |
| Browser | `DELETE /dashboard/api/v4/accounts/{id}/browser-profile` | v3 `MutationExpectation` |
| Browser socket | Not an `api` call | Server still mounts it. The preserved path is outside the CLI allowlist. |
| CPA | `GET`, `PUT`, or `DELETE /dashboard/api/v4/external-integrations/cpa` | CAS on `PUT` and `DELETE` |
| CPA | `POST .../cpa/oauth/start`, then `GET .../cpa/oauth/status` | v3 `CpaOAuthStartRequest` |
| CPA process | `POST .../cpa/runtime/start`, `.../stop`, `.../rollback`; `GET .../runtime` and `.../runtime/logs` | CAS on the POSTs and on `DELETE .../runtime` |
| CPA keys | `POST /dashboard/api/v4/external-integrations/cpa/client-keys` | Response `CpaRuntimeKeyCreated` only in `--output` |
| CPA CLI import | `GET` or `POST /dashboard/api/v4/external-integrations/cpa/cli-imports` | v3 `CpaCliImportRequest` |
| CPA models | `GET` or `PUT /dashboard/api/v4/cpa/models` | v4 `CpaCatalogUpdate` |
| CPA models | `GET /dashboard/api/v4/external-integrations/cpa/models` | V3 integration snapshot, a different catalog |
| BYOK | `GET`, `POST`, or `DELETE /dashboard/api/v4/applications/byok/{client}` | v4 `ByokConfigureRequest`; `{client}` is `codex`, `kimi`, `minimax`, or `zcode` |
| BYOK | `POST /dashboard/api/v4/applications/byok/{client}/recover` | v4 `ByokMutationRequest` |
| DSH | `GET`, `POST`, or `DELETE /dashboard/api/v4/applications/dsh` | v4 `DshApplicationInstallRequest`, `DshApplicationUninstallRequest` |
| Updater | `GET /dashboard/api/v4/settings/check-update` and `GET .../settings/update-status` | Read-only phase and release check |
| Updater | `POST /dashboard/api/v4/settings/install-update` | CAS, and unavailable on this host |

### Gates the routes leave in place

Credential create stays routed and answers `allowed: false` with `external_integration`, `singleton`, `no_authentication`, `dedicated_account_flow`, `builtin_definition`, `unavailable`, or `draft`. Custom accounts and CPA use their own flows. Zen Free is a singleton. `POST /accounts/{id}/verify` does not probe when the plan's verification policy is not required. CPA returns not implemented for that probe.

`POST /providers/{id}/protocol-probes` runs for `opencode`, `opencode-zen-free`, `command-code`, `minimax`, and `kimi`. `ollama`, `custom`, and `cpa` stay on the route and return not implemented. Custom is rejected earlier as account-owned (`protocol probes for Custom API are account-owned`). The other refusal string is `protocol probes are not available for this Plan in this slice`.

`POST /providers/{id}/pricing/refresh` accepts `opencode`, `command-code`, and `ollama`. Any other id returns `provider does not support pricing refresh`. The body is `ProviderPricingRefreshUpdate`. `--cas-current` fills `expectedRevision` and `processGeneration`. You still set `expectedProviderPricingRevision` from `GET /providers/{id}/pricing`: use `pricingRevision` for `opencode`, and `providerPricingRevision` for `command-code` and `ollama`. A refresh that already holds the pricing lock returns 409 `provider pricing refresh is already running`. Leave that POST as the one you already sent, and read pricing again.

`PUT /providers/{id}/pricing/multipliers` accepts `opencode` and `command-code`. The body is `PricingMultipliersUpdate`. `expectedPricingRevision` is `pricingRevision` for `opencode` and `providerPricingRevision` for `command-code`. Other ids return `provider offering does not support pricing multipliers`.

Official balance and official price routes answer for a bearer `api` preset whose id is `deepseek` or `zhipu` and whose endpoint is that preset's official route. Anything else returns `official financial evidence is unavailable for this preset or destination`.

`POST /accounts/{id}/billing/credits/grants` is one send. If the client times out, read `GET /accounts/{id}/billing` before you consider another grant.

Transfer bodies are limited to 4 MiB by the router. Export's `bundle` is ciphertext (`ocg-manager-account-backup`). Preview and a wrong import password leave the database where it was. Import is the CAS write.

### Browser, CPA, BYOK, and DSH

The default CLI build enables `dsh-local-host` and `serve` registers the BYOK and DSH hosts. When a supported Chromium executable is found, it also registers the native browser launcher and stopper, and `GET /browser/capabilities` reports `mode: native`. Open a native session with `POST /accounts/{id}/browser` (`BrowserOpenRequest`). Remote mode is the worker at `OCG_BROWSER_WORKER_URL` with `OCG_BROWSER_CONTROL_TOKEN_FILE` (default file `/run/ocg-browser/control-token`), and only when a native launcher is not registered. Without either runtime, browser capabilities report `unsupported`. A remote session goes idle after 30 minutes and ends after 4 hours. Watching that session is a later remote viewer or an external WebSocket client. `api` does not attach to the socket. Profile reset and account open re-check CAS after the browser operation lock.

`GET /cpa/models` is the local CPA catalog. `GET /external-integrations/cpa/models` is the integration snapshot. Runtime start, stop, rollback, install, and update affect the supervisor inside this `serve`. OAuth start stores its session there too.

BYOK configure and remove, and DSH install and uninstall, are CAS writes. `codex` and `kimi` require `clientClosed: true`. Configure also requires `expectedFingerprint` from a prior GET. An empty published catalog returns 412 `No models are published by this Key yet; configure a model source first`. An unknown `{client}` returns `Unknown BYOK application`. The host creates or reuses an ordinary key named `codex`, `kimi-code`, `minimax-code`, or `zcode`.

A binary built with `--no-default-features` still exposes the routes. `GET /applications/byok/{client}` returns `status: unsupported_runtime` and the detail `Use a native OCG host on the client computer to configure this application.` `GET /applications/dsh` returns `unsupported_runtime` and `DSH installation is unavailable in this build; use the Desktop app or a native CLI on the DSH host`. POST and DELETE return 412 with `Native application configuration is unavailable on this host`. DSH install returns 412 `DSH installation is unavailable in this build; use the Desktop app or a native CLI on the DSH host`. DSH uninstall returns 412 `DSH uninstallation is unavailable in this build; use the Desktop app or a native CLI on the DSH host`. They do not return 404.

### Tray, Dock, and signed update

This host does not register auto-start, Dock visibility, or a signed desktop-update installer. Those belong to the deferred UI. `GET /settings` reports the support flags false. `PUT /settings` with `autoStart` returns 400 `auto-start is unavailable in this runtime`. `showDockIcon` returns 400 `Dock visibility is unavailable in this runtime`. `GET /settings/check-update` still performs the release check and reports `installSupported: false`. `POST /settings/install-update` returns 400 `desktop update installation is unavailable in this runtime` and does not bump the epoch. `GET /settings/update-status` remains the phase read.

## Backup

A full backup is the procedure in [Upgrade, backup, restore](upgrade-backup.md): stop `serve`, copy the entire data directory (including `data.sqlite` and `.encryption-key`), and restore by stopping, replacing that directory, and starting the same or a newer build. There is no `backup` subcommand.

Portable encrypted transfer is three files through `api`. Export and preview omit `--cas-current`. Import passes it. A wrong password fails the import and leaves revision unchanged.

```bash
ocg-manager-cli --endpoint http://127.0.0.1:9042 api POST /dashboard/api/v4/accounts/transfer/export \
  --input export.json --output export-out.json
ocg-manager-cli --endpoint http://127.0.0.1:9042 api POST /dashboard/api/v4/accounts/transfer/preview \
  --input preview.json --output preview-out.json
ocg-manager-cli --endpoint http://127.0.0.1:9042 api POST /dashboard/api/v4/accounts/transfer/import \
  --input import.json --cas-current --output import-out.json
```

`export.json` is `{ "bundlePassword": "synthetic-bundle-password" }`. Preview and import send `{ "password": "synthetic-bundle-password", "bundle": "<bundle from export-out.json>" }`. Import also receives the expectation pair from `--cas-current`.

## Build

Build this binary from the workspace with Cargo. The default feature is `dsh-local-host`.

```bash
cargo build -p ocg-manager-cli --locked
cargo build -p ocg-manager-cli --locked --no-default-features
```

Workspace quality is locked tests and Clippy with the core loopback feature:

```bash
cargo test --workspace --locked --features ocg-core/ollama-cloud-loopback-test
cargo clippy --workspace --all-targets --locked --features ocg-core/ollama-cloud-loopback-test -- -D warnings
```

The independent acceptance runner is Node, with no `pnpm`, `package.json`, or `src-tauri` step:

```bash
node scripts/cli-acceptance.mjs
```

The tag workflow remains the historical desktop release path. It is not a release of this all-function CLI. Use the binary you just built.

---

[User guide index](../USER.md) · [简体中文](cli.zh-CN.md) · [Docs index](../README.md)
