[English](account-model-unification.md)

# RFC：重新设计账号与供应商模型

状态：**物理存储与 destination/credential 投影已切换；Account overlay 与部分运行时消费方仍是后续债。** 阶段 1–8 删到 schema v57，并把操作路由重挂到 `/dashboard/api/v4`。HEAD 已按实体增量写 destinations / credentials / catalogs：`project()` 与 `replace_all_on` 只留在遗留表升级和 V4–V6 备份转换，不再挂在账号创建、冷却或目录变更上。Payload V7 导出/导入以 Destination、Credential 为权威（密钥只存在已加密信封内），并携带平台与 CPA observer 管理凭据。合并导入时，若包中没有 CPA observer key，会保留目标已有 management key。账号页按 Credential 分组，不再要求另一份 Account 列表才能出卡片。仍未做：公开的 `POST/PATCH /destinations`；重挂的 `/accounts*` 仍是输入输出适配器；`legacy_account_id` 与 V3 Account overlay 仍桥接部分运行时消费方。[运行时不变量](runtime-invariants.zh-CN.md)与[Dashboard API](dashboard-api.zh-CN.md)描述 HEAD。

## 1. 问题在哪

网关只有一件事：把客户端请求经某个凭据路由到某个上游。现有设计用至少五种实体形态表达"上游"与"凭据"，而每种形态的身份都泄漏进共享代码。HEAD 上非测试代码统计：约 50 处前端、100 余处 Rust 分支形如 `if provider_id == X` / `isZenFreeAccount` / `isCpaIntegrationAccount` / `isCustomApiAccount` / `account_type === "managed"` / `type === "platform"`；`db.rs` 一个文件 24 处。

按危害排序的根因：

1. **`accounts` 把目的地和凭据混在一起。** 一行同时持有 Key、启用开关、顺序、cooldown，以及（对 Custom API）endpoint、协议与模型映射。其他每种形态都得决定自己用这一行的哪一半。
2. **Provider 与 Plan 是同一个身份（`provider_id`）。** Plan 是带用量窗口和到期的商业套餐；Provider 是传输。二者融合意味着 Plan 的用量语义搭在选择 adapter 的同一个键上，任何 Plan 专属行为都变成 adapter 特例。
3. **不是账号的东西被存成账号。** Zen Free（`…0002`）与 CPA（`…0003`）是保留 UUID 的 `accounts` 行，为的是参与排序与回退。于是每次遍历 `accounts` 都要排除或特判它们。
4. **一种传输，三套存储。** Custom API（`accounts` + `custom_config` + `account_model_capabilities`）、用户定义供应商（`providers` + `provider_models`）、平台站点（`platform_accounts` + `platform_links` + 快照）驱动的是同一个 Configurable HTTP adapter。守卫、模型发现、余额探测按形态各实现一遍。
5. **V4 identity 是 V3 行之上的卫星投影。** 正确的模型（identity → credential → binding → quota pool）已经存在，却是从 `accounts` 派生而非拥有数据；只要 V3 仍是权威，它永远成不了唯一事实来源。
6. **前端照搬了以上全部。** 三个服务端状态所有者（`accountsStore`、`identitiesStore`、`PlatformAccountsSection.view`）、一个 route-item 联合类型、按种类分支的卡片。

在保留其余的前提下修补任何一条，得到的就是今天的形状。解法是一个新模型加一次迁移，而不是再叠一层。

## 2. 硬约束与可推翻的现状

**保留——这些是安全与范围，不是设计口味：**

- Key 材料静态加密，列表与变更响应从不返回明文；日志脱敏。
- 目的地 URL 与 DNS 守卫（元数据地址、链路本地、IPv4 伎俩、带密钥不跟随重定向、不转发客户端鉴权）。
- 所有控制平面变更走 CAS；创建按 operation id 幂等。
- 不动态加载代码：adapter 编译进二进制。用户定义传输是绑定到 sealed Configurable HTTP adapter 的数据。
- 无远程同步、无 Admin API、无 Tauri `invoke` 数据通道。
- 本机单用户信任边界。

