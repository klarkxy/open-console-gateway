[English](dashboard-api.md)

# Dashboard API

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

面板 JSON 位于 `/dashboard/api/v4`。这是唯一存活的面板 JSON 前缀：增量 V4 路由加上挂回的 V3 操作处理器。V3 的 `$defs` 不再增加新字段。

V4 复用 V3 会话中间件。其列表返回与 V3 CAS 相同的 `ControlRevision`（`expectedRevision` / `processGeneration`）。V4 变更是 `POST /onboarding/commit`、`POST /credentials/{id}/rotate`、`PATCH /bindings/{id}`、`POST /identities/{id}/credentials`、`POST /applications/dsh`（同时绑定 GET 检查指纹）、`PUT /cpa/models`、`POST /provider-contracts/{scope_kind}/{scope_id}/catalog/remove` 与 `PATCH /alias-publication`，它们检查这两枚令牌；只读路由不检查。

已检入的仅增量 V4 契约是 `schema/dashboard-api-v4.schema.json`，由 `dashboard_v4::contract_schema_pretty()` 经 `crates/ocg-core/examples/export_dashboard_v4_schema.rs` 生成。生成的 TypeScript（`src/api/generated/dashboard-v4.ts`）只有类型，没有 HTTP 封装。`dashboard_v4/types.rs` 的 `CATALOG_TYPE_NAMES` 同样是有序 `$defs` 目录；追加时必须保持既有 definition 对象字节一致。

只读路由为 `GET /contract`、`GET /templates`、`GET /connections`、`GET /accounts`（身份列表）、`GET /account-records`（挂回的 V3 账号列表垫片）、`GET /destinations`、`GET /credentials`、`GET /applications/dsh`、`GET /cpa/models` 与 `GET /alias-publication`。这些读取不会发出出站请求。

`GET /templates` 是只读的添加目录：密封内置项（不含 CPA）加上 `custom-http` 手动模板。预设不属于该模板目录。模板没有用户实例或密钥。

`GET /connections` 是已保存实例的投影：已有账号的内置项、每一个用户定义供应商，以及每个 Custom API 账号各自一条；CPA 永远不是 connection。每条 connection 携带生命周期、授权状态、带原因的本地资格、endpoints、模型目标，以及一份遗留身份引用。connection id 是由该遗留身份派生的确定性 UUIDv5，从不由名称或 URL 派生。

`GET /accounts` 返回 `IdentityList { revision, identities[] }`。每条 `IdentitySummary` 携带 `identity`（`id`、`label`、`authorityRef` `{ issuerOrSite, tenantOrSubject }`、`identityConfidence`、`enabled`、`notes`）、`credentials[]`、身份级 `declaredRelations[]`（`platformAccountId`、`group`）以及 `legacy`（`kind` `account` | `platform_account`，`id`）。载荷形状是嵌套的：`credentials[].credential`（`id`、`purpose` `inference` | `platform_observer`、`materialKind` `api_key` | `external_reference`、`secretRef`——不透明句柄，绝不是材料本身、`hasMaterial`、`version`、`enabled`、`authState` `unknown` | `valid` | `invalid`、`authStateVersion`、未知时 `expiresAt` 为 null），同级字段为 `subject`（`account_credential` | `anonymous`）、`bindings[]`（`id`、`connectionId`、`allowedEndpointIds`、`allowedOrigins`、`modelScope`、`enabled`、`routingRank`）、`quotaWindows[]`、`onboardingTask`、`subscription`（未知时为 null）、`lastError`（已脱敏；无法安全脱敏时为 null）与 `legacy`。平台父账号的 `platform_observer` 凭据是投影，没有 `credential_state` 行。`authState` 是本地状态：`unknown` 绝不是 `valid`；`valid` 需要既有验证记录。Vue 账号页只把该投影叠加到展示上；Key 轮换、绑定编辑与身份内新增凭据走 V4 原生路由，其余账号变更走挂回 V4 的原 V3 路径。

`GET /destinations` 与 `GET /credentials` 是 `destination_projection` 的 RFC 第 4b 阶段投影：只读、不含密钥、带 revision。只要 destinations/credentials 已有行就下发，即使活的 `project()` 会拒绝。空库或遗留表升级窗口才回落到 `project()`；只有这条空库回落会返回 `409` `destinationProjectionRefused`，`details` 数组点名每一条被拒绝的行。

