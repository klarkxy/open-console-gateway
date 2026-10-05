[English](cli.md)

# CLI

本页是 Open Console Gateway 的 OCG3 代操作指南。`ocg3` 与 `open-console-gateway` 是同一项目的两代，关系如同 Python 3 与 Python 2。产品名仍是 Open Console Gateway。这一代的命令是 `ocg`（Windows 上为 `ocg.exe`）。Rust 包名是 `ocg-cli`。上一代的命令名不是别名。

`ocg serve` 是提交控制面变更的常驻服务。`api` 是它的 HTTP 客户端。即便同时带了 `--data-dir`，它也不创建、不打开数据库。`schema` 是离线 JSON 帮助：它不连接 `serve`，也不打开数据目录。

`status` 和 `key list` 只在宿主停止时打开所选数据目录，并在初始化前取得同一目录锁。`serve` 运行期间，用 `api` 读取设置、账户记录和网关状态。

`key ping ACCOUNT_ID` 必须使用该数据目录所记录的正在运行的 `serve`。它读取 `cli-listener.json`；该进程仍在时，向记录的监听地址发送 `POST /dashboard/api/v4/accounts/{id}/model-tests`。它不打开数据库，不改选账户，也不自己调用供应商。标记缺失、进程已退出或标记不可读时，命令在解析混淆密钥、打开 SQLite 或发起供应商 I/O 之前拒绝。`--endpoint` 不能把这条命令发到另一个宿主，也不能让已停止的 `serve` 变得可接受。`--model` 默认是 `mimo-v2.5`，并总会作为 `modelId` 发送。省略 `--message` 和 `--max-tokens` 时，保留服务端的最小验证正文。传入 `--message` 或正数 `--max-tokens` 会加上 `message` 或 `maxTokens`。`--max-tokens 0` 会在这次 POST 之前被拒绝。这里说明的是命令要求，不是已完成的在线模型测试验证。

```bash
ocg --data-dir ./ocg-data key ping ACCOUNT_ID
ocg --data-dir ./ocg-data key ping ACCOUNT_ID --message "synthetic check" --max-tokens 16
```

本页是 `serve`、`api`、`schema`、`backup` 的操作约定。若可执行文件的 `--help` 里没有这些命令和参数，那份程序比本页旧。原生桌面 GUI 延后，直到这份 CLI 完成。监听器仍可提供与可执行文件放在一起的既有 `dist/`；该兼容行为不定义新 GUI。

Windows 上的可执行文件是 `ocg.exe`。Linux 与 macOS 解压后执行 `chmod +x ocg`。各平台默认数据目录都是 `~/.ocg3`。`ocg` 不打开、不移动、不删除上一代的 `~/.ocg-mgr-cli`。用 `--data-dir <path>` 指定目录。开发和测试使用合成目录，不用已安装的配置目录。混淆密钥依次为 `--encryption-key`、环境变量 `OCG_MANAGER_ENCRYPTION_KEY`、`<data-dir>/.encryption-key`。该环境变量名不变。Windows 上该文件不存在时，`serve` 退回机器绑定的密钥。优先用密钥文件。写在命令行上的密钥会进 shell 历史。