**摆在桌面上——以下每一条都可以改：**

- `accounts` 作为主表及其列集。
- Provider 与 Plan 共用 `provider_id`。
- 保留 UUID 的单例行。
- Custom API / 用户定义供应商 / 平台站点各自的存储。
- V3 作为冻结的权威契约。V3 降为新模型之上的兼容垫片，经一个弃用版本后移除。
- 账号 / 供应商 / 别名三页现在的划分方式。
- `runtime-invariants.md`、`AGENTS.md`、`DESIGN.md` 中编码了上述内容的措辞。

## 3. 目标模型

四个实体。其余一切都是能力标志或观测。

### 3.1 Destination（目的地）

请求去哪、怎么成形。取代 Provider、Connection、Custom API 配置与平台父账号。

| 字段 | 含义 |
| --- | --- |
| `id` | 稳定 UUID |
| `adapter` | sealed adapter 类型：`opencode_go`、`zen`、`goat`、`minimax`、`kimi`、`ollama`、`cpa`、`http` |
| `name`、`brand_family` | 展示 |
| `base_url`、`protocols`、`auth_scheme` | 传输；sealed 类型由 adapter 固定，`http` 为用户数据 |
| `catalog` | 模型行：对外名、上游 id、按协议启用、首选协议 |
| `capabilities` | 见 3.5 |
| `plan` | 可选商业套餐：用量窗口、到期周期、价格来源（见 3.4） |
| `max_credentials` | 单例与账号所有 endpoint 为 `1`，否则 `null` |
| `observer_credential_id` | 可选的非推理凭据，用于读取站点数据（平台管理令牌） |
| `enabled`、`revision` | 生命周期与 CAS |

Custom API 账号成为 `adapter = http`、`max_credentials = 1` 的目的地，其唯一凭据在同一次写入中创建。用户定义供应商相同但 `max_credentials = null`。平台站点在此基础上加 `observer_credential_id` 与 `observer` 能力。Zen Free 是 `adapter = zen`、`auth_scheme = none`、`max_credentials = 1`。CPA 是 `adapter = cpa` 加 `capabilities.external_integration`，本机不持有推理凭据。

### 3.2 Credential（凭据）

一个可路由单元。取代账号行的 Key 半边、V4 credential 与平台 link。

| 字段 | 含义 |
| --- | --- |
| `id`、`destination_id` | 身份 |
| `name`、`notes` | 展示 |
| `secret` | 加密；`auth_scheme = none` 与外部集成为 `null` |
| `enabled`、`routing_rank` | 路由门与全局顺序（所有目的地共用一个顺序） |
| `scope` | 模型范围：全部，或显式对外名列表 |
| `grants` | 允许发送 secret 的 endpoint id / origin |
| `auth_state`、`auth_state_version`、`last_error` | 本地验证状态 |
| `cooldowns` | 通用与按窗口的 `until` 时间戳（自 `accounts` 迁出） |
| `quota_pool_id` | 共享池成员 |
| `onboarding_task` | 可选的托管注册状态机（取代 `account_type = managed` + `setup_step`） |
| `purchase_date` | 仅当目的地有 `plan` 时有意义 |
| `revision` | CAS |

无密钥目的地仍恰有一行 `secret = null` 的凭据，因此启用与顺序是统一的：路由规划器遍历凭据而非目的地，不存在单例检查。

### 3.3 Quota pool（额度池）

形状与 V4 相同：`id`、`policy_mode`、`relation_confidence`、成员。某成员上的 429 或窗口耗尽扇出到整个池。

### 3.4 Plan（嵌入目的地）

从传输中分离出来的商业语义：`usage_source`（`official_api`、`local_projection`、`none`）、`windows`（5 小时 / 周 / 月定义）、`expiry_cadence`、`pricing_source`、`manual_calibration`。两个目的地可共用 adapter 而 plan 不同（例如未来的 GOAT 档位）；预设声明时 plan 也可挂在 `http` 目的地上。Plan 是数据；adapter 从不读 `provider_id` 去推断它。

