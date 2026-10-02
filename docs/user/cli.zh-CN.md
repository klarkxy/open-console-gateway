[English](cli.md)

# CLI

`ocg-manager-cli serve` 是提交控制面变更的常驻服务。`api` 是它的 HTTP 客户端。即便同时带了 `--data-dir`，它也不创建、不打开数据库。`schema` 是离线 JSON 帮助：它不连接 `serve`，也不打开数据目录。

旧的 `status`、`key list`、`key ping` 辅助命令只在服务停止时打开本地数据目录，并在初始化前取得同一目录锁。`serve` 运行期间，通过 `api` 读取设置、账户记录、网关状态，或运行模型测试。

本页是 `serve`、`api`、`schema` 的操作约定。若 `ocg-manager-cli api --help` 里没有下面这些参数，那份可执行文件比本页旧。React 面板和桌面外壳是后续界面。监听器仍可提供与可执行文件放在一起的既有 `dist/`。

Windows 上的可执行文件是 `ocg-manager-cli.exe`。Linux 解压后执行 `chmod +x ocg-manager-cli`。各平台默认数据目录都是 `~/.ocg-mgr-cli`。`serve` 用 `--data-dir <path>` 改它。混淆密钥依次为 `--encryption-key`、环境变量 `OCG_MANAGER_ENCRYPTION_KEY`、`<data-dir>/.encryption-key`。Windows 上该文件不存在时，`serve` 退回机器绑定的密钥。优先用密钥文件。写在命令行上的密钥会进 shell 历史。

## 命令

启动宿主并让它一直运行。`--port` 写入 SQLite；之后不带该参数的 `serve` 会沿用这个端口。默认绑定 `127.0.0.1`。该绑定上没有 `Origin`、也没有转发头时，环回调用者已经是本地管理员。注册之前 `initialized` 为 false。

```bash
ocg-manager-cli --data-dir ./ocg-data serve --port 9042
```

用 Ctrl+C 停止。该进程启动时恢复自己托管的 CPA 运行时，退出时关掉这个托管进程。OAuth 会话、浏览器会话、更新阶段和 settings epoch 都在这个进程里。另一个进程打开同一目录并不能代替它。

在另一个终端指定监听地址：

```bash
ocg-manager-cli --endpoint http://127.0.0.1:9042 api METHOD PATH \
  --input request.json \
  --output response.json \
  --cas-current \
  --session-file session.json
```

`--input -` 从 stdin 读一份 JSON。GET 可以不带 `--input`。`--output` 可选，它是响应的私有副本。`--cas-current` 和 `--session-file` 可选；何时使用见下文。

推理走同一个 `api`，外加一份 bearer 文件。文件里是网关 Key，一行，不放进进程参数。

```bash
ocg-manager-cli --endpoint http://127.0.0.1:9042 api POST /v1/chat/completions \
  --input chat.json \
  --key-file gateway-key.txt \
  --output chat-out.json
```

`schema` 不连接 `serve`，也不打开数据目录。它把离线 JSON schema 写到 stdout。

```bash
ocg-manager-cli schema v4
ocg-manager-cli schema v3
```

能力表里的名字是该输出中的 `$defs`。只属于 V4 的请求体在 `schema v4`。由重新挂载的 V3 处理函数拥有的请求体在 `schema v3`。

`api` 接受 `/dashboard/api/v4/...`、下面六条推理路径，以及保留的认证路径 `/dashboard/api/auth/status`、`/dashboard/api/auth/register`、`/dashboard/api/auth/login` 和 `/dashboard/api/auth/logout`。绝对 URL、协议相对路径、`/dashboard/api/v3` 路径、用 `..` 爬出前缀的路径，以及保留的浏览器套接字 `/dashboard/api/browser/sessions/{token}/ws` 会被拒绝。这些拒绝以非零退出，且不发送请求。认证优先走 v4 路径；保留副本共用同一会话。