节点转移（`POST /accounts/transfer/export|preview|import`）挂在 V4。最新导出是 payload V7：`destinations` 与 `credentials`（明文密钥、平台与 CPA observer 管理凭据，以及 identity / grant / cooldown extras 只存在已加密信封内），外加 `quotaPools` 与 `node`。合并导入时，若包中没有 CPA observer key，会保留目标已有 management key。不再生成 `accounts`、`platformAccounts`、`platformLinks`、`dynamicProviders` 或 `identities`。这些 Portable 类型只用于转移，不进 V4 列表 DTO。V4–V6 包仍走旧图解码器导入。

V4 不把授权 `unknown` 当作 `valid`。资格是本地投影，不是上游健康。

`POST /onboarding/commit` 请求体：`expectedRevision`、`processGeneration`（与 V3 相同的 CAS 令牌）、`operationId`（客户端生成的 UUID）、`connection`、可选 `authorization`，以及 `targets`。

`connection` 为 `kind: new`（`templateId` 是 `custom-http` 或预设 id，外加 `name`、`endpointUrl`、`upstreamProtocol`、`authKind`）或 `kind: existing`（`connectionId`）。`authorization` 为 `kind: api_key`（`secretInput`，可选 `accountLabel` / `notes`）或 `kind: none`。`targets` 把公开模型映射到精确上游模型，可带每条目的上游覆盖。`new` 要求 `targets` 非空；`existing` 必须为空（模型编辑仍走 V3 `PATCH /providers/{id}`）。

求值顺序：(1) 解析；(2) `operationId` 必须是 UUID；(3) 先取 `settings_update` 锁，再在 CAS 之前做幂等查找——若该 `operationId` 已用同一载荷摘要提交过，则直接返回已存的无密钥结果，并带 `replayed: true` 与当前 revision 令牌，不再检查 CAS（首次写入已经推进 revision）；同一 `operationId` 配不同载荷返回 `409` `operationPayloadMismatch`，不写入；(4) CAS 检查（`409` `revisionConflict`）；(5) 写入。

`new` 复用 V3 用户定义供应商校验。模板 id 作为不透明预设 id 透传；预设表单归前端所有，Rust 只消费由 `resources/provider-presets.json` 生成的 offering 投影。keyed 鉴权下省略 `authorization` 只保存定义（随后 V4 connection 的授权为 `missing`）；`api_key` 在 keyed 鉴权下要求非空密钥；`none` 仅对无鉴权模板有效，且总会创建单例账号。供应商行、可选的首个账号行与操作记录在同一 SQLite 事务中提交；提交后按 V3 同样方式安装动态供应商快照。

`existing` 只接受用户定义（dynamic）且为 keyed 鉴权的 connection 新增 `api_key`。内置与 Custom API 的 connection id 返回 `400`（“add Keys on Accounts”）。账号行与操作记录在同一事务中提交，随后只推进 revision（`reload_contracts=false`），与 V3 普通账号创建一致。

结果为 `{ revision, connectionId, credentialId | null, targetIds, replayed }`。`connectionId` 是动态供应商的确定性 UUIDv5；`credentialId` 是账号 id；`targetIds` 是每个公开模型的 UUIDv5。响应从不包含密钥、密文或摘要。

**幂等操作。** `operationId` 与载荷摘要绑定一次提交：摘要是只对语义载荷——`operationId`、`connection`、`authorization`（因此覆盖密钥）与 `targets`——计算的 hex HMAC-SHA256；`expectedRevision` / `processGeneration` 不参与，所以刷新 CAS 令牌后的重试仍会重放。每次提交存储在 `dashboard_operations`；已存的 `result_json` 不含密钥。插入时会清理超过 30 天的行；被清理后，同一 `operationId` 视为新写入。

`POST /credentials/{id}/rotate` 替换一条投影凭据上的 Key。必须带 CAS 令牌，没有 `operationId`。凭据 id、绑定与配额关系保持不变。`version` 与 `authStateVersion` 一起递增；`authState` 变为 `unknown`；底层账号的 `auth_error` / `last_error` 与验证结果会被清空，避免旧版本污染新 Key。请求体是 `{ secretInput }` 加上 CAS 令牌。结果不含密钥。平台观察者、匿名、无鉴权与 CPA 凭据返回 `400`。未知 id 返回 `404`。过期 CAS 令牌返回 `409` 且不写入。