### 3.5 Capabilities（能力）

目的地上的布尔与枚举，sealed 类型由 adapter 推导，`http` 由行推导：

`toggleable_credentials`、`testable`、`discoverable_models`、`official_balance_probe`（主机允许表）、`observer`、`managed_signup`、`external_integration`、`billing_tier_required`、`redirect_policy`、`auth_header_kind`、`identity_headers`。

所有差异化的 UI 与路由行为都读这些。目标代码中不存在 `is_zen_free`、`is_cpa`、`is_custom` 或 `provider_id == "opencode"`，Rust 侧亦然。

### 3.6 Observations（观测）

按目的地或凭据键控的仅追加快照：用量窗口、钱包 / 订阅 / Key 额度、价格表、目录抓取结果、探测结果。仅用于展示，除通过凭据上显式的 cooldown 写入外，绝不作为路由输入。

## 4. 目标 UI

- **账号**页列出**按目的地分组的凭据**，按全局路由顺序。每组使用同一张卡壳（本分支已交付的那张）：目的地头部（品牌、名称、类型、副标题、能力动作）加每个凭据一行（名称、状态标签、元信息、开关、工具、菜单）。单凭据目的地渲染同一张卡但只有一行；不再有"账号卡"与"平台卡"之分。
- **供应商**页列出**目的地**。详情 = 目录、套餐 / 价格、传输设置、观测凭据。Custom API 目的地像任何 `http` 目的地一样出现在这里；由于 `max_credentials = 1`，其传输字段也可从凭据卡编辑。
- **新增**是一条流程：选择目的地（已有，或从 sealed 类型 / 预设 / 手工 `http` 新建），再添加凭据。目的地具备 `managed_signup` 时，托管注册作为凭据的 onboarding task 从同一流程发起。
- **别名**页职责不变：跨已启用凭据，每个对外名一行。
- Custom API 在"新增"中保留一等入口，即"手工 `http` 目的地 + 一个凭据"。它是任何上游的兜底，永不隐藏。

## 5. 目标 API 与存储

- **Dashboard API V4 成为完整表面**：`destinations`、`credentials`、`quota-pools`、`observations`、`onboarding`，加上既有的 `contract`、`templates`、`applications`。所有变更走 CAS；创建按 operation id 幂等。
- **V3 降为读写垫片**，在新表之上维持一个版本（`accounts` ↔ 凭据 + 单凭据目的地投影），然后移除。`schema/dashboard-api-v3.schema.json` 不再是冻结的权威；生成的 V4 类型才是。
- **存储**：新表 `destinations`、`destination_models`、`credentials`、`credential_grants`、`quota_pools`、`quota_pool_members`、`observations`。一次迁移读取 `accounts`、`providers`、`provider_models`、`account_model_capabilities`、`platform_accounts`、`platform_links`、`credential_bindings`、`cpa_integration` 与 V4 identity 表写入新表；垫片版本之后删除旧表。迁移是全量的：每一现有行必须恰好映射为一个目的地与一个凭据，否则迁移拒绝运行。
- **转移包** payload V7 直接携带新实体。

## 6. 迁移

绞杀者模式；每个阶段独立交付。