`api` 发送普通 HTTP 请求并读取响应。它不升级 WebSocket，因此没有 `api GET .../ws` 命令。`/dashboard/api/v4/browser/sessions/{token}/ws` 也一样：V4 前缀并不是套接字客户端。服务器仍挂载该会话套接字。远程查看器是后续工作，也可以把外部 WebSocket 客户端指到服务器。可用的原生浏览器流程是 `GET /dashboard/api/v4/browser/capabilities` 和 `POST /dashboard/api/v4/accounts/{id}/browser`。

## 从空目录到第一次推理

下面的占位符是合成值。形状与 schema 以及 `scripts/cli-acceptance.mjs` 一致。它们不是真实供应商。

新建的环回宿主上，`status.json` 里 `local` 为 true，`authenticated` 为 true，`initialized` 为 false，并带有 `revision` 与 `processGeneration`。`--cas-current` 读的就是这份公开响应。其中没有密码、cookie 或 Key。`GET /dashboard/api/v4/contract` 需要会话；在非本地绑定上，用它为尚未注册的宿主取期望值是错误的。

```bash
ocg-manager-cli --endpoint http://127.0.0.1:9042 api GET /dashboard/api/v4/auth/status --output status.json
ocg-manager-cli --endpoint http://127.0.0.1:9042 api GET /dashboard/api/v4/templates --output templates.json
```

`register.json` 是省略两个期望字段的 `AuthRegister`，由 `--cas-current` 填入：

```json
{ "username": "local-admin", "password": "synthetic-admin-password" }
```

```bash
ocg-manager-cli --endpoint http://127.0.0.1:9042 api POST /dashboard/api/v4/auth/register \
  --input register.json --cas-current --session-file session.json --output register-out.json
```

密码留在 `register.json`，不会写到 stdout。`--session-file` 保存会话 cookie。默认环回监听上，后续调用不靠这份 cookie 授权。其他绑定上，每次调用都带同一个 `--session-file`。注册不推进 `revision`。管理员已存在时，再次注册返回 409。

`onboarding.json` 是省略期望字段的 `OnboardingCommitRequest`。`templateId` 必须是 `templates.json` 里见过的值；`custom` 是内置 HTTP 模板。

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

用 `GET /dashboard/api/v4/accounts/{id}` 读回账号，或在 `GET /dashboard/api/v4/account-records` 里找那一行。这两个列表是不同的读取。若提交后别名尚未发布：

```json
{ "publicModel": "example-model", "published": true }
```

```bash
ocg-manager-cli --endpoint http://127.0.0.1:9042 api PATCH /dashboard/api/v4/alias-publication \
  --input publish.json --cas-current --output publish-out.json
```

`GET /dashboard/api/v4/connection` 返回 `ConnectionInfo`，其中含 `primaryKey`。用 `--output connection.json` 写入，再自行把 `primaryKey` 抄进 `gateway-key.txt`。不带 `--output` 时，stdout 会略去这把 Key。

```bash
ocg-manager-cli --endpoint http://127.0.0.1:9042 api GET /dashboard/api/v4/connection --output connection.json
ocg-manager-cli --endpoint http://127.0.0.1:9042 api GET /v1/models --key-file gateway-key.txt --output models.json
```

`chat.json`：

```json
{ "model": "example-model", "messages": [{ "role": "user", "content": "ping" }] }
```

```bash
ocg-manager-cli --endpoint http://127.0.0.1:9042 api POST /v1/chat/completions \
  --input chat.json --key-file gateway-key.txt --output chat-out.json
```

其余推理路径是 `POST /v1/responses`、`POST /v1/messages`、`POST /v1beta/models/{model}:generateContent` 和 `POST /v1/models/{model}:generateContent`，都要带 `--key-file`。带 `"stream": true` 的 chat completion 按服务器发送事件原样拷贝，客户端不会把它缓冲成另一份 JSON。`POST /v1/responses` 且 `"store": true` 时，请求在转发到上游之前失败。