这一代的 CPA 是 `ocg serve` 拥有的那一个本地运行时。`external-integrations/cpa` 路由保留为拒绝与迁移表面。含义见[自有本地 CPA](#自有本地-cpa)。整目录备份仍使用快照格式标记 `ocg-directory-snapshot`。

## 自有本地 CPA

产品模式是一个由 OCG 拥有的本地 CPA。自定义 HTTP 仍是由该本地 CPA 执行的供应商能力。省略 `target` 时，请求指向自有子进程。已保存的历史远程行不会因此选中 `integration`。该行继续保存。这些路由不会激活它，不会把它转换成本地凭据，也不会删除它。显式迁移是单独的操作，尚未实现。这些路由不打开、不复制、不移动、不删除 `~/.ocg-mgr-cli`。`OCG_CPA_BASE_URL` 不是产品开关：本视图不解析它，安装、回滚和 apply 也不读取它。整份 CLI 验收和这份运行时仍待完成。原生桌面 GUI 继续延后。

`GET /dashboard/api/v4/external-integrations/cpa` 返回 `CpaIntegration`。仅当存在已保存的历史远程 CPA 行时，`legacyMigrationRequired` 为 true。该值已脱敏：响应不复制 URL、密钥、账号 id 或目录。自有健康状态是另一个字段。`runtimeUnavailableReason` 是既有的平台不支持文本、执行记录错误或 `cpa execution is unavailable`，否则为 JSON `null`。迁移语句和任何 `OCG_CPA_BASE_URL` 语句都不是该字段的取值。已保存的行可以在 `runtimeUnavailableReason` 为 null 时把 `legacyMigrationRequired` 设为 true；健康原因也可以在 `legacyMigrationRequired` 为 false 时出现。`CpaRuntime.unavailableReason` 使用同一健康规则。`CpaRuntime` 不携带 `legacyMigrationRequired`。历史数据不会把已安装或正在运行的子进程变成不可用，也不会把缺失的子进程变成已安装或正在运行。

仅当存在自有托管记录，或执行报告表明自有可执行文件已安装时，`configured` 与 `runtimeOwned` 为 true。仅有历史行时两者都为 false。`runtimeRunning` 是 `ExecutionReport.running`。`runtimeSupported` 跟随 `cpa_runtime_supported()`。已安装该制品时，`installedVersion` 取执行报告的 `current_version`，否则取自有托管记录的 `current_version`，再否则为 null。`latestVersion` 是执行报告的 `latest_version`（当前报告为 `v8.0.10`）。`updateAvailable` 与 `currentOperation` 来自该报告。执行报告给出端口时，`baseUrl` 是该自有环回地址；否则在存在托管记录时为 `http://127.0.0.1:{managed.port}`，再否则为空字符串。已保存的远程 URL 不出现在 `baseUrl` 中。`baseUrlReadOnly` 恒为 true。`revision` 与 `processGeneration` 仍是控制令牌。GET 不推进 revision，也不写入历史行、账号、目录或目的地字节。

退役的单例输出仍可解析，并保持空或 false：`managementKeyConfigured` 为 false，`inferenceKeyConfigured` 为 false，`enabled` 为 false，`accountId` 为 null，`modelCount` 为 0，`modelsRefreshedAt` 为 null。历史密文不是自有执行来源。自有启用按原生凭据计算。历史 `CPA_ACCOUNT_ID` 不是自有账号。自有目录按 `destination_models` 中的目的地分开。这个整数不能代表一份规范的自有原生目录，全局 `provider_model_catalogs` 的 CPA 行也不会复制到这里。`destination_models` 没有刷新时间列。

候选的 `CpaControlTarget` schema 枚举是 `owned`。Serde 仍接受 `integration`，处理函数在任何 I/O 之前拒绝它。`CpaIntegrationUpdate` 与 `CpaTestRequest` 在线路上仍接受 `baseUrl`、`managementKey` 和 `inferenceKey`；生成的 schema 预期省略这三项。`CpaIntegrationUpdate.enabled` 仍留在 schema 中。已检入的 schema JSON 是 Rust 示例导出。两份已构建二进制的 `schema v3` 和 `schema v4` 与这些文件一致。本页没有执行导出。`schema v3` 与 `schema v4` 描述的是你运行的那份二进制。字段合约见[自有 CPA 控制](../maintainer/dashboard-api.zh-CN.md#自有-cpa-控制)。

`GET /dashboard/api/v4/routing/explain` 是对自有 CPA 已应用、静态、运行时和额度事实的一次读取。`model` 在去除空白后必填。`clientProtocol` 默认 `chat_completions`。接受的值是 `chat_completions`、`responses`、`messages` 和 `gemini`。公开的 `gemini` 通过 `chat_completions` 解释，响应里的 `clientProtocol` 仍是 `gemini`。`routingMode` 与 `conversationSticky` 只展示已保存的设置。`conversationBinding` 为 `not_evaluated`。任何路由模式下 `expectedBasePolicyFirstPick` 都是 null。已知目录行即使 `routeable` 为 false，也是一次成功的解释。期望路由不进入合格列表。合格行仍可带 `callerPending`、`secretRecheckPending` 和 `sendPending`。`RoutingResolvedMapping` 上的线路字段 `migrationRequired` 已经实现，包括只有目录的历史远程行。省略该字段的旧载荷为 false，新响应会发出该字段。该字段已在检入的 schema 导出中。这次读取的 Cargo 断言运行和普通 CLI 验收仍待完成。合格判断使用同一目的地、同一供应商和实际上游上已选中的可路由映射，以及该次读取捕获的设置。该捕获的源码已经存在，其验收仍待完成。这次读取不解密、不放行、不写入。线路合约见[自有 CPA 路由解释](../maintainer/dashboard-api.zh-CN.md#自有-cpa-路由解释)。

## 命令

启动宿主并让它一直运行。`--port` 写入 SQLite；之后不带该参数的 `serve` 会沿用这个端口。默认绑定 `127.0.0.1`。该绑定上没有 `Origin`、也没有转发头时，环回调用者已经是本地管理员。注册之前 `initialized` 为 false。

```bash
ocg --data-dir ./ocg-data serve --port 9042
```

用 Ctrl+C 停止。该进程拥有本地 CPA 子进程：启动是恢复路径，退出是关闭路径。已审阅的制品锁已经存在。本页不记录一个正在运行的子进程，整份 CLI 验收仍待完成。OAuth 会话、浏览器会话、更新阶段和 settings epoch 都在这个进程里。另一个进程打开同一目录并不能代替它。

在另一个终端指定监听地址：

```bash
ocg --endpoint http://127.0.0.1:9042 api METHOD PATH \
  --input request.json \
  --output response.json \
  --cas-current \
  --session-file session.json
```

`--input -` 从 stdin 读一份 JSON。GET 可以不带 `--input`。`--output` 可选，它是响应的私有副本。`--cas-current` 和 `--session-file` 可选；何时使用见下文。

推理走同一个 `api`，外加一份 bearer 文件。文件里是网关 Key，一行，不放进进程参数。

```bash
ocg --endpoint http://127.0.0.1:9042 api POST /v1/chat/completions \
  --input chat.json \
  --key-file gateway-key.txt \
  --output chat-out.json
```

`schema` 不连接 `serve`，也不打开数据目录。它把离线 JSON schema 写到 stdout。

```bash
ocg schema v4
ocg schema v3
```

能力表里的名字是该输出中的 `$defs`。只属于 V4 的请求体在 `schema v4`。由重新挂载的 V3 处理函数拥有的请求体在 `schema v3`。

`api` 接受 `/dashboard/api/v4/...`、下面六条推理路径，以及保留的认证路径 `/dashboard/api/auth/status`、`/dashboard/api/auth/register`、`/dashboard/api/auth/login` 和 `/dashboard/api/auth/logout`。绝对 URL、协议相对路径、`/dashboard/api/v3` 路径、用 `..` 爬出前缀的路径，以及保留的浏览器套接字 `/dashboard/api/browser/sessions/{token}/ws` 会被拒绝。这些拒绝以非零退出，且不发送请求。认证优先走 v4 路径；保留副本共用同一会话。

`api` 发送普通 HTTP 请求并读取响应。它不升级 WebSocket，因此没有 `api GET .../ws` 命令。`/dashboard/api/v4/browser/sessions/{token}/ws` 也一样：V4 前缀并不是套接字客户端。服务器仍挂载该会话套接字。远程查看器是后续工作，也可以把外部 WebSocket 客户端指到服务器。可用的原生浏览器流程是 `GET /dashboard/api/v4/browser/capabilities` 和 `POST /dashboard/api/v4/accounts/{id}/browser`。

## 从空目录到第一次推理

下面的占位符是合成值。形状与 schema 以及 `scripts/cli-acceptance.mjs` 一致。它们不是真实供应商。

新建的环回宿主上，`status.json` 里 `local` 为 true，`authenticated` 为 true，`initialized` 为 false，并带有 `revision` 与 `processGeneration`。`--cas-current` 读的就是这份公开响应。其中没有密码、cookie 或 Key。`GET /dashboard/api/v4/contract` 需要会话；在非本地绑定上，用它为尚未注册的宿主取期望值是错误的。

```bash
ocg --endpoint http://127.0.0.1:9042 api GET /dashboard/api/v4/auth/status --output status.json
ocg --endpoint http://127.0.0.1:9042 api GET /dashboard/api/v4/templates --output templates.json
```

`register.json` 是省略两个期望字段的 `AuthRegister`，由 `--cas-current` 填入：

```json
{ "username": "local-admin", "password": "synthetic-admin-password" }
```

```bash
ocg --endpoint http://127.0.0.1:9042 api POST /dashboard/api/v4/auth/register \
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
ocg --endpoint http://127.0.0.1:9042 api POST /dashboard/api/v4/onboarding/commit \
  --input onboarding.json --cas-current --output onboard.json
```

用 `GET /dashboard/api/v4/accounts/{id}` 读回账号，或在 `GET /dashboard/api/v4/account-records` 里找那一行。这两个列表是不同的读取。若提交后别名尚未发布：

```json
{ "publicModel": "example-model", "published": true }
```

```bash
ocg --endpoint http://127.0.0.1:9042 api PATCH /dashboard/api/v4/alias-publication \
  --input publish.json --cas-current --output publish-out.json
```

`GET /dashboard/api/v4/connection` 返回 `ConnectionInfo`，其中含 `primaryKey`。用 `--output connection.json` 写入，再自行把 `primaryKey` 抄进 `gateway-key.txt`。不带 `--output` 时，stdout 会略去这把 Key。

```bash
ocg --endpoint http://127.0.0.1:9042 api GET /dashboard/api/v4/connection --output connection.json
ocg --endpoint http://127.0.0.1:9042 api GET /v1/models --key-file gateway-key.txt --output models.json
```

`chat.json`：

```json
{ "model": "example-model", "messages": [{ "role": "user", "content": "ping" }] }
```

```bash
ocg --endpoint http://127.0.0.1:9042 api POST /v1/chat/completions \
  --input chat.json --key-file gateway-key.txt --output chat-out.json
```

其余推理路径是 `POST /v1/responses`、`POST /v1/messages`、`POST /v1beta/models/{model}:generateContent` 和 `POST /v1/models/{model}:generateContent`，都要带 `--key-file`。带 `"stream": true` 的 chat completion 按服务器发送事件原样拷贝，客户端不会把它缓冲成另一份 JSON。`POST /v1/responses` 且 `"store": true` 时，请求在转发到上游之前失败。

## 期望值

`--cas-current` 是一次显式注入。它从 `GET /dashboard/api/v4/auth/status` 读取 `revision` 和 `processGeneration`，仅在请求体缺少对应键时写入 `expectedRevision` 和 `processGeneration`。文件里已经写了的值会照原样发送，包括过期值。该参数不填写 `expectedPricingRevision` 或 `expectedProviderPricingRevision`。

需要这对字段的变更，如果文件和参数都没提供，就会失败。只带 `{ "conversationSticky": false }` 的 `PUT /dashboard/api/v4/settings` 就是这种失败。设置文档保持原样。

读取、修改、再写入：

```bash
ocg --endpoint http://127.0.0.1:9042 api GET /dashboard/api/v4/accounts/ACCOUNT_ID --output account.json
```

`rename.json` 是 `AccountUpdate`。省略期望字段时，`--cas-current` 填入当前这一对：

```json
{ "name": "example-renamed" }
```

```bash
ocg --endpoint http://127.0.0.1:9042 api PATCH /dashboard/api/v4/accounts/ACCOUNT_ID \
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
ocg --endpoint http://127.0.0.1:9042 api POST /dashboard/api/v4/external-integrations/cpa/oauth/start \
  --input oauth.json --cas-current --output oauth-start.json
ocg --endpoint http://127.0.0.1:9042 api GET /dashboard/api/v4/external-integrations/cpa/oauth/status \
  --output oauth-status.json
```

上面的 OAuth 一对请求省略了 `target`，因此指向自有子进程。历史远程行不会选中远程 OAuth 目标。本页不记录一次已完成的供应商 OAuth。同一模式还有：`POST /accounts/{id}/usage/refresh`（`UsageRefreshUpdate`）之后 `GET /accounts/{id}/usage`；运行时安装、启动、停止或回滚之后读 `GET /external-integrations/cpa/runtime` 和 `GET /external-integrations/cpa/runtime/logs`。`GET /settings/update-status` 是更新阶段的读取。在这个无头宿主上，安装用的 POST 不会启动安装器；见更新器各行。

改端口是一次设置写入，然后换一个 endpoint。`SettingsUpdate` 可以只包含你要改的字段：

```json
{ "gatewayPort": 9043 }
```

```bash
ocg --endpoint http://127.0.0.1:9042 api PUT /dashboard/api/v4/settings \
  --input port.json --cas-current --output port-out.json
ocg --endpoint http://127.0.0.1:9043 api GET /dashboard/api/v4/settings --output settings.json
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
| 路由 | `GET /dashboard/api/v4/routing/explain` | v4 `RoutingExplanation`；自有 CPA 已应用事实 |
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
| 全量数据备份 | `backup create --output FILE`、`backup restore --input FILE` | 离线整目录快照；恢复到新目录或空目录。 |
| 日志 | `GET /dashboard/api/v4/logs/gateway`、`/logs/forward`、`/logs/forward/models`、`/logs/forward/keys` | 已脱敏的处理函数响应 |
| 日志 | `GET /dashboard/api/v4/gateway/status`、`/dashboard/summary`、`/dashboard/daily-tokens-by-model` | 需要会话的读取 |
| 浏览器 | `GET /dashboard/api/v4/browser/capabilities` | `mode` 为 `native`、`remote` 或 `unsupported` |
| 浏览器 | `POST /dashboard/api/v4/accounts/{id}/browser` | v3 `BrowserOpenRequest` |
| 浏览器 | `DELETE /dashboard/api/v4/accounts/{id}/browser-profile` | v3 `MutationExpectation` |
| 浏览器套接字 | 不是 `api` 调用 | 服务器仍挂载。保留路径不在 CLI 允许列表中。 |
| CPA | `GET`、`PUT` 或 `DELETE /dashboard/api/v4/external-integrations/cpa` | v3 `CpaIntegration`；`PUT` 与 `DELETE` 需要 CAS；远程 URL 与密钥字段会被拒绝 |
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

`GET /cpa/models` 读取共享的供应商目录槽（`provider_id` 为 `cpa`）。`GET /external-integrations/cpa/models` 是保留的集成快照。自有原生目录按目的地分开。单例 `modelCount` 保持为 0，并且不复制这两份目录。运行时的启动、停止、回滚、安装和更新作用于这个 `serve` 内部、面向自有子进程的监督器。已审阅的制品锁已经存在。本页不记录一个正在运行的子进程，整份 CLI 验收仍待完成。OAuth 启动把会话存在这个进程里。本页不记录一次已完成的供应商 OAuth。仍携带 `baseUrl`、`managementKey` 或 `inferenceKey` 的请求在任何 I/O 之前被拒绝。显式 `target` 为 `owned` 且带上其中任一字段时，返回 `owned CPA control does not accept a remote base URL or key`。显式 `target` 为 `integration`，或省略 `target` 却带上其中任一字段时，若存在历史行则返回 `Stored remote CPA configuration requires explicit migration and was left unchanged`，若不存在则返回 `Remote CPA is not a target in this product`。`DELETE` 使用同样的语句，并让已保存的字节保持原样。只带 `enabled` 的 `PUT` 返回自有视图，不改写历史行和账号，响应里的 `enabled` 为 false。OAuth 状态查询和 CPA target 查询上的 `target=integration` 仍能反序列化，并在任何 I/O 之前以同样方式拒绝。

BYOK 的配置与移除、DSH 的安装与卸载都是 CAS 写入。`codex` 和 `kimi` 要求 `clientClosed: true`。配置还要求先前 GET 得到的 `expectedFingerprint`。已发布目录为空时返回 412 `No models are published by this Key yet; configure a model source first`。未知 `{client}` 返回 `Unknown BYOK application`。宿主会创建或复用名为 `codex`、`kimi-code`、`minimax-code` 或 `zcode` 的普通 Key。

用 `--no-default-features` 构建的二进制仍暴露这些路由。`GET /applications/byok/{client}` 返回 `status: unsupported_runtime`，说明为 `Use a native OCG host on the client computer to configure this application.` `GET /applications/dsh` 返回 `unsupported_runtime`，说明为 `DSH installation is unavailable in this build; use the Desktop app or a native CLI on the DSH host`。POST 和 DELETE 返回 412，正文是 `Native application configuration is unavailable on this host`。DSH 安装返回 412 `DSH installation is unavailable in this build; use the Desktop app or a native CLI on the DSH host`。DSH 卸载返回 412 `DSH uninstallation is unavailable in this build; use the Desktop app or a native CLI on the DSH host`。它们不返回 404。

### 托盘、Dock 与签名更新

此宿主不注册开机自启、Dock 可见性或签名的桌面更新安装器。这些属于延后的界面。`GET /settings` 把支持标志报为 false。带 `autoStart` 的 `PUT /settings` 返回 400 `auto-start is unavailable in this runtime`。`showDockIcon` 返回 400 `Dock visibility is unavailable in this runtime`。`GET /settings/check-update` 仍会检查发行版，并报告 `installSupported: false`。`POST /settings/install-update` 返回 400 `desktop update installation is unavailable in this runtime`，且不推进 epoch。`GET /settings/update-status` 仍是阶段读取。

## 备份

停止 `serve` 后创建整目录快照。输出必须是源目录之外的新文件。恢复时使用另一个新目录或空目录：

```bash
ocg --data-dir ./ocg-data backup create --output ./ocg-snapshot.tar.gz
ocg --data-dir ./restored-ocg-data backup restore --input ./ocg-snapshot.tar.gz
```

快照包含 SQLite、加密身份、CPA 托管的认证与运行时配置文件，以及其他持久数据。宿主或数据库锁被占用时立即拒绝。恢复先验证格式、schema、文件哈希和加密身份，再发布目录；它会重定位托管 CPA 的认证目录，排除活动监听器与锁标记，并且不启动进程。已有输出文件和非空目标目录都会保留，没有替换模式。

加密选择依次为显式密钥、环境变量密钥、密钥文件、Windows 机器密钥。即便存在旧 `.encryption-key`，显式或环境变量覆盖仍优先。恢复需要相同的有效外部密钥或 Windows 机器身份；使用文件密钥的快照会携带该文件。旧的未认证密文会保留，并标为尚未验证的历史材料：仅能读出文本，不能证明其原始密钥正确。

限制为展开后总计 4 GiB、单文件 2 GiB、每份托管 CPA 配置 1 MiB。成功输出一条包含路径、格式、版本、数量和状态的 JSON 回执；失败返回非零退出码。快照应与兼容程序一起保留。历史安装及目录复制流程见[升级、备份、恢复](upgrade-backup.zh-CN.md)。

便携加密转移是通过 `api` 的三个文件。导出和预览不加 `--cas-current`。导入要加。密码错误时导入失败，revision 不变。

```bash
ocg --endpoint http://127.0.0.1:9042 api POST /dashboard/api/v4/accounts/transfer/export \
  --input export.json --output export-out.json
ocg --endpoint http://127.0.0.1:9042 api POST /dashboard/api/v4/accounts/transfer/preview \
  --input preview.json --output preview-out.json
ocg --endpoint http://127.0.0.1:9042 api POST /dashboard/api/v4/accounts/transfer/import \
  --input import.json --cas-current --output import-out.json
```

`export.json` 是 `{ "bundlePassword": "synthetic-bundle-password" }`。预览和导入发送 `{ "password": "synthetic-bundle-password", "bundle": "<bundle from export-out.json>" }`。导入的期望字段对由 `--cas-current` 填入。

## 构建

用 Cargo 从工作区构建此二进制。默认特性是 `dsh-local-host`。

```bash
cargo build -p ocg-cli --locked
cargo build -p ocg-cli --locked --no-default-features
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

可选的 CPA 验收程序、宿主变体对齐和证据等级见[开发说明](../maintainer/development.zh-CN.md#cpa-验收程序)。本页不记录该程序的运行结果。整份 CLI 验收仍待完成。

标签工作流仍是历史上的桌面发布路径。它不是这套全功能 CLI 的发布。请使用刚刚构建的二进制。

---

[用户指南索引](../USER.zh-CN.md) · [English](cli.md) · [文档索引](../README.zh-CN.md)