1. **`ocg-domain` 里的领域模型**——`Destination`、`Credential`、`Capabilities`、`Plan` 类型及从现有行的全量映射器。纯代码加测试；不接线。
2. **前端能力层**——`src/domain/account-capabilities.ts` 从今天的目录 / connection 投影计算 3.5 的能力记录。替换全部前端身份谓词。零行为变化；在后端推进期间解锁 UI 工作。
3. **前端状态归并**——一个 `destinationsStore` + `credentialsStore`（或一个 store 持两张映射），经适配层由现有 V3/V4 读接口供给；`PlatformAccountsSection.view` 与 route-item 联合类型从视图中消失。
4. **新表 + 迁移 + V4 完整表面**，V3 重新实现为其上的垫片。在 shadow 对比仪表之下落地，切换前要求 fixture 套件路由结果一致。拆分为：
   - **4a 投影（只读，无 schema）。** `ocg-core` 从活行（accounts、动态供应商、平台父与 link、CPA、identity binding 与额度池）构造 `LegacyDestinationFacts` / `LegacyCredentialFacts`，运行阶段 1 的映射器，join 持久化目录快照，返回完整的目的地 + 凭据集合或拒绝映射的行清单。对每个现有集成 fixture 的测试断言全量——这就是以代码执行的迁移测试计划。
   - **4b V4 读表面。** 由 4a 提供 `GET /dashboard/api/v4/destinations` 与 `GET /dashboard/api/v4/credentials`；schema、生成类型、`contract:v4:check`、成对文档。
   - **4c 前端读取目的地。** `destinationsStore` 由 4b 供给；账号页通过共用卡壳渲染按目的地分组的凭据；删除 route-item 联合类型。拆分为：
     - **4c-1** 由投影分组（`destination-groups.ts`）；`PlatformAccountsSection` 仍只作变更宿主。
     - **4c-2** `DestinationCard` + `CredentialRow`；删除 `PlatformAccountCard`。恰有一把 Key 的目的地折叠成今天的 `AccountCard`；平台父即使只有一把 Key 也保持组卡。拖柄移动整组；行菜单的上移/下移不跨组。
   - **4d 新表 + 双写 + V3 垫片。** 不可回头点；只在 4a–4c 对真实数据跑通之后进行。拆分为：
     - **4d-1 影子表 + 回填。** schema v50 创建 `destinations`、`destination_models`、`credentials`、`credential_grants`。打开数据库时若 4a 投影全量成功则用其重建这些表。V3/V4 读与全部变更仍走旧表。不复制 Key 材料。v45 已有的 `quota_pools` / `quota_pool_members` 通过 `quota_pool_id` 复用；`observations` 留待后续切片。
     - **4d-2 双写。** 当时的做法：变更用完整 `project()` 刷新影子。**HEAD 已取代**为按行写 destination / credential / catalog。
     - **4d-3 V3 垫片。** shadow 对比保持干净后，V3/V4 读切到新表。拆分为：
       - **4d-3a** 当时：V4 GET 以活的 `project()` 为准，且只返回匹配的影子。**HEAD 已取代。**
       - **4d-3b** 当时的第一刀：V4 GET 下发已填充影子，但仍可能被活的 `project()` 拒绝挡住。**HEAD 已取代**——只要新表有行就下发，不被 `project()` 拒绝挡住。
       - **4d-3c** V3 列表成为同一套表上的垫片。
5. **路由规划器只读凭据与能力。** 删除 Rust 身份谓词；保留 UUID 只在迁移中出现。
6. **UI 切换**到 V4 完整表面：账号 = 按目的地分组的凭据，供应商 = 目的地，一条新增流程。
7. **弃用版本**：V3 垫片标记弃用，转移包默认 V7。
8. **移除版本**：删除 V3、旧表、`account-providers.ts`、`platform-accounts.ts` 中的 route-item 辅助函数。

阶段 1–3 不需要 schema 或契约变化。阶段 4 是最大的一步，也是不可回头的点；之前应先写一份迁移测试计划，覆盖每一种现有形态（每个 sealed adapter、根地址与完整路径两种 Custom、有 / 无 Key 的用户定义供应商、关联 / 未关联 / 待关联 Key 的平台、每一步的托管草稿、Zen、CPA）。

## 7. 实现后被本 RFC 取代的文档

- `runtime-invariants.md`："Provider 与 Plan 共用 `provider_id`"、"V3 冻结"、Custom API 账号所有制一节、平台父账号一节、保留 CPA / Zen 账号的措辞。
- `AGENTS.md`：V3 冻结 / V4 加法的边界句，以及"Custom API 归账号所有"。
- `DESIGN.md`：枚举卡片种类的账号 / 供应商左栏描述。
- `storage-migration.md`：新 schema 版本与迁移手册。

## 8. 待决事项