## 期望值

`--cas-current` 是一次显式注入。它从 `GET /dashboard/api/v4/auth/status` 读取 `revision` 和 `processGeneration`，仅在请求体缺少对应键时写入 `expectedRevision` 和 `processGeneration`。文件里已经写了的值会照原样发送，包括过期值。该参数不填写 `expectedPricingRevision` 或 `expectedProviderPricingRevision`。

需要这对字段的变更，如果文件和参数都没提供，就会失败。只带 `{ "conversationSticky": false }` 的 `PUT /dashboard/api/v4/settings` 就是这种失败。设置文档保持原样。

读取、修改、再写入：

```bash
ocg-manager-cli --endpoint http://127.0.0.1:9042 api GET /dashboard/api/v4/accounts/ACCOUNT_ID --output account.json
```

`rename.json` 是 `AccountUpdate`。省略期望字段时，`--cas-current` 填入当前这一对：

```json
{ "name": "example-renamed" }
```

```bash
ocg-manager-cli --endpoint http://127.0.0.1:9042 api PATCH /dashboard/api/v4/accounts/ACCOUNT_ID \
  --input rename.json --cas-current --output rename-out.json
```

文件里若仍是这次改名之前的那一对，就是过期期望。`--cas-current` 不会替换它：

```json
{ "expectedRevision": 1, "processGeneration": 1, "name": "must-not-commit" }
```

该 PATCH 以非零退出。已保存的名称仍是 `example-renamed`。客户端不会再发一次 PATCH。

409、429、超时，以及监听器已经迁走，都按同一规则处理。改文件，或者删掉旧的那一对，再由你自己发起一次新的 `--cas-current`。额度授予、Key 轮换、凭据轮换和 CPA 客户端 Key 创建在第一次请求超时后可能已经提交。先读资源，再决定。不要为了确认而把秘密放进命令行。

`serve` 重启后，`auth/status` 里的 `processGeneration` 是新值。上一进程保存的请求体失败，并且不会被自动再发一次。上一进程写入的 SQLite 行还在。

下列请求体拒绝被注入期望字段，不要加 `--cas-current`。它们的 `$defs` 是 `AccountExportRequest`、`AccountImportPreviewRequest`、`ProxyTestRequest`、`CustomModelDiscoveryRequest`、`ProviderDefinitionDiscoverRequest`、`ProviderDefinitionTestRequest`、`AccountModelTestRequest` 和 `CpaTestRequest`。`POST /dashboard/api/v4/destinations/{id}/model-tests` 是另一项模型测试：它需要 CAS，请求体是 `DestinationModelTestRequest`。导入需要 CAS，请求体是 `AccountImportRequest`。

## 发送一次，然后轮询

异步工作留在 `serve` 里。轮询对应的 GET。启动工作的那次 POST 不会因为 GET 慢、返回 409 或端口改变而重发。

```bash
ocg-manager-cli --endpoint http://127.0.0.1:9042 api POST /dashboard/api/v4/external-integrations/cpa/oauth/start \
  --input oauth.json --cas-current --output oauth-start.json
ocg-manager-cli --endpoint http://127.0.0.1:9042 api GET /dashboard/api/v4/external-integrations/cpa/oauth/status \
  --output oauth-status.json
```

同一模式还有：`POST /accounts/{id}/usage/refresh`（`UsageRefreshUpdate`）之后 `GET /accounts/{id}/usage`；运行时安装、启动、停止或回滚之后读 `GET /external-integrations/cpa/runtime` 和 `GET /external-integrations/cpa/runtime/logs`。`GET /settings/update-status` 是更新阶段的读取。在这个无头宿主上，安装用的 POST 不会启动安装器；见更新器各行。

改端口是一次设置写入，然后换一个 endpoint。`SettingsUpdate` 可以只包含你要改的字段：

```json
{ "gatewayPort": 9043 }
```

