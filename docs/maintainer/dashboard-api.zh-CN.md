[English](dashboard-api.md)

# Dashboard API

> 适用范围：已发布的面板操作参考，加上[自有 CPA 控制](#自有-cpa-控制)与[自有 CPA 路由解释](#自有-cpa-路由解释)中的当前候选合约。这些合约已在检入的 schema 导出中。整份 CLI 验收与运行时验收仍待完成。原生桌面 GUI 继续延后。界面步骤不适用于这个无头 CLI 阶段。代际设计见[架构](../architecture.zh-CN.md)。

## 计费与本地积分估算（V4）

`GET /dashboard/api/v4/accounts/{id}/billing` 统一返回周期额度、现金或积分，以及数据来源和可用操作。这里的 `id` 对应一个账号（一条 Key）；同一供应商容器内的多个账号仍独立计量。读取不请求上游。已有官方余额和额度刷新接口保留各供应商的观测适配器。

`PUT .../billing/credits` 配置个人积分估算。首次设置填写当前额度，之后修改价格或设置时保留余额。`POST .../billing/credits/calibrate` 校准各笔当前余额，`POST .../billing/credits/grants` 添加额度或加油包，`DELETE .../billing/credits` 停用此估算。修改要求 `expectedRevision` 和 `processGeneration`，并返回更新后的 `BillingStatus`。校准之后开始的请求从新基准扣减；界面保留在途和无法计价请求的提示。估算耗尽不会改变路由资格。

Step Plan 在官方用量 API 开放前使用上述本地估算与校准方式。原先的私有控制台令牌接口和 `StepFunUsageStatus` 已退役。StepFun 普通 API 余额与 `/step_plan` 通道保持独立。

个人积分仅支持 legacy kind 为 `custom_account` 或 `dynamic` 的 `http` 目的地。平台关联 Key、观察凭据和内置 Plan 保留既有计费合约。存在在途请求时拒绝积分校准，不在请求进行中移动基准。

## Dashboard V3

`/dashboard/api/v3` HTTP 挂载**已移除**。面板只走 V4。匿名访问 `/dashboard/api/v3` 与 `/dashboard/api/v3/*` 返回空 body 的 **401**（鉴权先于墓碑）。已鉴权（含回环本地模式）返回 **410** `{ "code": "dashboardV3Removed", "message": "Dashboard API V3 has been removed; refresh the page and retry." }`。

原先的 V3 操作处理器已挂到 `/dashboard/api/v4`，相对路径不变；唯一例外是账号列表垫片改为 `GET /account-records`，以免与 V4 `GET /accounts`（身份列表）冲突。`GET /contract` 使用现有 V4 ControlRevision。**挂回去的处理器仍是兼容垫片**，读目的地与凭据表。新客户端应使用 V4 `GET /destinations` 与 `GET /credentials`。挂载后的列表从 destinations 与 credentials 重建。Key 密文只留在凭据 SQL 行，不会出现在 V4 GET 目的地/凭据 DTO 上。DTO 使用 camelCase，变更体 `deny_unknown_fields`，可空响应字段始终序列化为 `T | null`。

控制面身份：

- `settings_revision` — `CoreState` 上的内存 `AtomicU64`，成功持久化后 bump。 CAS 令牌本身不存 SQLite。
- `process_generation` — 每个 `CoreState` 赋值一次，不会持久化。上一进程的 CAS 令牌在重启后不能复用。
- `pricingRevision` — 不可变快照 id。价格变更还要带 `expectedPricingRevision`。

`GET /contract` 返回当前进程的 live revision / generation token（`ControlRevision`：`revision`、`processGeneration`、`pricingRevision`）。

变更要求顶层 `expectedRevision` 与 `processGeneration`，包括 `/auth/register`、`/auth/login`、`/auth/logout` 以及 `POST /accounts/{id}/usage/refresh`。缺少 `expectedRevision` 返回 `400` `missingExpectedRevision`；不匹配返回 `409` `revisionConflict`，错误信封携带 `currentRevision` / `processGeneration`。Vue `controlPlane` store 从每个挂回 V4 的 V3 载荷记录两个令牌。遇到 409 时，客户端会刷新控制令牌与受影响资源，但不会自动重放变更；用户确认当前状态后可再次提交。revision 与 generation 令牌只属于当前进程，不协调共用同一数据目录的多个进程。

非变更操作跳过 CAS 且不 bump revision：诊断类如 `POST /settings/test-proxy`、`POST /custom/models/discover`；更新检查如 `GET /settings/check-update`、`GET /settings/update-status` 捕获令牌但不 bump。`POST /settings/install-update` 需要 CAS，原子启动，不 bump，不持有网络/DB 锁。

明文 Key 不会出现在 `Settings`、供应商、Zen 或合约 DTO 上。`ConnectionInfo`（`GET /connection`）是唯一携带密钥的 V3 响应：返回主 Key 与所有未软删的子 Key 值，包括禁用子 Key，受 dashboard 会话保护。只有启用的 Key 会进入鉴权快照。`CustomModelDiscoveryRequest.apiKey` 只写。账号 list/get 载荷保持无密钥。日志与错误信封脱敏已知密钥。

冻结契约是 `schema/dashboard-api-v3.schema.json`，由 `dashboard_v3::contract_schema_pretty()` 经 `crates/ocg-core/examples/export_dashboard_v3_schema.rs` 生成。生成的 TypeScript（`src/api/generated/dashboard-v3.ts`）只有类型，没有 HTTP 封装。`dashboard_v3/types.rs` 的 `CATALOG_TYPE_NAMES` 是有序 `$defs` 目录；追加时必须保持既有 definition 对象字节一致。

前端：`src/api/dashboard.ts` 展示客户端封装 `dashboardV3`，为每个页面和 store 投影所需字段。

`dashboard.rs` 提供 SPA 并保留 V2 鉴权与浏览器 WebSocket 处理器。保留家族之外的 `/dashboard/api/...` REST 路径在到达 `dashboard.rs` 之前由 `host_router` 墓碑拦截。

## Dashboard V4

面板 JSON 位于 `/dashboard/api/v4`。这是唯一存活的面板 JSON 前缀：增量 V4 路由加上挂回的 V3 操作处理器。已检入的 schema JSON 是 `export_dashboard_v3_schema` 与 `export_dashboard_v4_schema` 的标准输出。下面 CPA 小节中的字段，包括 `migrationRequired`，已在该 JSON 中。本页没有运行导出。

V4 复用 V3 会话中间件。其列表返回与 V3 CAS 相同的 `ControlRevision`（`expectedRevision` / `processGeneration`）。V4 变更是 `POST /onboarding/commit`、`POST /credentials/{id}/rotate`、`POST /credentials/{id}/quota-retry`、`PATCH /bindings/{id}`、`POST /identities/{id}/credentials`、`POST|DELETE /applications/dsh`（同时绑定 GET 检查指纹；DELETE 不带 `keyId`）、`PUT /destinations/{id}/catalog`、`POST /destinations/{id}/catalog/refresh`、`POST /destinations/{id}/model-tests`、`POST /platform-accounts/{id}/import-keys`、`PUT /cpa/models`、`POST /provider-contracts/{scope_kind}/{scope_id}/catalog/remove` 与 `PATCH /alias-publication`，它们检查这两枚令牌；只读路由不检查。

已检入的仅增量 V4 契约是 `schema/dashboard-api-v4.schema.json`，由 `dashboard_v4::contract_schema_pretty()` 经 `crates/ocg-core/examples/export_dashboard_v4_schema.rs` 生成。生成的 TypeScript（`src/api/generated/dashboard-v4.ts`）只有类型，没有 HTTP 封装。`dashboard_v4/types.rs` 的 `CATALOG_TYPE_NAMES` 同样是有序 `$defs` 目录；追加时必须保持既有 definition 对象字节一致。

只读路由为 `GET /contract`、`GET /templates`、`GET /connections`、`GET /accounts`（身份列表）、`GET /account-records`（挂回的 V3 账号列表垫片）、`GET /destinations`、`GET /credentials`、`GET /accounts/{id}/billing`、`GET /accounts/{id}/official-api`、`GET /providers/{id}/official-api/pricing`、`GET /routing/cards`、`GET /applications/dsh`（可选 `profilePath` 与 `runtimeUrl`）、`GET /cpa/models` 与 `GET /alias-publication`。这些读取不会发出出站请求。

official-api 族——`GET /accounts/{id}/official-api`、`POST /accounts/{id}/official-api/balance` 与 `GET|POST /providers/{id}/official-api/pricing`——暴露官网 API 预设的账务依据。GET 是本地投影；带 CAS 的 POST 是唯一的联网路径。详见[官网 API 实现](../../crates/ocg-core/src/official_api.rs)。

`GET /templates` 是只读的添加目录：密封内置项（不含 CPA）加上 `custom-http` 手动模板。预设不属于该模板目录。模板没有用户实例或密钥。

`GET /connections` 是已保存实例的投影：已有账号的内置项，以及每一条可配置 HTTP 连接；每个持久化 Custom API 目的地只出现一次，并汇总引用它的全部 Key。CPA 永远不是 connection。每条 connection 携带生命周期、授权状态、带原因的本地资格、endpoints、模型目标，以及一份遗留身份引用。connection id 由该遗留身份派生，不由名称或 URL 派生。

`GET /accounts` 返回 `IdentityList { revision, identities[] }`。每条 `IdentitySummary` 携带 `identity`（`id`、`label`、`authorityRef` `{ issuerOrSite, tenantOrSubject }`、`identityConfidence`、`enabled`、`notes`）、`credentials[]`、身份级 `declaredRelations[]`（`platformAccountId`、`group`）以及 `legacy`（`kind` `account` | `platform_account`，`id`）。载荷形状是嵌套的：`credentials[].credential`（`id`、`purpose` `inference` | `platform_observer`、`materialKind` `api_key` | `external_reference`、`secretRef`——不透明句柄，绝不是材料本身、`hasMaterial`、`version`、`enabled`、`authState` `unknown` | `valid` | `invalid`、`authStateVersion`、未知时 `expiresAt` 为 null），同级字段为 `subject`（`account_credential` | `anonymous`）、`bindings[]`（`id`、`connectionId`、`allowedEndpointIds`、`allowedOrigins`、`modelScope`、`enabled`、`routingRank`）、`quotaWindows[]`、`onboardingTask`、`subscription`（未知时为 null）、`lastError`（已脱敏；无法安全脱敏时为 null）与 `legacy`。平台父账号的 `platform_observer` 凭据是投影，没有 `credential_state` 行。`authState` 是本地状态：`unknown` 绝不是 `valid`；`valid` 需要既有验证记录。Vue 账号页只把该投影叠加到展示上；Key 轮换、额度重试、绑定编辑与身份内新增凭据走 V4 原生路由，其余账号变更走挂回 V4 的原 V3 路径。

`GET /destinations` 与 `GET /credentials` 是不含密钥、带 revision、只读本机的投影。`DestinationCredentialDto` 可带可选可空的 `quotaRecovery`（camelCase）。缺省表示没有已确认耗尽，不是已验证的上游健康。该对象上的 `status` 只用于展示（`waiting` | `ready` | `probing`）。`IdentitySummary` 的凭据不带该字段。带 CAS 的 `PATCH /destinations/{id}` 完整替换可编辑 HTTP 目的地的名称、地址、鉴权、协议、映射与按模型路由覆盖；它不接收 Key，只给 `authorizeCredentialIds` 明确列出的凭据并入安全授权。`DELETE /destinations/{id}` 要求没有凭据引用。密封与平台管理目的地拒绝两种变更。空库或遗留表升级窗口回落到 `project()`；拒绝时返回结构化 `409`。

节点转移（`POST /accounts/transfer/export|preview|import`）挂在 V4。最新导出使用当前迁移 payload，以 `destinations` 与 `credentials` 为权威，并携带按模型路由覆盖、模型解析策略、`quotaPools` 与 `node`，以及显式 HTTP 协议路由。支持的导入范围、逐版本默认值与显式路由拒绝规则见当前[账号转移实现](../../crates/ocg-core/src/dashboard_v3/account_transfer.rs)。本机额度恢复不是可迁移字段：导出省略；目标 Key 未改则保留；替换 Key 则清除。

`GET /routing/cards` 返回带同一 revision 的 `cards`、`destinations` 与 `credentials` 快照。`PUT /routing/cards` 接收 CAS 令牌和完整有序卡片列表。每张卡包含 `id`、`destinationId` 与有序 `credentialIds`；包括禁用行在内，每份推理凭据必须在原目的地下恰好出现一次，观察者凭据不参与。布局和展开后的路由顺序一起提交，响应返回完整快照。多张卡共用同一目的地；新增或移除额外空卡不会新建或删除供应商。

`GET /routing/explain?model=...&clientProtocol=...` 是现有的已鉴权读取。当前候选合约是[自有 CPA 路由解释](#自有-cpa-路由解释)。已检入的 schema JSON 包含这次读取，包括 `migrationRequired`。

V4 不把授权 `unknown` 当作 `valid`。资格是本地投影，不是上游健康。

`POST /onboarding/commit` 请求体：`expectedRevision`、`processGeneration`（与 V3 相同的 CAS 令牌）、`operationId`（客户端生成的 UUID）、`connection`、可选 `authorization`，以及 `targets`。

`connection` 为 `kind: new`（`templateId` 是 `custom-http` 或预设 id，外加 `name`、`endpointUrl`、`upstreamProtocol`、`authKind`）或 `kind: existing`（`connectionId`）。`authorization` 为 `kind: api_key`（`secretInput`，可选 `accountLabel` / `notes`）或 `kind: none`。`targets` 把公开模型映射到精确上游模型，可带每条目的上游覆盖。`new` 要求 `targets` 非空；`existing` 必须为空（连接编辑走 V4 `PATCH /destinations/{id}`）。

求值顺序：(1) 解析；(2) `operationId` 必须是 UUID；(3) 先取 `settings_update` 锁，再在 CAS 之前做幂等查找——若该 `operationId` 已用同一载荷摘要提交过，则直接返回已存的无密钥结果，并带 `replayed: true` 与当前 revision 令牌，不再检查 CAS（首次写入已经推进 revision）；同一 `operationId` 配不同载荷返回 `409` `operationPayloadMismatch`，不写入；(4) CAS 检查（`409` `revisionConflict`）；(5) 写入。

`new` 复用 V3 用户定义供应商校验。模板 id 作为不透明预设 id 透传；预设表单归前端所有，Rust 只消费由 `resources/provider-presets.json` 生成的 offering 投影。keyed 鉴权下省略 `authorization` 只保存定义（随后 V4 connection 的授权为 `missing`）；`api_key` 在 keyed 鉴权下要求非空密钥；`none` 仅对无鉴权模板有效，且总会创建单例账号。供应商行、可选的首个账号行与操作记录在同一 SQLite 事务中提交；提交后按 V3 同样方式安装动态供应商快照。

`existing` 接受 keyed dynamic Provider 与遗留 Custom HTTP connection 新增 `api_key`。内置、平台管理与无鉴权 connection 返回 `400`。账号行与操作记录在同一事务中提交，随后推进 revision。

结果为 `{ revision, connectionId, credentialId | null, targetIds, replayed }`。`connectionId` 是动态供应商的确定性 UUIDv5；`credentialId` 是账号 id；`targetIds` 是每个公开模型的 UUIDv5。响应从不包含密钥、密文或摘要。

**幂等操作。** `operationId` 与载荷摘要绑定一次提交：摘要是只对语义载荷——`operationId`、`connection`、`authorization`（因此覆盖密钥）与 `targets`——计算的 hex HMAC-SHA256；`expectedRevision` / `processGeneration` 不参与，所以刷新 CAS 令牌后的重试仍会重放。每次提交存储在 `dashboard_operations`；已存的 `result_json` 不含密钥。插入时会清理超过 30 天的行；被清理后，同一 `operationId` 视为新写入。

`POST /credentials/{id}/rotate` 替换一条投影凭据上的 Key。必须带 CAS 令牌，没有 `operationId`。凭据 id、绑定与配额关系保持不变。`version` 与 `authStateVersion` 一起递增；`authState` 变为 `unknown`；底层账号的 `auth_error` / `last_error` 与验证结果会被清空，避免旧版本污染新 Key。轮换替换 Key 并清除本机额度恢复。请求体是 `{ secretInput }` 加上 CAS 令牌。结果不含密钥。平台观察者、匿名、无鉴权与 CPA 凭据返回 `400`。未知 id 返回 `404`。过期 CAS 令牌返回 `409` 且不写入。

`QuotaRecoveryDto` 为 `{ status: "waiting" | "ready" | "probing", reason: "quota_exhausted" | "insufficient_balance", window: "five_hours" | "week" | "month" | "unknown", observedAt: string（RFC3339）, resetsAt: string | null, nextRetryAt: string（RFC3339）, failureCount: number }`。

`POST /credentials/{id}/quota-retry` 使用既有扁平 `MutationExpectation` 请求体（`expectedRevision`、`processGeneration`），没有 `operationId`。结果为 `{ revision: ControlRevision, credential: DestinationCredentialDto }`，不含密钥。它只允许下一次正常选择：无出站请求、不改启用、不清除退避。状态已是 `ready` 或 `probing` 时幂等，可返回当前更新后的行。未知 id 返回 `404`。过期 CAS 令牌返回 `409` 且不写入。

`PATCH /bindings/{id}` 编辑一条推理绑定。必须带 CAS 令牌，没有 `operationId`。请求体是 `{ modelScope?, enabled? }` 加上 CAS 令牌，至少要有其中一个字段。`modelScope` 为 `{ kind: "all" }` 或 `{ kind: "only", models: [...] }`（精确 id，沿用既有模型名归一化）。同一身份上各绑定的启停彼此独立。未知 id 返回 `404`。平台观察者、匿名、无鉴权与 CPA 绑定返回 `400`。过期 CAS 令牌返回 `409` 且不写入。结果为 `{ revision, binding }`，不含密钥。

`POST /identities/{id}/credentials` 给已确认身份再加一把 Key。必须带 CAS 令牌，没有 `operationId`。请求体是 `{ connectionId, secretInput }` 加上 CAS 令牌。写入会新建一行 `accounts` 并复用既有 `identity_id`，插入 `credential_state` 与 `credential_bindings`，并把新账号加入该身份的额度池，全部落在同一 SQLite 事务中。不同的 `connectionId` 是第二件产品（D05）；同一 Plan connection 则是该产品上的另一把 Key。换 Key 不会另起一个新池。未知身份或 connection 返回 `404`。内置不可变 / Zen Free / CPA / 无鉴权 / Custom API / 平台观察者目标返回 `400`。过期 CAS 令牌返回 `409` 且不写入。结果不含密钥。

面板用 `GET /connections` 渲染供应商页 rail，用 `POST /onboarding/commit` 创建用户定义供应商，并用 `GET /accounts` 作为账号页的展示叠加。客户端在草稿改动时生成新的 `operationId`，对未改动草稿的重试沿用同一 id，成功后再重新生成。编辑与删除账号以及账号页上的其余账号操作仍走 V3；Key 轮换、额度重试、绑定编辑与给既有身份新增 Key 使用 V4。

## Settings 变更流程



这条流程只描述受 CAS 保护的 Settings 写入；发现、诊断和读取操作可能按上文所述跳过 CAS。客户端提交 `expectedRevision` 与 `processGeneration`。令牌不匹配时返回 `409`；客户端刷新令牌与受影响资源，但不会自动重放写入。

CAS 成功后，Host 先持久化新设置并释放设置锁。只有端口发生变化且监听器正在运行时才会重绑。若重绑失败，请求以 `internal` 代码返回 `500`。补偿逻辑仅在实时配置仍等于本次失败写入的端口时恢复旧端口，避免覆盖随后成功的写入。

成功的 V4 投影保存会在拿到已保存回执之后调用既有的 `note_product_apply`。目的地 PATCH 在外层提交结果之后等待这一次调用。绑定 PATCH、凭据轮换、目的地删除、刷新和内置项移除只在同步保存成功并且设置锁已释放之后调用它。身份创建和引导重放仅在该操作不是重放时调用它。对绑定而言，`patch_locked` 保存该行并推进设置 revision；处理函数随后调用 `note_product_apply`，并且不调用 `schedule_owned_apply`。模型测试保留一次探测前的 `schedule_owned_apply`，没有产品保存钩子。额度重试和读取路由没有投影钩子。`commit_configuration_update` 在数据库事务打开期间执行变更、路由调和、导入运行时准备和临时策略编译，然后提交，再安装导入的运行时和同一份已编译快照。编译不替换实时快照。编译失败时事务保持未提交。`publish_temporary_policy` 仍在该事务之外编译并安装，且这次提交之后不会使用它。应用失败时，`note_product_apply` 保留成功的保存回执。一次性的回执失败分支只存在于测试中。这一边界的源码已经存在。同级测试已检视，本页没有执行它们。已检入的 schema JSON 就是该导出。Cargo 断言和普通 CLI 验收仍待完成。

## V2 REST 墓碑

受保护的 Dashboard V2 REST 统一返回固定墓碑。

- 匿名 V2 REST：空 body 的 **401**（鉴权先于墓碑）。
- 已鉴权的 V2 REST（含回环本地模式）：**410**，body 为 `{ "code": "dashboardV2Removed", "message": "Dashboard API V2 has been removed; refresh the page and retry." }`。
- 既非 V3 墓碑前缀、非 V4，也非保留家族的未知 `/dashboard/api/...` 路径，在已鉴权时同样 410。未知的 V4 路径是 V4 的 `404`，不是墓碑。

保留的 `/dashboard/api` 家族（精确路径，无尾斜杠，无额外段）：

- `auth/status`、`auth/register`、`auth/login`、`auth/logout`
- `browser/sessions/{token}/ws`（token 非空）

`/dashboard/api/v3` 前缀是单独的 410 家族（`dashboardV3Removed`）。Vue 外壳与产品页面只调用 `/dashboard/api/v4`（`requestV3` 与 `requestV4` 共用该前缀；`dashboardV3.listAccounts` 使用 `GET /account-records`）。推理路由、面板 HTML 与 `/dashboard/assets/...` 不在墓碑范围内。

## 自有 CPA 控制

`GET /dashboard/api/v4/external-integrations/cpa` 以及不写入的、只带 enabled 的 `PUT` 返回 `CpaIntegration`。V4 挂回这些 V3 DTO。V3 REST 仍是上文的 410 墓碑。已检入的 `schema/dashboard-api-v3.schema.json` 与 `schema/dashboard-api-v4.schema.json` 是 Rust 示例导出。源码用 schemars skip 标记退役的请求字段。本页不编辑 schema JSON。整份 CLI 验收与运行时验收仍待完成。

没有外部 CPA 产品模式，也没有远程选择器。共享前缀仍是 `/dashboard/api/v4/external-integrations/cpa`。旧路由和旧字段是保留的拒绝与迁移表面。省略 `target` 时，请求指向自有子进程。已保存的历史行不会选中 `integration`。已保存的历史字节继续保留。显式迁移是必需的，尚未实现。本视图不解析 `OCG_CPA_BASE_URL`，它也不是产品开关。

`legacyMigrationRequired`（`legacy_migration_required`）是必需布尔值。仅当 `cpa_integration()` 为 `Some`，即存在已保存的历史远程 CPA 行时，它为 true。该值已脱敏。它不复制 URL、密钥、账号 id 或目录，也不从 `OCG_CPA_BASE_URL` 推导。

`runtimeUnavailableReason` 只表示自有运行时健康：既有的平台不支持文本、执行记录错误或 `cpa execution is unavailable`，否则为 JSON `null`。迁移语句和 `OCG_CPA_BASE_URL` 语句都不是该字段的取值。已保存的行可以在 `runtimeUnavailableReason` 为 null 时把 `legacyMigrationRequired` 设为 true，反过来也成立。这两个字段互不蕴含。`CpaRuntime` 不增加 `legacyMigrationRequired`。它的 `unavailableReason` 使用同一健康规则。`installed`、`running` 和 `owned` 仍是执行报告的值。历史数据不会把已安装或正在运行的子进程变成不可用，也不会把缺失的子进程变成已安装或正在运行。

`CpaIntegration` 上的自有生命周期：

| 字段 | 取值 |
| --- | --- |
| `configured` | 仅当存在自有托管记录，或执行报告表明自有可执行文件已安装时为 true。仅有历史行时为 false。 |
| `runtimeOwned` | 与 `configured` 同一谓词。 |
| `runtimeRunning` | `ExecutionReport.running`。 |
| `runtimeSupported` | `cpa_runtime_supported()`。 |
| `installedVersion` | 该制品已安装时取执行报告的 `current_version`，否则取自有托管记录的 `current_version`，再否则为 null。 |
| `latestVersion` | 执行报告的 `latest_version`（当前报告为 `v8.0.10`）。 |
| `updateAvailable` | 执行报告的标志。 |
| `currentOperation` | 执行报告的 `current_operation`。 |
| `baseUrl` | 执行报告给出端口时，取该自有环回地址；否则在存在托管记录时为 `http://127.0.0.1:{managed.port}`，再否则为 `""`。已保存的远程 URL、`OCG_CPA_BASE_URL` 和 `DEFAULT_CPA_BASE_URL` 都不出现在这个值里。 |
| `baseUrlReadOnly` | 恒为 true。 |

退役的单例输出仍可解析，并保持空或 false。它们不宣告外部账号或目录：

| 字段 | 固定值 | 原因 |
| --- | --- | --- |
| `managementKeyConfigured` | `false` | 历史管理密文不是自有执行来源。 |
| `inferenceKeyConfigured` | `false` | 历史推理密文不是自有执行来源。 |
| `enabled` | `false` | 自有启用按原生凭据计算，不是这个单例。 |
| `accountId` | `null` | 历史 `CPA_ACCOUNT_ID` 不是自有账号。 |
| `modelCount` | `0` | 自有目录按 `destination_models` 中的目的地分开。这个整数不能代表一份规范的自有原生目录。全局 `provider_model_catalogs` 的 CPA 行不复制到这里。 |
| `modelsRefreshedAt` | `null` | `destination_models` 没有刷新时间列，一个时间戳也不能覆盖按目的地分开的目录。 |

`revision` 与 `processGeneration` 不变。GET 不推进 revision，也不写入历史行、账号、目录或目的地字节。

请求仍可解析，并在任何 I/O 和任何写入之前被拒绝。候选 schema 不再宣告这些退役输入。

| 类型 | 从 schema 排除 | Serde 仍接受 |
| --- | --- | --- |
| `CpaControlTarget` | 变体 `integration` | `"integration"` |
| `CpaIntegrationUpdate` | `baseUrl`、`managementKey`、`inferenceKey` | 这三项 |
| `CpaTestRequest` | `baseUrl`、`managementKey`、`inferenceKey` | 这三项 |

候选的 `CpaControlTarget` schema 枚举是 `["owned"]`。`CpaIntegrationUpdate`、`CpaTestRequest` 和 `CpaRuntimeInstall` 上的 `target` 仍引用该枚举。拒绝语句仍在任何 I/O 之前：

- 显式 `target: "owned"` 加上 `baseUrl`、`managementKey` 或 `inferenceKey` 中的任一项：`owned CPA control does not accept a remote base URL or key`。
- 显式 `target: "integration"`，或省略 `target` 却带上这三项中的任一项：存在历史行时为 `Stored remote CPA configuration requires explicit migration and was left unchanged`，否则为 `Remote CPA is not a target in this product`。
- 对集成的 `DELETE` 使用同样的语句。已保存的字节保持原样。

`CpaIntegrationUpdate.enabled` 仍留在 schema 中。只带 enabled 的 PUT 返回自有视图，不改写历史行或账号。响应里的 `enabled` 为 false。

`OAuthStatusQuery` 与 `CpaTargetQuery` 是 serde 查询类型。它们不是 `JsonSchema` 类型，因此不在生成的 schema 里。这些查询上的 `target=integration` 仍能反序列化，并在任何 I/O 之前被拒绝。仍会构建客户端的路径，其 URL 与脱敏检查保持不变。遗留持久化不被删除。`CpaIntegration`、`CpaControlTarget`、`CpaIntegrationUpdate` 和 `CpaTestRequest` 上的面向读者的注释把这些字段称为退役的兼容字段。

## 自有 CPA 路由解释

只有现有的 `GET /dashboard/api/v4/routing/explain`。处理函数只调用一次 `explain_owned_routes(state, public_model, callable_protocol, now)`。`cpa_execution.rs` 含有 `pub(crate) mod explain`。该门面是稳定读取。本页不把遗留选择器或物化器写成当前执行事实。`contract_schema()` 包含 `RoutingExplanation`，已检入的 schema JSON 是该合约的示例导出。`dashboard_v4/types/tests.rs` 构造带有映射 `destinationId`、`adapterKind` 和 `migration_required` 的 `RoutingExplanation` 与 `RoutingResolvedMapping`。它断言线路上的 `migrationRequired` 为 false，并把省略该键的旧载荷反序列化为 false。它也覆盖排除项的 `authority: null`、`desiredRoutes: []`，以及下面的 `ownedProjection` 字段。本页没有执行该测试，也没有运行导出。播种数据库再调用 `explain` 的处理函数测试不是在线 Ready 证明。`verified_ready` 不能由单元测试设置。这次读取不解密、不放行、不写入，也不调用选择器、粘性变更、`OCG_CPA_BASE_URL`、物化器或出站客户端。

请求不变。`model` 在去除空白后必填。`clientProtocol` 默认 `chat_completions`。接受的值仍是 `chat_completions`、`responses`、`messages` 和 `gemini`。其他值是既有的非法请求。公开 Gemini 不是第四种可调用协议：门面收到的是 `chat_completions`，响应里的 `clientProtocol` 仍是 `gemini`。已鉴权的 V4、`ControlRevision` 和进程代仍在响应上。适配器不接收 `settings_update`。门面在既有的 `settings_update` 保护下捕获设置 revision、进程代、定价 revision、路由模式和 `conversationSticky`，适配器只格式化这些已捕获的值。`routingMode` 与 `conversationSticky` 是这份已捕获的配置，只用于展示。`conversationBinding` 为 `not_evaluated`。任何路由模式下 `expectedBasePolicyFirstPick` 都是 null。公开合格还要求一条已选中的可路由映射，其目的地、供应商和实际上游相同。公开模型不是这个键。漂移并入既有的 `state_changed` 路径。这次捕获不增加公开线路字段。该捕获的源码已经存在。Cargo 验收和普通 CLI 验收仍待完成。

门面 `QueryResolution` 是唯一的别名事实。`known == false`、`ambiguous == true` 或 `kind == None` 是既有非法请求 `model is unknown or ambiguous`。已知行即使 `routeable == false` 也是一次成功的解释。禁用、草稿和仅验证的映射仍留在 `resolved.mappings`。`resolved.kind` 与 `resolved.alias` 复制 `QueryResolutionKind` 和 `alias`。每条映射复制 `destinationId`、`providerId`、`upstreamModel`、`routeable`、`adapterKind` 和 `migrationRequired`。`RoutingResolvedMapping` 含有 `destinationId`、`adapterKind` 和 `migration_required`。最后一个字段的线路名是 `migrationRequired`。`#[serde(default)]` 使省略该字段的旧载荷为 false，新响应会发出该字段。

规范类别是 `Alias` 或 `PinnedRaw`。原始形态的请求必须与已保存的上游拼写完全一致。大小写变体是未知。精确的原始拼写在身份折叠之前，优先于拼写不同的规范别名或公开别名。请求本身就是规范别名时，它仍是别名。两个不同的原始供应商身份保持歧义。已知目录行如果没有匹配的已应用路由，保持已知且 `routeable == false`。仅期望平面的行保持已知且不可路由。已应用的仅验证行保持不可路由，即使期望平面不是仅验证。远程放置设置 `migration_required` 并强制 `routeable == false`。`routeable` 是额度与放置之后、当前已应用配置的资格。它不是目的地启用、目录启用、引导草稿，也不是期望平面与已应用平面 `validation_only` 标志的并集。

`QueryMapping.migration_required` 被复制到已脱敏的公开 `RoutingResolvedMapping`，包括没有 `RouteFact`、只有目录的历史远程行。该行的源码测试期望 `migrationRequired` 为 true、`routeable` 为 false，合格、排除和期望列表为空，并且远程 URL 已脱敏。本页没有执行该测试。Cargo 验收和普通 CLI 验收仍待完成。自有原生目的地标记 `cpa-owned-native` 不是供应商 id。供应商 id 来自原生凭据存储（`provider_id` 为 `cpa`）。第二个能解析同一名称的真实供应商保持歧义。`ValidationUse` 已不存在。

一条路由要成为公开合格，必须同时满足：平面为已应用；`client_configuration_eligible`；姿态是客户端且 `validation_only` 为 false；`runtime.owned_running`；`origin_verified`、`verified_ready`、`policy_ready`、`pin_capabilities_ready` 和 `tuple_aligned`；不是 `state_changed`、`stopped`、`poisoned`、`policy_malformed` 或 `unavailable`；不是 `known_restriction_blocks`，也不是 `trial_pending`；额度不是 `malformed`；不是 `migration_required`，且历史放置不是 `remote`。仅有静态标志不是发送承诺。合格行仍携带 `callerPending`、`secretRecheckPending` 和 `sendPending`。名次是一个字段。列表顺序是门面顺序。没有 SDK 的下一凭据。缺失的额度证据是 `unknown`。它不是无限额度，单独也不构成公开合格的阻断。已知且适用的重置会阻断。未知重置是 `trialPending`，不是已消耗的试探。过期证据仍可见，且 `applicable` 为 false。畸形文档是不可用，不是一份空的开放列表。仅期望、仅验证、当前已吊销、已停止、不受信任、状态已变和历史迁移的行留在 `eligible` 之外。

`desiredRoutes` 只是期望平面。它不并入 `eligible` 或 `exclusions`。`exclusions` 是未达到公开合格的已应用路由，顺序为门面顺序。每个线路代码是一条记录。每条记录携带同一个 `authority` 对象，包括精确额度，即使该路由不在期望平面中。`RoutingExclusion` 增加 `authority: RoutingRouteAuthority | null`。这个处理函数发出对象。对于没有结构化证明的旧载荷，null 仍然有效。

`RoutingRouteAuthority` 携带平面、凭据 id、路由凭据版本、当前数据库版本、供应商、绑定、auth id、注册 epoch、路由名次、目的地 id、遗留账号 id、账号标签、目的地标签、公开模型、上游模型、拼写（`empty`、`same` 或 `distinct_upstream`）、协议、端点 id、源、端点指纹、仅验证、通道、适配器种类、材料、姿态、配置排除代码、启用与草稿标志、设置步骤、原生供应商、原生模式、已列出的能力、授权覆盖、原生操作、`callerPending`、`secretRecheckPending`、`sendPending`、额度、`knownRestrictionBlocks`、`trialPending`、`clientConfigurationEligible`、`migrationRequired` 和历史放置。该对象不含材料指纹、密文、跳转令牌、策略令牌、认证文件路径或私有相对路径。公开路由的端点指纹包含在内。`RoutingNativePin` 是协议、端点 id、源、端点指纹和 HTTP 方法。`RoutingGrantDisposition` 是带 pin 的 `granted`、`not_granted`、`local_only` 或 `unavailable`。`RoutingOperationFact` 是一种生成种类加上该处置。`RoutingQuotaFact.state` 是 `unknown`、`evidence` 或 `malformed`。证据复制主体身份、可选的公开模型、窗口、来源、观测时间、观测 id、重置（`known`、`unknown_reset` 或 `expired`）、重置时间和 `applicable`。恢复租约和尝试 id 不复制。通道只有 `go` 或 `free`。适配器种类保持目的地的 `AdapterKind` 字符串，不从通道推导。

`ownedProjection` 复制 `desired` 与 `applied` 的 generation、revision 和 digest；`applyStatus`、`desiredRunning`、`runtimeChildGeneration`；`unavailable`、`stateChanged`、`stopped`、`poisoned`；`originVerified`、`verifiedReady`、`policyReady`、`policyMalformed`、`tupleAligned`、`pinCapabilitiesReady`；`ownedRunningBefore`、`ownedRunningAfter` 和 `ownedRunning`。只有两次本地观测都看到子进程时，`ownedRunning` 才为 true。`verifiedReady` 是捕获到的平面位。`applyStatus: applied` 并不蕴含这两者中的任何一个。两次运行观测不一致时，不因此设置 `stateChanged`。

既有的 `RoutingExclusionCode` 与 `RuntimeOnlyUncertainty` 变体保留当前线路拼写，以便旧载荷仍能解析。这个处理函数不发出 `mapping_protocol_incompatible`、`credential_disabled`、`binding_disabled`、`model_scope_denied`、`goat_not_eligible`、`goat_unverified`、`candidate_materialization_failed`、`production_route_unsupported`、`account_disabled`、`setup_not_ready`、`channel_mismatch`、`credential_missing`、`auth_error`、`cooling_down`、`free_channel_unavailable`、`quota_waiting`、`quota_due` 或 `quota_probing`。追加的排除代码是 `identity`、`rebound`、`version`、`setup_blocked`、`disabled`、`draft`、`scope`、`model`、`protocol`、`capability`、`native_presence`、`native_mode`、`native_targets`、`not_granted`、`material`、`configuration_unavailable`、`validation_only`、`quota_known_reset`、`quota_unknown_reset`、`quota_malformed`、`migration_required`、`state_changed`、`stopped`、`untrusted`、`owned_not_running`、`origin_unverified`、`not_ready`、`policy_not_ready`、`pin_capabilities`、`tuple_unaligned` 和 `policy_malformed`。从门面或真实运行时位发出的追加不确定项是 `cpa_selection_not_evaluated` 和 `quota_trial_not_evaluated`。事实为真时还会发出：`conversation_binding_not_evaluated`；返回的路由带有 `secretRecheckPending` 时的 `credential_recheck_pending`；`runtime.state_changed` 时的 `state_changed_after_snapshot`。`retry_exclusions_not_applied` 与 `upstream_result_unknown` 留在枚举上，且不被发出。

---

[维护者指南索引](../MAINTAINER.zh-CN.md) · [English](dashboard-api.md) · [文档索引](../README.zh-CN.md)