- V3 移除与垫片在同一个大版本还是晚一个版本。
- `plan` 嵌入目的地（提案）还是独立表被目的地引用；嵌入更简单，独立可跨目的地共享 plan。
- CPA 内部 OAuth 账号是作为 CPA 目的地下的只读观测投影（提案），还是仍只在 CPA 页面。
- 命名：内部用 `Destination`，还是沿用面向用户的"供应商"一词。RFC 内部使用 `Destination`，UI 标签留给 `DESIGN.md`。

### 阶段 1 已定下的 sealed 事实

`crates/ocg-domain/src/destination.rs` 把 sealed 能力与套餐编码为数据。HEAD 未唯一给出的取值按下述决定（代码中已无 `TODO(rfc)` 标记）：

- 所有带密钥的 adapter 都是 `NoFollow`；只有无密钥的 Zen 可以跟随重定向。旧 Go 描述符的 `follow_redirects` 被该不变量取代。
- 鉴权头类型不是能力。它由目的地的 `auth_scheme` 推导，`Http` 还按线协议推导（Messages → `x-api-key`）；该字段已删除。
- `toggleable_credentials` 已删除：每个凭据都有启用开关，包括无密钥的，因此该标志没有信息量。
- `testable` 对除外部集成（CPA）外的所有 adapter 为真，与"每张就绪卡片都提供测试连接"的运行时不变量一致。
- GOAT、MiniMax、Kimi 的套餐 `expiry_cadence = Monthly`——它们的卡片本来就显示按月的购买倒计时。MiniMax 与 Kimi 的窗口为 `FiveHours` + `Week`，即今天面板呈现其官方用量的方式。
- CPA 的 `base_url` 为 `None`（托管子进程的运行时回环地址）。
- 内置与平台的 `catalog` 在映射时为空；投影层（阶段 4a）在映射后 join 持久化的目录快照。

### 阶段 4a 已定下的迁移规则

`crates/ocg-core/src/destination_projection.rs` 对活行运行映射器。持久化形状与模型不匹配之处按以下规则处理：

- **投影从不发明状态。** 全新数据库只投影 schema 自带的 Zen 单例；CPA 目的地及其无密钥凭据只在集成写入其保留账号行之后出现。
- **binding 启用折叠进凭据启用**：`enabled = account.enabled && binding.enabled`。目标凭据只有一个开关；被禁用的 binding 本来也不可路由，因此折叠对路由无损，只丢失"两个开关中哪一个被关"的区分，而新 UI 不呈现这一区分。
- **额度池是存储的成员关系，绝不推断。** 同一 identity 上的第二个凭据只在显式加入时才共享池。
- **目录 join 从不新增模型。** 内置目的地只取持久化目录快照中存在的 id；平台目的地取其关联 Key 声明能力的大小写不敏感并集。
- **没有 `custom_config` 行的 Custom 账号拒绝映射**（`CustomAccountMissingEndpoint`）而不是跳过：全量的含义是每一行要么被映射、要么出现在拒绝清单里。
- `routing_rank` 是持久化账号顺序中的位置，不是原始 `sort_order` 列。

### 阶段 4c-2 已定下的 UI 规则

已在真实账号页验证（九个目的地组，含一个四 Key 的 New API 站和一个两 Key 的站）：

- 折叠判定看**未筛选**的组。筛选把多 Key 目的地收成一行时，仍渲染组卡。
- 平台父不会仅因当前只有一把 Key 而折叠（除非 `max_credentials = 1`）。
- 组拖柄的键盘上下键会把全局顺序持久化到刷新之后。行菜单的上移/下移只在组内持久化。
- V4 `GET /destinations` 与 `GET /credentials` 仍是分组来源；响应不含 `key_cipher`。
- 供应商页不变：仍列 connection，不列目的地组。

### 阶段 4d-1 的存储规则