```bash
ocg-manager-cli --endpoint http://127.0.0.1:9042 api PUT /dashboard/api/v4/settings \
  --input port.json --cas-current --output port-out.json
ocg-manager-cli --endpoint http://127.0.0.1:9043 api GET /dashboard/api/v4/settings --output settings.json
```

第二条命令连的是刚刚绑定 `9043` 的监听器。PUT 不会再发。若重新绑定失败，宿主会补偿，仍在应答的是旧 endpoint。先读那里的 `gatewayPort`，再改 `--endpoint`。

`routingMode` 和 `conversationSticky` 是同一次 `SettingsUpdate` 的字段，不是另一套路由 API。`PUT /routing/cards` 和 `PUT /routing/temporary-unavailability` 才是策略写入。

## stdout、stderr 和输出文件

| 流向 | 用途 |
| --- | --- |
| stdout | `schema` 的 JSON。未指定 `--output` 时的 `api` 响应体，其中的网关 Key 材料会被去掉。 |
| stderr | 命令失败的原因。进程同时以非零退出。 |
| `--output` | 响应体。处理函数若返回了 `ConnectionInfo.primaryKey` 或 `CpaRuntimeKeyCreated.secret`，它们在这里。 |
| `--session-file` | 注册或登录得到的 cookie。 |
| `--key-file` | 你为 `/v1` 提供的 bearer。客户端读取该文件，不回显。 |

失败的命令不会把请求密码打到 stdout，也不会再发一次调用。本地拒绝或 HTTP 失败的原因在 stderr 中。HTTP 失败以非零退出，并保留既有的 `--output` 文件；该文件是以前的响应，不是本次失败请求的响应体。

创建 Key 和轮换 Key 的响应不含新的明文。明文网关 Key 是 `GET /connection` 的 `primaryKey`，且只在 `--output` 文件中。浏览器载荷不含 worker URL 和控制令牌。日志和摘要载荷已由处理函数脱敏。

## 能力示例

每个家族一条具体调用，用来定位路由。其余字段在 `schema v3` 和 `schema v4`。这张表不是路由注册表。除特别注明外，路径都在 `http://127.0.0.1:9042` 之下。「CAS」表示传递 `--cas-current`，或自己写入两个期望字段。