`PATCH /bindings/{id}` 编辑一条推理绑定。必须带 CAS 令牌，没有 `operationId`。请求体是 `{ modelScope?, enabled? }` 加上 CAS 令牌，至少要有其中一个字段。`modelScope` 为 `{ kind: "all" }` 或 `{ kind: "only", models: [...] }`（精确 id，沿用既有模型名归一化）。同一身份上各绑定的启停彼此独立。未知 id 返回 `404`。平台观察者、匿名、无鉴权与 CPA 绑定返回 `400`。过期 CAS 令牌返回 `409` 且不写入。结果为 `{ revision, binding }`，不含密钥。

`POST /identities/{id}/credentials` 给已确认身份再加一把 Key。必须带 CAS 令牌，没有 `operationId`。请求体是 `{ connectionId, secretInput }` 加上 CAS 令牌。写入会新建一行 `accounts` 并复用既有 `identity_id`，插入 `credential_state` 与 `credential_bindings`，并把新账号加入该身份的额度池，全部落在同一 SQLite 事务中。不同的 `connectionId` 是第二件产品（D05）；同一 Plan connection 则是该产品上的另一把 Key。换 Key 不会另起一个新池。未知身份或 connection 返回 `404`。内置不可变 / Zen Free / CPA / 无鉴权 / Custom API / 平台观察者目标返回 `400`。过期 CAS 令牌返回 `409` 且不写入。结果不含密钥。

面板用 `GET /connections` 渲染供应商页 rail，用 `POST /onboarding/commit` 创建用户定义供应商，并用 `GET /accounts` 作为账号页的展示叠加。客户端在草稿改动时生成新的 `operationId`，对未改动草稿的重试沿用同一 id，成功后再重新生成。编辑与删除账号以及账号页上的其余账号操作仍走 V3；Key 轮换、绑定编辑与给既有身份新增 Key 使用 V4。

## Settings 变更流程

[![Dashboard V3 Settings 变更流程](../diagrams/dashboard-v3-mutation.visual-check.1440x900.light.png)](https://klarkxy.github.io/open-console-gateway/diagrams/dashboard-v3-mutation/)

[在 GitHub Pages 打开交互式流程图](https://klarkxy.github.io/open-console-gateway/diagrams/dashboard-v3-mutation/)。

这条流程只描述受 CAS 保护的 Settings 写入；发现、诊断和读取操作可能按上文所述跳过 CAS。客户端提交 `expectedRevision` 与 `processGeneration`。令牌不匹配时返回 `409`；客户端刷新令牌与受影响资源，但不会自动重放写入。

CAS 成功后，Host 先持久化新设置并释放设置锁。只有端口发生变化且监听器正在运行时才会重绑。若重绑失败，请求以 `internal` 代码返回 `500`。补偿逻辑仅在实时配置仍等于本次失败写入的端口时恢复旧端口，避免覆盖随后成功的写入。

## V2 REST 墓碑

受保护的 Dashboard V2 REST 统一返回固定墓碑。

- 匿名 V2 REST：空 body 的 **401**（鉴权先于墓碑）。
- 已鉴权的 V2 REST（含回环本地模式）：**410**，body 为 `{ "code": "dashboardV2Removed", "message": "Dashboard API V2 has been removed; refresh the page and retry." }`。
- 既非 V3 墓碑前缀、非 V4，也非保留家族的未知 `/dashboard/api/...` 路径，在已鉴权时同样 410。未知的 V4 路径是 V4 的 `404`，不是墓碑。

保留的 `/dashboard/api` 家族（精确路径，无尾斜杠，无额外段）：

- `auth/status`、`auth/register`、`auth/login`、`auth/logout`
- `browser/sessions/{token}/ws`（token 非空）

`/dashboard/api/v3` 前缀是单独的 410 家族（`dashboardV3Removed`）。Vue 外壳与产品页面只调用 `/dashboard/api/v4`（`requestV3` 与 `requestV4` 共用该前缀；`dashboardV3.listAccounts` 使用 `GET /account-records`）。推理路由、面板 HTML 与 `/dashboard/assets/...` 不在墓碑范围内。

---

[维护者指南索引](../MAINTAINER.zh-CN.md) · [English](dashboard-api.md) · [文档索引](../README.zh-CN.md)