- v50 表是 `project()` 的**影子**，还不是权威。投影拒绝时表保持空，不阻止打开数据库——V4 已经用 409 暴露拒绝。
- `credentials` 只存 `has_secret` 与 `legacy_account_id`，不得增加 `key_cipher` 或明文密钥列。
- `quota_pools` 已在 v45 存在；v50 不得再造一张池表。
- 目录与授权行的顺序就是 `project()` 的顺序，用 `ORDER BY rowid` 重建；没有 position 列。

### 阶段 4d-2 已定下的变更规则

当时的双写：写入用完整 `project()` 快照重建 v50 影子。**HEAD 已取代。**
运行时按行持久化 destinations、credentials 与 `destination_models`。
`refresh_destination_shadow` 只把内建目录与已持久化的 contract 对齐。
`replace_all_on` 只留给遗留表回填。

### 阶段 4d-3a 已定下的读取规则

当时：V4 GET 以活的 `project()` 为准，影子仅在与活投影相等时下发。
**HEAD 已取代**为下面 4d-3b 的“已填充表优先”规则。

### 阶段 4d-3b 已定下的读取规则

**HEAD 已取代。** V4 `GET /destinations` 与 `GET /credentials` 通过
`load_all` 下发已填充的 destinations/credentials。活的 `project()` 拒绝
不得挡住这些行。空库或遗留表升级窗口仍回落到 `project()`。

### 阶段 4d-3c 已定下的读取规则

V3 `GET /accounts` 与 `GET /platform-accounts` 从已填充的 v50 影子取得身份与顺序
（`credentials.legacy_account_id` 以及 `destinations.legacy = platform_parent`）。
列出的每一行仍是活的遗留记录——Key 材料从不从影子读取。影子未点名的行按活列表
顺序追加，因此冻结的 V3 合约不会丢掉账号。空影子或加载失败回落到活列表顺序。
V3 不返回 `409 destinationProjectionRefused`。

### 阶段 5 已开始的路由规则

活的执行器加载 `routing_projection()`（已填充的影子，否则用全量 `project()`），
当某个 `adapter = zen` 的目的地其凭据 `cooldowns.free_until` 仍在未来时，把
Free 视为耗尽。规划器的 Free 门不再看保留的 Zen 账号/供应商 UUID。有投影行时，
规划辅助函数取目的地 adapter（`adapter_for_account`、`account_channel_for`）。
裸账号行上的 `free_channel_is_exhausted_at` 仍给面板探测用，按目录 adapter
`ProviderAdapterKind::ZenFree` 判断，不看保留账号 id，也不走
`validate_provider_binding`。活候选顺序使用 `list_accounts_for_v3`（影子凭据
顺序）。有投影时，请求物化按 `destination.legacy` 匹配映射（内置/动态 id，或
Custom/平台 → adapter `http` 加上 Configurable HTTP 目录键）；没有投影的测试
仍回落 `account.provider_id`。`RoutingCandidate.adapter` 有投影时来自
`destination.adapter`，否则来自映射的目录种类——不来自 `account.provider_id`。
Key 材料来自凭据行。密封 adapter 用 `ProviderAdapterKind`
（`get_by_kind`）取描述符，不再用账号上的保留 UUID。选择器通道资格用
`channel_for_adapter`。Zen/CPA 解析路径不再要求保留账号 id。CPA 实发用
`AttemptSpec::is_local_external_integration`（CPA adapter 的代理模型），不用
`provider_id`。诊断通道用 `mapping_is_zen_free`（adapter 种类），不用
`ProviderMapping::is_zen_free`。有投影行时，Custom 与动态 HTTP 看
`destination.legacy`；剩余行和 `resolve_route_with_dynamics` 按是否存在动态
供应商运行时拆分 Configurable HTTP，不再看 `is_custom_api`。隔离实发的授权
先认已存的 `account_custom_config`，否则走动态运行时。`/v1/models` 列表按映射
adapter / CPA 集成凭据接纳 Custom 与 CPA 行，不再用 `CPA_ACCOUNT_ID` 或
`mapping.is_custom_api()`。实发与探测路径上的身份头跟随目的地
`capabilities.identity_headers` 和 adapter 种类（`opencode_go` 会话；`zen`
还会补匿名字段），不再比较保留的 `provider_id` 字符串。解析、探测和生产支持
API 显式接收 `ProviderAdapterKind`，误导性的 `account.provider_id` 不能胜出。
Shadow 比较用路由 adapter 标注尝试，不再按 `provider_id` 查目录。转发日志的
`scope_to_provider` 按目录 adapter 区分 free / unknown / unpriced。目录映射仍按
`provider_id` 连接（`mapping_adapter_kind`、密封 Custom 目录键）；那是目录键，
不是账号身份判断。