| 家族 | 示例 | Schema `$defs` |
| --- | --- | --- |
| 认证 | `GET /dashboard/api/v4/auth/status` | v3 `AuthStatus` |
| 认证 | `POST /dashboard/api/v4/auth/register`，带 `--session-file` | v3 `AuthRegister` |
| 认证 | `POST /dashboard/api/v4/auth/login` 与 `POST .../auth/logout` | v3 `AuthLogin`、`AuthLogout` |
| 账号 | `GET /dashboard/api/v4/templates` | v4 `TemplateList` |
| 账号 | `POST /dashboard/api/v4/onboarding/commit` | v4 `OnboardingCommitRequest` |
| 账号 | `GET /dashboard/api/v4/accounts` 与 `GET /dashboard/api/v4/accounts/{id}` | v4 身份列表；`PATCH` 用 v3 `AccountUpdate` |
| 账号 | `GET /dashboard/api/v4/account-records` | v3 账号列表，与 `GET /accounts` 不是同一次读取 |
| 账号 | `POST /dashboard/api/v4/accounts` 与 `POST /dashboard/api/v4/accounts/managed` | v3 `AccountCreate`、`AccountManagedCreate` |
| 凭据 | `POST /dashboard/api/v4/identities/{id}/credentials` | v4 `IdentityCredentialCreateRequest` |
| 凭据 | `POST /dashboard/api/v4/credentials/{id}/rotate` | v4 `CredentialRotateRequest` |
| 凭据 | `POST /dashboard/api/v4/credentials/{id}/quota-retry` | v3 `MutationExpectation` |
| 绑定 | `PATCH /dashboard/api/v4/bindings/{id}` | v4 `BindingPatchRequest` |
| 平台账号 | `GET` 或 `POST /dashboard/api/v4/platform-accounts`，`POST .../{id}/import-keys` | GET 为 v3 `PlatformAccounts`，POST 为 `PlatformCreate`；v4 `PlatformKeyImportRequest` |
| 目的地 | `GET /dashboard/api/v4/destinations`，`PATCH /dashboard/api/v4/destinations/{id}` | v4 `DestinationList`、`DestinationPatchRequest` |
| 目录 | `PUT /dashboard/api/v4/destinations/{id}/catalog` 与 `POST .../catalog/refresh` | v4 `DestinationCatalogUpdate` |
| 目录 | `PUT /dashboard/api/v4/provider-contracts/provider/{id}/catalog/model` | v4 `CatalogModelEditRequest` |
| 目录 | `POST /dashboard/api/v4/provider-contracts/{scopeKind}/{scopeId}/catalog/add` 与 `.../remove` | v4 `CatalogModelsAddRequest`、`CatalogModelsRemoveRequest` |
| 模型元数据 | `GET` 或 `PUT /dashboard/api/v4/destinations/{id}/model-metadata` | `PUT` 为 v4 `DestinationModelMetadataUpdate` |
| 协议 | `PUT /dashboard/api/v4/provider-contracts/provider/{id}/model-protocol-overrides` | v3 `ModelProtocolOverridesUpdate` |
| 协议 | `POST /dashboard/api/v4/providers/{id}/protocol-probes` | v3 `ProtocolProbeRequest` |
| 供应商 | `GET` 或 `POST /dashboard/api/v4/providers`，`PATCH /dashboard/api/v4/providers/{id}` | v3 `ProviderDefinitionCreate`、`ProviderDefinitionUpdate` |
| 定价 | `GET /dashboard/api/v4/providers/{id}/pricing` | v3 `ProviderPricing` |
| 定价 | `POST /dashboard/api/v4/providers/{id}/pricing/refresh` | v3 `ProviderPricingRefreshUpdate` |
| 定价 | `PUT /dashboard/api/v4/providers/{id}/pricing/multipliers` | v3 `PricingMultipliersUpdate` |
| 别名发布 | `GET` 或 `PATCH /dashboard/api/v4/alias-publication` | v4 `AliasPublicationUpdate` |
| 路由 | `GET /dashboard/api/v4/routing/explain` | v4 `RoutingExplanation` |
| 路由 | `GET` 或 `PUT /dashboard/api/v4/routing/cards` | v4 `RoutingCardUpdate` |
| 策略 | `GET` 或 `PUT /dashboard/api/v4/routing/temporary-unavailability` | v4 `TemporaryPolicyUpdate` |
| 策略 | `POST /dashboard/api/v4/routing/temporary-unavailability/restrictions/{id}/clear` | v4 `TemporaryPolicyClearRequest` |
| Key | `GET /dashboard/api/v4/connection`，带 `--output` | v3 `ConnectionInfo` |
| Key | `POST /dashboard/api/v4/keys` 与 `POST /dashboard/api/v4/keys/primary/regenerate` | v3 `KeyCreate`；`PATCH /keys/{id}` 为 `KeyUpdate` |
| 计费 | `GET /dashboard/api/v4/accounts/{id}/billing` | v4 额度表 |
| 计费 | `POST /dashboard/api/v4/accounts/{id}/billing/credits/grants` | v4 `CreditGrantRequest` |
| 计费 | `PUT` 或 `DELETE /dashboard/api/v4/accounts/{id}/billing/credits`，`POST .../calibrate` | v4 `CreditConfigureRequest`、`CreditCalibrationRequest` |
| 用量 | `GET` 或 `PATCH /dashboard/api/v4/accounts/{id}/usage`，`POST .../usage/refresh` | v3 `UsageRefreshUpdate` |
| 官方 API | `GET /dashboard/api/v4/accounts/{id}/official-api`，`POST .../official-api/balance` | POST 为 v3 `MutationExpectation`；v4 `OfficialApiStatus` |
| 官方 API | `GET` 或 `POST /dashboard/api/v4/providers/{id}/official-api/pricing` | v4 `OfficialApiPrices` |
| 设置 | `GET` 或 `PUT /dashboard/api/v4/settings` | v3 `SettingsUpdate` |
| 代理 | `POST /dashboard/api/v4/settings/test-proxy` | v3 `ProxyTestRequest` |
| 便携备份 | `POST /dashboard/api/v4/accounts/transfer/export` 与 `.../preview` | v3 `AccountExportRequest`、`AccountImportPreviewRequest` |
| 便携备份 | `POST /dashboard/api/v4/accounts/transfer/import` | v3 `AccountImportRequest` |
| 全量数据备份 | 停止 `serve`，复制目录，再复制回去，然后启动 | 没有 CLI 子命令。见[升级、备份、恢复](upgrade-backup.zh-CN.md)。 |
| 日志 | `GET /dashboard/api/v4/logs/gateway`、`/logs/forward`、`/logs/forward/models`、`/logs/forward/keys` | 已脱敏的处理函数响应 |
| 日志 | `GET /dashboard/api/v4/gateway/status`、`/dashboard/summary`、`/dashboard/daily-tokens-by-model` | 需要会话的读取 |
| 浏览器 | `GET /dashboard/api/v4/browser/capabilities` | `mode` 为 `native`、`remote` 或 `unsupported` |
| 浏览器 | `POST /dashboard/api/v4/accounts/{id}/browser` | v3 `BrowserOpenRequest` |
| 浏览器 | `DELETE /dashboard/api/v4/accounts/{id}/browser-profile` | v3 `MutationExpectation` |
| 浏览器套接字 | 不是 `api` 调用 | 服务器仍挂载。保留路径不在 CLI 允许列表中。 |
| CPA | `GET`、`PUT` 或 `DELETE /dashboard/api/v4/external-integrations/cpa` | `PUT` 与 `DELETE` 需要 CAS |
| CPA | `POST .../cpa/oauth/start`，然后 `GET .../cpa/oauth/status` | v3 `CpaOAuthStartRequest` |
| CPA 进程 | `POST .../cpa/runtime/start`、`.../stop`、`.../rollback`；`GET .../runtime` 与 `.../runtime/logs` | POST 与 `DELETE .../runtime` 需要 CAS |
| CPA Key | `POST /dashboard/api/v4/external-integrations/cpa/client-keys` | 响应 `CpaRuntimeKeyCreated` 只在 `--output` |
| CPA CLI 导入 | `GET` 或 `POST /dashboard/api/v4/external-integrations/cpa/cli-imports` | v3 `CpaCliImportRequest` |
| CPA 模型 | `GET` 或 `PUT /dashboard/api/v4/cpa/models` | v4 `CpaCatalogUpdate` |
| CPA 模型 | `GET /dashboard/api/v4/external-integrations/cpa/models` | V3 集成快照，是另一份目录 |
| BYOK | `GET`、`POST` 或 `DELETE /dashboard/api/v4/applications/byok/{client}` | v4 `ByokConfigureRequest`；`{client}` 为 `codex`、`kimi`、`minimax` 或 `zcode` |
| BYOK | `POST /dashboard/api/v4/applications/byok/{client}/recover` | v4 `ByokMutationRequest` |
| DSH | `GET`、`POST` 或 `DELETE /dashboard/api/v4/applications/dsh` | v4 `DshApplicationInstallRequest`、`DshApplicationUninstallRequest` |
| 更新器 | `GET /dashboard/api/v4/settings/check-update` 与 `GET .../settings/update-status` | 只读的阶段与发行检查 |
| 更新器 | `POST /dashboard/api/v4/settings/install-update` | 需要 CAS，且在此宿主上不可用 |