### 阶段 8 已开始的删除规则

Schema v52 在把剩余 Account 字段回填到 `credentials`、并断言每个
`accounts.id` 都有 `credentials.legacy_account_id` 之后，物理删除了
`accounts` 表。目的地与凭据成为账号行存储。Schema v53 在把可映射的
Custom endpoint/协议/模型事实抄到 `destinations` / `destination_models`
之后，删除了 `account_custom_configs` 与 `account_model_capabilities`。
Schema v54 在把可映射的平台父账号抄到目的地（`legacy_kind=platform_parent`）、
把关联抄到推理凭据、并把管理密文落到观察者凭据之后，删除了
`platform_accounts` 与 `platform_links`。Schema v55 在把遗留
`cpa_integration` 行映射到 CPA 目的地（`adapter=cpa` / 遗留 CPA id）、
并把 `management_key_cipher` 落到观察者凭据之后，删除了该表。推理凭据
不承载管理密文。没有遗留行的全新库不会发明 CPA 目的地。Schema v56 在把遗留
`origin IN ('preset','custom')` 行映射到目的地（`legacy_kind=dynamic`）
与 `destination_models` 之后，删除了 `providers` 与 `provider_models`。
密封目录仍来自 `BUILTIN_PROVIDERS`；v56 不为 builtin 种子行发明目的地。
Schema v57 在把可映射的身份 / 绑定 / 入职 / 订阅事实抄到 credentials +
`credential_grants` 之后，删除了遗留身份附属表。`quota_pools` /
`quota_pool_members` 保留。那六张身份遗留表已不在。
V4 GET 列表仍不含秘密。Key
仍以密文落盘。
`src/domain/account-providers.ts` 已删除；卡片标志走目的地 capabilities
或目录键，不再用保留账号 UUID。`/dashboard/api/v3` 是 410 墓碑。

### 阶段 7 已开始的弃用规则

新节点备份导出 payload V7。加密 envelope 仍为 v1。V7 携带 `destinations` 与
`credentials`（明文密钥、平台与 CPA observer 管理凭据，以及 identity / grant /
cooldown 等 extras 只存在该信封内），以及 `quotaPools` 与 `node`。合并导入时，若包中没有
CPA observer key，会保留目标已有 management key。最新导出不再生成 `accounts`、平台行、动态供应商定义
或单独的 identities 数组。V4–V6 仍可通过旧图解码器转入同一套新模型导入对象。
若 V7 包仍带旧字段，必须与 dest/cred 一致，否则拒绝。V8 及更新是不支持版本错误。
重挂的 `/accounts*` 是输入输出适配器；新客户端的读模型是 V4 目的地/凭据。

### 阶段 6 已开始的界面规则

Accounts 上每个目的地分组都走同一套 `DestinationCard` 外壳，凭据行仍是
`CredentialRow`。没有平台父级时，页头品牌和类型来自目的地的 `brand_family` /
capabilities。`AccountCard` 不再挂在这个页面上。Providers 左侧列出目的地
（仍 join 到负责变更的 V4 connection；平台目的地没有 connection，用
`destination=`）。Add 是一条流程：Providers 的「添加供应商」和
`?view=providers&add=1` 都打开 Accounts 选择器（`add=1`；带 preset 的书签映射成
`preset:<id>`）。Providers 自己的预设浏览器不再是 Add 入口。

---

[维护者指南索引](../MAINTAINER.zh-CN.md) · [English](account-model-unification.md) · [文档索引](../README.md)