### 路由仍然保留的门禁

凭据创建路由仍在，会以 `allowed: false` 回答，原因是 `external_integration`、`singleton`、`no_authentication`、`dedicated_account_flow`、`builtin_definition`、`unavailable` 或 `draft`。自定义账号和 CPA 走各自的流程。Zen Free 是单例。计划的验证策略为不需要时，`POST /accounts/{id}/verify` 不探测。CPA 对该探测返回未实现。

`POST /providers/{id}/protocol-probes` 对 `opencode`、`opencode-zen-free`、`command-code`、`minimax` 和 `kimi` 执行。`ollama`、`custom` 和 `cpa` 路由仍在，并返回未实现。Custom 会更早以账号所有为由拒绝（`protocol probes for Custom API are account-owned`）。另一句拒绝是 `protocol probes are not available for this Plan in this slice`。

`POST /providers/{id}/pricing/refresh` 接受 `opencode`、`command-code` 和 `ollama`。其他 id 返回 `provider does not support pricing refresh`。请求体是 `ProviderPricingRefreshUpdate`。`--cas-current` 填写 `expectedRevision` 和 `processGeneration`。`expectedProviderPricingRevision` 仍要从 `GET /providers/{id}/pricing` 取得：`opencode` 用 `pricingRevision`，`command-code` 和 `ollama` 用 `providerPricingRevision`。已经持有定价锁的刷新返回 409 `provider pricing refresh is already running`。那次 POST 保持为已经发出的一次，然后重新读取定价。

`PUT /providers/{id}/pricing/multipliers` 接受 `opencode` 和 `command-code`。请求体是 `PricingMultipliersUpdate`。`expectedPricingRevision` 对 `opencode` 是 `pricingRevision`，对 `command-code` 是 `providerPricingRevision`。其他 id 返回 `provider offering does not support pricing multipliers`。

官方余额和官方价格路由适用于 bearer、`api` 套餐、预设 id 为 `deepseek` 或 `zhipu`、且端点是该预设官方路由的情况。其余返回 `official financial evidence is unavailable for this preset or destination`。

`POST /accounts/{id}/billing/credits/grants` 只发送一次。客户端超时后，先读 `GET /accounts/{id}/billing`，再考虑是否另一次授予。

路由器把转移请求体限制在 4 MiB。导出的 `bundle` 是密文（`ocg-manager-account-backup`）。预览，以及密码错误的导入，都让数据库保持原状。导入才是带 CAS 的写入。

### 浏览器、CPA、BYOK 与 DSH

默认 CLI 构建启用 `dsh-local-host`，`serve` 会注册 BYOK 宿主和 DSH 宿主。找到受支持的 Chromium 可执行文件时，还会注册原生浏览器启动器与停止器，此时 `GET /browser/capabilities` 报告 `mode: native`。用 `POST /accounts/{id}/browser`（`BrowserOpenRequest`）打开原生会话。远程模式是 `OCG_BROWSER_WORKER_URL` 指向的 worker，配合 `OCG_BROWSER_CONTROL_TOKEN_FILE`（默认文件 `/run/ocg-browser/control-token`），且仅在没有注册原生启动器时使用。两种运行时都没有时，浏览器能力报告 `unsupported`。远程会话空闲 30 分钟后断开，最长 4 小时。查看该会话属于后续远程查看器，或使用外部 WebSocket 客户端。`api` 不附着到这个套接字。重置配置文件和打开账号会在浏览器操作锁之后再次检查 CAS。

`GET /cpa/models` 是本地 CPA 目录。`GET /external-integrations/cpa/models` 是集成快照。运行时的启动、停止、回滚、安装和更新作用于这个 `serve` 内部的监督器。OAuth 启动也把会话存在这里。

BYOK 的配置与移除、DSH 的安装与卸载都是 CAS 写入。`codex` 和 `kimi` 要求 `clientClosed: true`。配置还要求先前 GET 得到的 `expectedFingerprint`。已发布目录为空时返回 412 `No models are published by this Key yet; configure a model source first`。未知 `{client}` 返回 `Unknown BYOK application`。宿主会创建或复用名为 `codex`、`kimi-code`、`minimax-code` 或 `zcode` 的普通 Key。

用 `--no-default-features` 构建的二进制仍暴露这些路由。`GET /applications/byok/{client}` 返回 `status: unsupported_runtime`，说明为 `Use a native OCG host on the client computer to configure this application.` `GET /applications/dsh` 返回 `unsupported_runtime`，说明为 `DSH installation is unavailable in this build; use the Desktop app or a native CLI on the DSH host`。POST 和 DELETE 返回 412，正文是 `Native application configuration is unavailable on this host`。DSH 安装返回 412 `DSH installation is unavailable in this build; use the Desktop app or a native CLI on the DSH host`。DSH 卸载返回 412 `DSH uninstallation is unavailable in this build; use the Desktop app or a native CLI on the DSH host`。它们不返回 404。

### 托盘、Dock 与签名更新

此宿主不注册开机自启、Dock 可见性或签名的桌面更新安装器。这些属于延后的界面。`GET /settings` 把支持标志报为 false。带 `autoStart` 的 `PUT /settings` 返回 400 `auto-start is unavailable in this runtime`。`showDockIcon` 返回 400 `Dock visibility is unavailable in this runtime`。`GET /settings/check-update` 仍会检查发行版，并报告 `installSupported: false`。`POST /settings/install-update` 返回 400 `desktop update installation is unavailable in this runtime`，且不推进 epoch。`GET /settings/update-status` 仍是阶段读取。

## 备份

完整备份按[升级、备份、恢复](upgrade-backup.zh-CN.md)操作：停止 `serve`，复制整个数据目录（含 `data.sqlite` 和 `.encryption-key`），恢复时先停止，换上该目录，再用相同或更新的构建启动。没有 `backup` 子命令。

便携加密转移是通过 `api` 的三个文件。导出和预览不加 `--cas-current`。导入要加。密码错误时导入失败，revision 不变。

```bash
ocg-manager-cli --endpoint http://127.0.0.1:9042 api POST /dashboard/api/v4/accounts/transfer/export \
  --input export.json --output export-out.json
ocg-manager-cli --endpoint http://127.0.0.1:9042 api POST /dashboard/api/v4/accounts/transfer/preview \
  --input preview.json --output preview-out.json
ocg-manager-cli --endpoint http://127.0.0.1:9042 api POST /dashboard/api/v4/accounts/transfer/import \
  --input import.json --cas-current --output import-out.json
```

`export.json` 是 `{ "bundlePassword": "synthetic-bundle-password" }`。预览和导入发送 `{ "password": "synthetic-bundle-password", "bundle": "<bundle from export-out.json>" }`。导入的期望字段对由 `--cas-current` 填入。

## 构建

用 Cargo 从工作区构建此二进制。默认特性是 `dsh-local-host`。

```bash
cargo build -p ocg-manager-cli --locked
cargo build -p ocg-manager-cli --locked --no-default-features
```

工作区质量门是带 core 环回特性的锁定测试和 Clippy：

```bash
cargo test --workspace --locked --features ocg-core/ollama-cloud-loopback-test
cargo clippy --workspace --all-targets --locked --features ocg-core/ollama-cloud-loopback-test -- -D warnings
```

独立验收运行器是 Node，不经过 `pnpm`、`package.json` 或 `src-tauri`：

```bash
node scripts/cli-acceptance.mjs
```

标签工作流仍是历史上的桌面发布路径。它不是这套全功能 CLI 的发布。请使用刚刚构建的二进制。

---

[用户指南索引](../USER.zh-CN.md) · [English](cli.md) · [文档索引](../README.zh-CN.md)
