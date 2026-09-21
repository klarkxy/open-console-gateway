[English](storage-migration.md)

# 存储与迁移

本页是升级、备份与回滚的运维约定。schema 细节见 [持久化](state-and-lifecycle.zh-CN.md#持久化)。

## Schema v62 — 个人积分估算

v62 增加可空的 `credentials.credit_meter_json` 和 `forward_logs.credit_receipt_json`。每个账号独立保存配置、各笔积分、校准基准和估算消耗。结算记录与扣减在同一事务中提交，流式响应重复完成不会重复扣减，移除日志也不会补回已保存的余额。供应商容器和已有额度共享元数据不拥有这些个人计量状态。

v61 的控制台登录态列作为历史 schema 保留，升级到 v62 时清空；运行时不再读取或续期这些令牌。配置重写只为同一凭据、目的地和地址保留积分计量。该账号换 Key 不会重置余额。推理授权、冷却和额度恢复保持不变。增量修改在事务中完成，不单独建立 pre-v62 备份。回滚需恢复升级前的整份数据目录；旧版二进制拒绝 schema v62。

`.database-open-gate.lock` 的独占锁串行化初始化，每个打开的数据库另持有 `.database-open.lock` 的共享锁。只有在所有旧数据库句柄关闭、打开者能取得独占锁时，才恢复未完成的积分结算记录；前一个初始化失败后，等待者会重新判断并执行恢复。并发执行 CLI status 不会改动仍在运行的请求。若网关异常退出后还有其他句柄存活，恢复会延后到一次所有句柄均已关闭后的重新打开。目录使用期间不得删除或替换这两个锁文件；升级前须停止不参与此锁的旧版程序。

## Schema v60 — 按 Key 额度恢复

v60 在 `credentials.quota_recovery_json`（可空 TEXT JSON）上增量保存已确认的按 Key 额度耗尽。`migrate_to_v60` 要求 schema v59，调用 `quota_recovery::ensure_column`，再写入 `schema_version` 60。已经是 v60 的打开仍会执行 `ensure_column`。不改写 credentials 表，也不写 pre-v60 SQLite 快照。普通冷却列、凭据 ID、路由顺序和 Key 密文保持原样。

JSON 保存 epoch、原因、窗口映射（可选重置时刻）、观测时间、下次重试和失败次数。试探租约只在进程内，不落库；重启后从该列恢复等待与退避。恢复独立于额度池和普通冷却。本机目的地投影重写会先快照再写回该列。节点转移不导出它。不替换 Key 的元数据编辑会保留它；轮换、替换 Key 以及托管 Key 写入会把它置为 NULL。

旧版二进制拒绝打开 v60 数据库（`existing_version > CURRENT_SCHEMA_VERSION`），也不会执行额度恢复闸门。回滚需恢复升级前的整份数据目录；这不是对已迁移文件的行为保持降级。

## Schema v59 — 运行时授权与模型权威

v59 持久化 `credentials.authorization_connection_id`，保留已有 Endpoint 授权命名空间，不改变授权值、Key 密文、凭据 ID 或全局顺序。平台 Key 保留历史上的逐 Key 授权身份，共享 HTTP Key 保留连接身份；正常路由直接读取该字段。

旧 Custom 协议判断在一个事务内归入目的地模型目录。共享模型存在冲突判断时，升级明确失败，不合并权限。CPA 已选目录同时写入 `destination_models`。非空 v58 数据库在变更前生成已校验的 `data.sqlite.pre-v59.<timestamp>.bak` 和 `.sha256` 文件。回滚需恢复对应的升级前数据目录，不提供逆向迁移。

## 路由卡片（schema v59，不新增表）

`settings.routing_cards_v1` 保存带版本的卡片 ID、目的地引用和凭据成员。卡片身份独立于共用的目的地配置。运行时仍以 `credentials.routing_rank` 为顺序：一次 CAS 布局写入校验完整推理凭据集合，在同一事务提交展开后的顺序与卡片元数据。读取不会重排凭据；旧的纯排序写入按保存的顺序整理卡片边界。相邻卡和空卡保持独立。Payload V9 增加经校验的 `routingCards`；V4–V8 导入按凭据顺序生成卡片。

## 数据目录与加密身份

每次打开数据库都使用 Host 解析的 cipher（CLI、桌面、Docker 均为 `Database::open_with_cipher`）。迁移前会检查已有账号密文，解密错误会 fail closed。新写入使用已认证的 AES-256-GCM（`v2:`）。无前缀的旧 XOR 仍可解密，以便备份恢复；成功的 Host-cipher 打开会把这些行改写成 v2。XOR 恰好解出 UTF-8 不会被当成 v2 成功。请保留原 cipher；改写密文无法修复不匹配。

| 形态 | 默认数据目录 | 加密身份 |
| --- | --- | --- |
| Windows 桌面（Tauri） | `%USERPROFILE%\.ocg-mgr` | `MachineBoundCipher`，取自 `USERNAME`、`COMPUTERNAME` 与 `APPDATA`。数据目录不作为 cipher 种子；此路径没有 `.encryption-key`。 |
| macOS / Linux 桌面（Tauri） | `~/.ocg-mgr` | `StaticKeyCipher`，取自 `<data-dir>/.encryption-key`（首次启动创建）。 |
| CLI | `~/.ocg-mgr-cli`，或 `--data-dir <path>` | 优先级：`--encryption-key` > `OCG_MANAGER_ENCRYPTION_KEY` > `<data-dir>/.encryption-key`。 |
| Docker | 容器内 `--data-dir /data`（Compose 卷 `ocg-data`） | 同 CLI 解析。可选 `OCG_MANAGER_ENCRYPTION_KEY` 是显式恢复覆盖；正常卷保留 `.encryption-key`。`/data` 内文件必须保持 UID/GID `10001` 可写。 |

每种形态使用自己的加密身份：

- Windows 桌面数据无法在另一个 Windows 用户或机器上解密账号密文，也无法在 CLI/Docker 静态 cipher 下解密。
- 把 GUI 目录拷到 CLI 默认路径（或反向）会用不同的目录，且在 Windows 上是不同的 cipher。
- 如果进程以 `--encryption-key` 或 `OCG_MANAGER_ENCRYPTION_KEY` 启动，只恢复 `.encryption-key` 不够；必须再次提供同一个显式秘密值。

## 升级与备份

GUI 或 CLI 启动时会原地执行 SQLite 迁移。打开新版二进制前：

1. 停止所有打开该数据目录的进程（桌面托盘 **退出**、CLI Ctrl+C / 服务停止、`docker compose stop`）。WAL 文件与 `data.sqlite` 同属一份库。
2. 备份**整个**数据目录，包括存在时的 `.encryption-key` 与 `browser-profiles/`；Docker 同时备份 `ocg-data` 与 `ocg-browser-profiles` 两个卷。保留上表匹配的加密材料。
3. 签名桌面升级器会自行停止并重启；CLI 与 Docker 升级保持手动。

不支持降级：旧版二进制无法打开已迁移的数据库。需要回滚时，恢复升级前制作的整目录备份。

## Schema v27 与 pre-v3 快照

`CURRENT_SCHEMA_VERSION = 62`（`crates/ocg-core/src/db.rs`）。下文保留 v1–v57 的历史迁移细节。v58 新增 `destinations.model_resolution`，回填 `adapter_defined` / `public_only` / `public_and_upstream`，把遗留 Custom 目的地改为不限制凭据数量，保留全部目的地与凭据 ID，并在修改非全新规范 v57 源之前写入经校验的 pre-v58 SQLite 备份。v60 增量保存 `credentials.quota_recovery_json`（见上文）。

## Schema v45 — 身份 / 凭据 / 绑定附属表

v45 把遗留 Account 拆成身份容器 / 凭据 / 绑定语义，但不搬移 Key 材料。`accounts` 行仍是物理凭据；增量附属表表达该行无法表达的内容。所有新 id 都是与 connection id 同一命名空间的确定性 UUIDv5，因此迁移幂等、可重试。不另写迁移前备份（只做加法，与 v43/v44 相同）。回滚仍是既有的整目录恢复。

表：

- `upstream_identities` — `id`、`label`、`identity_confidence`（`opaque` | `declared`）、`authority_site`、`authority_subject`、`enabled`、`notes`、`created_at`、`updated_at`
- `accounts.identity_id` — 新列
- `credential_state` — `account_id` 主键 → `accounts`，`credential_id` UNIQUE，`version` = 1，`auth_state_version` = 1，`rotated_at`
- `credential_bindings` — `id`、`account_id`、`connection_legacy_kind`、`connection_legacy_id`、`model_scope` JSON `{kind:all}` | `{kind:only,models}`、`enabled`、`created_at`、`updated_at`
- `legacy_identity_map` — `legacy_kind`、`legacy_id`、`new_kind`、`new_id`、`migration_version`
- `onboarding_tasks` — `id`、`account_id`、`kind` `managed_registration`、`step`、`state` `in_progress` | `completed`、…
- `subscription_records` — `account_id` 主键，`source` `legacy_manual` | `managed_payment`，`purchase_date`、`expires_on`、`recorded_at`
- `quota_pools` — `id`、`subject_kind`、`subject_ref`、`relation_confidence`、`policy_mode`、`created_at`
- `quota_pool_members` — `pool_id`、`account_id`（回填时每个身份一名成员；新增凭据默认使用独立额度池，只有显式选择共享才加入已有池）

附属行在同一事务中显式删除（DDL 声明了 `ON DELETE CASCADE`，但进程未启用 foreign-key pragma）。打开数据库时，v45 一致性检查用幂等回填补齐缺失的附属行；若仍不一致则 fail closed。v45 回填时，缺少必需的 `accounts` 列会通过普通 SQL 错误使打开失败，不会被跳过。

迁移规则：每个既有账号恰好对应一个身份（`label` = 账号名，置信度 `opaque`）、一份凭据（`version` 1），以及一条绑定到该账号 connection 的记录（内置供应商 / 动态供应商 / Custom 账号自己的 connection），`model_scope=all`，绑定默认 `enabled=true`。账号启用开关继续控制能否进入路由；单独禁用的绑定在重开和修复时保持禁用。路由排序读取既有 `accounts.sort_order`，不另存一份。`legacy_identity_map` 记录账号 → 身份 / 凭据 / 绑定。尚未 `ready` 的托管账号写入一条 `onboarding_tasks`，状态 `in_progress`、步骤为当前步；已 ready 的托管账号不编造历史。只有已经公布购买/到期日的密封内置 Provider 账号才写入 `subscription_records`，`source` 为 `legacy_manual`。用户定义与 Custom API 账号不写：日期保持未知，不以零定价（D07）。平台关联：被关联 Key 的身份变为 `declared`，`authority_site` = 父账号 `base_url`；每个平台父账号自有身份，并带一份 `platform_observer` 凭据（管理凭据，从不用于推理）。父账号与被关联 Key 永不合并；关系保持已声明、未验证（D04）。冷却列不搬迁：投影为额度窗口（generic / 5h / week / month → subject `credential`；free → subject `egress` `free_channel`，declared、authoritative），精确保留已存时刻。未知指标为 `null`，绝不为零。迁移为每个身份创建一个额度池（`subject` 为 credential / 身份 id，`relation_confidence` 为 unknown，`policy_mode` 为 authoritative_limit），从不写 `verified`。在当前 v46 写入路径下，新增凭据默认独立；显式与同身份凭据共享时才加入其额度池，并把关系标为 `declared`。路由遵守已存的 `model_scope` 与绑定 `enabled`。

每一次账号插入（V3 创建、托管创建、用户定义供应商首把 Key、V4 onboarding commit、节点导入）都通过与本迁移共用的唯一映射器，在同一事务写入附属行。平台关联 / 解除关联在同一事务更新被关联身份的置信度与站点。

轮换、绑定编辑、第二份凭据写入，以及可配置目的地 PATCH/DELETE 都是 V4 CAS 路径。新导出使用 portable payload V10（envelope v1）。V9 以目的地与凭据为权威，并携带按模型路由覆盖与 `modelResolution`；V7 导入确定性补默认值，V8 及以上必须显式携带。V4–V10 均可导入，V11+ 拒绝。遗留 Custom 行保持稳定 ID 与 `public_only` 解析，同时改为连接所有、多 Key。目的地/凭据归并保持单事务。

## Schema v46 — 持久化绑定授权

v46 把凭据绑定授权存成已保存事实：

- `credential_bindings.allowed_endpoint_ids` — JSON 字符串数组，内容为连接端点 id
- `credential_bindings.allowed_origins` — JSON 字符串数组，内容为规范化 Origin（`scheme://host[:port]`）

空数组表示无授权。NULL 只在一次性迁移期间合法；v46 按当前已配置的已分配连接端点（与 `/connections` 使用同一套 id）回填既有行一次。新 Key 捕获同一套安全默认：密封适配器保持静态官方端点范围且无 Origin；Custom 与动态默认 URL 可包含同源既有路由端点；外站 Origin 的模型覆盖不会被隐式授权。轮换、连接/URL/模型编辑、修复与重新打开都不会制造或扩大已保存授权。显式授权在修复/重新打开/导入中保留。

V4 `BindingDto.allowedEndpointIds` / `allowedOrigins` 投影这些已存事实。可选 PATCH 必须同时带上两个授权字段，按当前已配置的选定连接端点校验 id 与规范化 Origin；外站 id、畸形 Origin、未配置 Origin 会原子拒绝；接受的值按规范形式落库；两者都为空表示主动撤销。可选 `POST /identities/{id}/credentials` 的 `quotaSharing` 默认为 `{kind:"independent"}`（含省略该字段的旧客户端），或 `{kind:"shared", credentialId}` 显式指定同一身份上的推理凭据。既有 v45 身份池保留。显式加入会使用源池（若有），否则只创建包含所选源与新成员的池。普通共享池冷却写入在成员（含源）之间保留各窗口的最晚截止时间；显式手动清除仍会清空整个池。`GET /accounts` 的 `CredentialSummary.quotaPoolId` 投影已存池成员关系（非成员为 `null`），包括单成员身份池，即使 `quotaWindows` 为空也会给出。可选 `operationId` 复用 v44 HMAC 面板操作账本。V6 可移植身份图要求带授权，并在事务前拒绝畸形引用；V4/V5 导入一次性获得安全授权。不另写迁移前备份（只做加法，与 v43–v45 相同）。回滚仍是既有的整目录恢复。

## Schema v47 — 持久化入职草稿

v47 把入职生命周期加在既有 `providers` 行上：

- `providers.onboarding_draft` — 整型布尔，`NOT NULL DEFAULT 0`

既有行迁移为已配置。草稿可以省略 Key 和模型目标；即使草稿已有 Key 和模型，也不会进入路由、别名、目录或网关。普通 V3 Provider 写入会保留该标志，不会把草稿静默变成可路由。通过 V4 入职 `mode=complete` 完成草稿时，会在同一事务中连同操作回执清掉该标志。V6 节点导出包含草稿，且每个可移植 Provider 必须带 `onboardingDraft`；V4/V5 包不得带该字段。空白模型列表只对草稿合法。不另写迁移前备份（只做加法，与 v43–v46 相同）。回滚仍是既有的整目录恢复。

## Schema v49 — 不对下游列出的对外模型名

v49 增量创建 `unpublished_public_models`，保存已鉴权 `GET /v1/models` 中隐藏的对外名称：

- `public_model` — 主键，按大小写折叠存储
- `updated_at`

未出现的名称默认对外展示。隐藏名称仍可路由。写入路径是 `PATCH /dashboard/api/v4/alias-publication`。节点迁移不携带此表。不另写迁移前备份（只做加法，与 v43–v47 相同）。回滚仍是既有的整目录恢复。

## Schema v50 — 目的地影子表

v50 增量持久化阶段 4a `project()` 的目的地与凭据集合影子。这些表还不是权威：V3/V4 读取与所有变更仍走既有遗留行。控制面写入中 `project()` 会读到的那些，会在同一 SQLite 事务里重建影子；重新打开数据库仍会从活行重建。投影拒绝时清空四张表，不阻止打开数据库。

表：

- `destinations` — `id`、`legacy_kind`（`builtin` | `dynamic` | `custom_account` | `platform_parent`）、`legacy_id`、`adapter`、`name`、`brand_family`、`base_url`、`protocols_json`、`auth_scheme`、`capabilities_json`、`plan_json`、`max_credentials`、`observer_credential_id`、`enabled`
- `destination_models` — 目录行，主键为 `(destination_id, public_model_key)`；`public_model_key` 是大小写折叠后的对外名
- `credentials` — 只把 `has_secret` 与 `legacy_account_id` 作为与密钥相邻的事实；没有 `key_cipher`、`password_cipher` 或明文密钥
- `credential_grants` — `endpoint_id` / `origin` 授权

`credentials.quota_pool_id` 是可空文本 id，复用既有 v45 的 `quota_pools` / `quota_pool_members`。v50 不再造一张池表，也不创建 `observations`。JSON 列存放现有领域类型的 `serde_json`。日期为 RFC3339 文本。布尔为 `0`/`1`。不另写迁移前备份（只做加法，与 v43–v49 相同）。回滚仍是既有的整目录恢复。

## Schema v51 — 凭据密钥库

v51 把 Host cipher 的 Key 与口令材料增量存到 `credentials`：

- `credentials.key_cipher` — `TEXT NOT NULL DEFAULT ''`
- `credentials.password_cipher` — 可空 `TEXT`

既有行通过 `legacy_account_id` 从 `accounts` 复制密文。打开时重建影子也会同样复制，避免清空密钥。实发优先用非空的凭据密文，没有再回退到 `accounts` 行。V4 GET 列表仍不含秘密。本版本不删除 `accounts` 表。不另写迁移前备份（只做加法）。回滚仍是既有的整目录恢复。

## Schema v52 — 删除 `accounts`

v52 让 `credentials`（join `destinations`）成为账号行存储，并物理删除 `accounts`：

- 在 `credentials` 上补齐重建 Account 仍需要的列（`username`、`referral_code`、`cooldown_until`、`created_at`、`updated_at`、`auth_error`、`account_type`、`setup_step`、`provider_id`、`credential_kind`、`quota_scope`、`identity_id`，以及运行时仍会写入的校验与用量窗口列）。
- 在 `accounts` 仍在时通过 `legacy_account_id` 从该表回填这些列。
- 删除前重建一次目的地影子，使每个活账号都有凭据行。
- 若任一 `accounts.id` 没有对应的 `credentials.legacy_account_id`，迁移拒绝执行（不编造行）。
- 改写指向 `accounts` 的子表外键，然后 `DROP TABLE accounts`。

v52 之后的 Host 打开不会因为 `project()` 再也读不到 `accounts` 而清空已有的 destinations/credentials。`get_account` / `list_accounts` 从凭据重建 `Account`（`legacy_account_id` 仍是稳定的重挂 id）。运行时写入改打 credentials（provider/name/url 变化时同时写 destinations）。V4 GET 目的地/凭据 DTO 仍不含秘密；Key 密文只留在凭据 SQL 行。全新库迁移结束后不再保留 `accounts` 表。v53 之后 `account_custom_configs` 与 `account_model_capabilities` 也不再保留。v54 之后 `platform_accounts` 与 `platform_links` 也不再保留。v55 之后 `cpa_integration` 也不再保留。v56 之后遗留 `providers` / `provider_models` 也不再保留。身份附属表仍在。不另写迁移前备份。回滚仍是既有的整目录恢复。

## Schema v53 — 删除遗留 Custom 表

v53 让 `destinations` + `destination_models` 成为 Custom HTTP 的 endpoint、协议与模型映射存储，并物理删除两张遗留表：

- 删除前，每一条未链接的遗留 `account_custom_configs` / `account_model_capabilities` 必须能映射到 Custom 目的地（`legacy_kind=custom_account`，`legacy_id=account_id`）及其 `destination_models`。空 URL 或未知协议拒绝迁移；不编造 URL 或协议。
- 只存在于 `platform_links` 加遗留 custom 配置的已链接平台 Key 仍留在 `platform_*` 表，直到 v54。它们的遗留 custom 行不会映射成 Custom 目的地；遗留模型行会合入平台父级目录。
- 可读的遗留能力会与每一把 Custom 凭据的已存范围取交集，包括没有任何遗留行的 Key（`All ∩ []` 与 `Only[x] ∩ []` 都变成 `Only[]`）。列无法映射的遗留表在仍有行时拒绝迁移，且不会被当成空能力集合。
- 然后 `DROP TABLE account_custom_configs;` 与 `DROP TABLE account_model_capabilities;`。

v53 之后的 Host 打开不会清空已有的 destinations/credentials。`account_custom_config` / `list_account_model_capabilities*` 从 Custom 目的地重建（已链接 Key 则从平台父级目的地目录重建）。写入打 destinations 与 `destination_models`，并在需要时刷新 `credentials.destination_id`。V4 GET 目的地/凭据 DTO 仍不含秘密。全新库迁移结束后不再保留这两张遗留表。v54 之后 `platform_accounts` 与 `platform_links` 也不再保留。v55 之后 `cpa_integration` 也不再保留。v56 之后遗留 `providers` / `provider_models` 也不再保留。身份附属表仍在。不另写迁移前备份。回滚仍是既有的整目录恢复。

## Schema v54 — 删除遗留平台表

v54 让 destinations + credentials 成为平台父账号与关联的存储，然后物理删除遗留表：

- 删除前，每一条遗留 `platform_accounts` 必须映射为目的地（`legacy_kind=platform_parent`，`legacy_id=parent.id`），并带上 `base_url`、`name` 与 `platform_kind`（`new_api` | `sub2api`；同时镜像到 `brand_family`）。空 URL 或未知 kind 拒绝迁移；不编造站点或种类。
- 管理 `credential_cipher` 落到观察者凭据（`destinations.observer_credential_id`）。有密文时该凭据 `has_secret` 为 true。父账号 `version` / `snapshot` 以目的地附加列 `platform_version` / `platform_snapshot` 保留。
- 每一条遗留 `platform_links` 必须映射到已关联推理凭据（`legacy_account_id=account_id`），且 `destination_id` 为平台父级目的地。`group_json`、关联 version 与关联 snapshot 以凭据附加列保留。父级目的地或推理凭据缺失则拒绝迁移。
- 然后 `DROP TABLE platform_links;` 与 `DROP TABLE platform_accounts;`。

v54 之后的 Host 打开不会清空已有的 destinations/credentials。`list_platform_accounts` / `list_platform_links` 以及创建/更新/删除/关联/解除/刷新/导入都从 destinations + credentials 重建并写入。`project()` 从这些行推导平台父账号。V4 GET 目的地/凭据 DTO 仍不含秘密（无管理密文 / `key_cipher`）。全新库迁移结束后不再保留这两张遗留表。v55 之后 `cpa_integration` 也不再保留。v56 之后遗留 `providers` / `provider_models` 也不再保留。身份附属表仍在。不另写迁移前备份。回滚仍是既有的整目录恢复。

## Schema v55 — 删除遗留 CPA 表

v55 让 destinations + credentials 成为 CPA 单例接入的存储，然后物理删除遗留表：

- 删除前，遗留 `cpa_integration` 行必须映射到 CPA 目的地（`adapter=cpa` / 遗留 builtin `cpa`），有 `base_url` 时写到目的地，`management_key_cipher` 写到观察者凭据（`destinations.observer_credential_id`）。保留的推理凭据不承载管理密文。空管理密文或缺失列拒绝迁移；没有遗留行时不发明 CPA 目的地。
- 然后 `DROP TABLE cpa_integration;`。
- CPA 模型快照仍在 `provider_model_catalogs`。回环 / compose `base_url` 覆盖仍遵循既有运行时不变量。

v55 之后的 Host 打开不会清空已有的 destinations/credentials。`cpa_integration()` / `upsert_cpa_integration` / `delete_cpa_integration` 从 CPA 目的地 + 观察者凭据重建并写入。`project()` 不再读 `cpa_integration`。V4 GET 目的地/凭据 DTO 仍不含秘密（无管理密文 / `key_cipher`）。全新库迁移结束后不再保留该遗留表，也不会发明 CPA 目的地。v56 之后遗留 `providers` / `provider_models` 也不再保留。身份附属表与 `provider_model_catalogs` 仍在。不另写迁移前备份。回滚仍是既有的整目录恢复。

## Schema v56 — 删除遗留动态 Provider 表

v56 让 destinations + `destination_models` 成为用户定义 / 预设 HTTP Provider 的存储，然后物理删除遗留动态 Provider 表：

- 删除前，每一条遗留 `origin IN ('preset','custom')` 供应商必须映射为目的地（`legacy_kind=dynamic`，`legacy_id=provider.id`），并带上名称、`base_url`、协议、认证、origin、offering、preset、时间戳与 `onboarding_draft`。这些 id 上的每一条遗留 `provider_models` 映射到 `destination_models`（含 `upstream_override`）。keyed HTTP 空 URL、未知 adapter 或无法映射的遗留 `provider_models` 拒绝迁移；不编造 URL 或 adapter。
- 不复制 builtin 种子行。密封目录仍来自编译期 `BUILTIN_PROVIDERS` / adapter 注册表。v56 不为 builtin 种子发明目的地，也不发明运行时 adapter 行。
- 然后 `DROP TABLE provider_models;` 与 `DROP TABLE providers;`。

v56 之后的 Host 打开不会清空已有的 destinations/credentials。`list_control_plane_dynamic_providers` / get / upsert / delete / 入职提交 / 转移合并都从 destinations + `destination_models` 重建并写入。V4 connections/templates 的 builtin 来自密封目录，用户定义行来自 destinations。`project()` 不再需要遗留 Provider 表。V4 GET 目的地/凭据 DTO 仍不含秘密。全新库迁移结束后不再保留遗留 `providers` / `provider_models`，也不会发明用户定义目的地。v57 之后那六张身份遗留表也不再保留。`quota_pools`、`provider_model_catalogs` 与 `dashboard_operations` 仍在。不另写迁移前备份。回滚仍是既有的整目录恢复。

## Schema v57 — 删除遗留身份附属表

v57 让 credentials + `credential_grants` 成为身份、绑定、入职与订阅事实的存储，然后物理删除遗留附属表：

- 增量凭据列承接遗留身份/绑定/状态事实（`identity_confidence`、`authority_site`、`authority_subject`、`identity_enabled`、`identity_label`、`identity_notes`、`credential_version`、`auth_state_version`、`rotated_at`、`binding_id`、`binding_enabled`、`subscription_source`、`subscription_expires_on`）。遗留授权在缺失时抄到 `credential_grants`。遗留入职抄到 `credentials.onboarding_json`。遗留订阅抄到凭据的购买/到期字段。
- 删除前，每一条被凭据或平台目的地引用的遗留 `upstream_identities` 必须能映射。每一条遗留 `credential_state` / `credential_bindings` / `onboarding_tasks` / `subscription_records` 必须映射到凭据（`legacy_account_id` / `identity_id`）。孤立绑定，或没有可重建凭据/目的地的身份，会拒绝迁移。没有遗留行的全新库不会发明身份。
- 保留 UUID 仍在代码里。`legacy_identity_map` 只是迁移桥。
- 然后 `DROP TABLE` `upstream_identities`、`credential_state`、`credential_bindings`、`legacy_identity_map`、`onboarding_tasks` 与 `subscription_records`。已经是 v57 的库再次打开也会 `DROP TABLE IF EXISTS` 这些遗留表。
- `quota_pools` / `quota_pool_members` 保留（`quota_pool_members.account_id` 是重挂后的 `credentials.legacy_account_id`）。`credential_grants`、`provider_model_catalogs` 与 `dashboard_operations` 保留。

v57 之后的 Host 打开不会清空已有的 destinations/credentials。`list_identity_model` / 创建凭据 / 轮换 / 更新授权 / 入职 / 转移 V6+V7 身份导入 / 平台身份标签更新都不依赖遗留身份表。V4 `GET /accounts` 仍是重建后的身份列表，且不含秘密（无 `key_cipher` / 管理密文）。全新库迁移结束后不再保留这六张遗留表，仍有 `quota_pools`，仍有 Zen，也不会发明额外身份。这六张身份遗留表已不在，阶段 8 的身份遗留删除到此完成。不另写迁移前备份。回滚仍是既有的整目录恢复。

## Schema v58 — Custom HTTP 改为连接所有

v58 新增非空 `destinations.model_resolution`。内置目的地回填 `adapter_defined`，动态 HTTP 回填 `public_and_upstream`，遗留 Custom/平台目的地回填 `public_only`。遗留 Custom 行保持目的地 `id`、`legacy_id`、凭据、顺序、范围、授权、冷却、额度池和模型映射；只有 `max_credentials` 改为 `NULL`，使后续 Key 可以引用同一目的地。同名或同 URL 绝不合并。非全新的规范 v57 源会在修改前写入唯一且经校验的 `data.sqlite.pre-v58.*.bak` 及 SHA-256 sidecar；全新库不写备份。重复打开幂等。

## Schema v48 — 无运行语义的列与空遗留表

v48 删除四个无效列：

- `provider_contract_scopes.chat_completions_enabled`
- `provider_contract_scopes.responses_enabled`
- `provider_contract_scopes.messages_enabled` — 自 v31 起不再读取；实际启停是模型协议覆盖与偏好表
- `accounts.free_alias_enabled` — 惰性 `0`；Zen Free 使用 `accounts.enabled`

同时，仅在遗留的 `dynamic_providers` / `dynamic_provider_models` 存在且为空时删除它们（索引随表删除）。v47 源上任一非空遗留会在任何删除或升版本前 fail closed；schema 保持 47，行原样保留。当前 schema 的库不会擅自删除非空遗留行。

在非空 v47 库做 v48 写入前，进程会写入一份唯一、不覆盖的同目录快照：

```text
data.sqlite.pre-v48.<timestamp>.bak
data.sqlite.pre-v48.<timestamp>.bak.sha256
```

快照是独立的 v47 SQLite 文件（`VACUUM INTO`）；sidecar 第一字段是 `.bak` 的小写 SHA-256。全新空目录直接创建当前 schema，不写这份副本。没有降级路径；回滚需恢复升级前的整个数据目录。

## Schema v44 — 面板操作记录

v44 增量创建 `dashboard_operations`，供 V4 幂等控制面提交使用：

- `operation_id` — 主键
- `kind`
- `payload_digest` — 对语义载荷（`operationId`、`connection`、含密钥的 `authorization`、`targets`）的 hex HMAC-SHA256；CAS 令牌不参与
- `result_json` — 已存的无密钥结果
- `created_at`

摘要密钥是每库一份的随机 32 字节，惰性写入 `settings` 的 `dashboard_operation_digest_key`，任何 API 都不会返回它。该密钥与账号 Key 同库存放，因此沿用既有本地存储威胁模型；它避免把已存摘要做成密钥的无键哈希，并不能防御持有数据库文件的攻击者。插入时会清理超过 30 天的行。该迁移只做加法，不另写迁移前备份（与 v43 相同）。回滚仍是既有的整目录恢复。

## Schema v42 — 统一的供应商表

v42 把 `dynamic_providers` / `dynamic_provider_models` 重命名为 `providers` / `provider_models`，并为 `providers` 新增四列：

- `origin` —— `builtin` | `preset` | `custom`。builtin 行是七个密封 Adapter 种子（OpenCode Go、Zen Free、Command Code GOAT、MiniMax CN、Kimi CN、Ollama Cloud、Custom API）的展示镜像；CPA 是静态外部接入，**不**进表。preset 行跟随 preset 派生的 dynamic 供应商，custom 行跟随手工创建的 dynamic 供应商。
- `adapter_kind` —— builtin 行镜像密封 `ProviderAdapterKind`；每条 dynamic 行的值都是 `configurable_http`。
- `offering` —— `plan` | `api`。builtin 行从 `ocg_domain::provider::builtin_offering(provider_id)` 取；dynamic 行通过 `ocg_domain::provider::preset_offering(preset_id)` 从 `preset_id` 推导（仅 custom 的行默认为 `api`）。
- `endpoint_per_account` —— builtin 行除 Custom API（值为 `1`）外都为 `0`；dynamic 行一律 `0`。

v41 的 `provider_model_protocol_preferences` 表被重建，去掉了它原本的 `provider_id` CHECK（现在 `origin` 可查，row 可以属于 builtin 或 dynamic id）；`protocol ∈ ('chat_completions', 'messages')` 的 CHECK 保留到 v43。该 CHECK 是 v41 schema 中唯一引用 origin 概念的 provider_id 约束，因此不需要改其他表。已经迁到当前 schema 的库再次打开时，不会重新创建 `dynamic_providers` / `dynamic_provider_models`。schema v48 仅在这些遗留表存在且为空时删除它们。v47 源上的非空遗留会拒绝升级并保持 schema 47；当前 schema 的库不会擅自删除非空遗留行。

## Schema v43 — 首选协议 CHECK 与互斥单选修复

v43 重建 `provider_model_protocol_preferences`，使 `protocol` 可以是 `chat_completions`、`responses` 或 `messages`。随后删除 MiniMax/Kimi 上与另一条 Chat/Messages `force_on` 成对的 `force_off` 覆盖，恢复 Auto，以便两条 available 协议都能透传。Go 上对 unavailable 兄弟协议的 `force_off` 保留。V5 导入在内存中做同样的 exclusive-available 修复。不另写快照文件。回滚需恢复升级前的整个数据目录。

v42 **不**改 v35 的 Provider 单一身份契约：builtin 适配器路由、CPA 接入、Custom API 与 dynamic Configurable HTTP 绑定行为都保持原样。dynamic 读路径都加 `origin IN ('preset', 'custom')`，使 builtin 种子不会进入路由。V5 节点迁移负载只携带 dynamic 定义；builtin 行从注册表推导，导入时从 `preset_id` 推导 `origin` / `offering`，以保持跨版本兼容。

在非空 v41 库做 v42 重写前，进程会写入一份唯一、不覆盖的同目录快照：

```text
data.sqlite.pre-v42.<timestamp>.bak
data.sqlite.pre-v42.<timestamp>.bak.sha256
```

快照是独立的 v41 SQLite 文件（`VACUUM INTO`，两侧都做 `quick_check`）；sidecar 第一个字段是 `.bak` 的小写 SHA-256。全新空目录直接创建到当前 schema，不写这份副本。恢复前在数据目录内校验 sidecar：

```bash
sha256sum -c data.sqlite.pre-v42.<timestamp>.bak.sha256      # Linux
shasum -a 256 -c data.sqlite.pre-v42.<timestamp>.bak.sha256  # macOS
```

降级走既有的整目录恢复，没有向下迁移路径。

## Schema v41 — 模型协议选择

v41 为密封的 MiniMax CN 与 Kimi CN 范围添加 provider_model_protocol_preferences，独立保存 Chat/Messages 选择，不与按协议启停覆盖混用。迁移只新增表，不改变既有路由、Key 或账号日期。协议选择和覆盖在同一事务写入，恢复静态基线时清除选择。V5 迁移合约可携带可选 preferences 集合，未携带该字段的旧包仍可导入。回滚需恢复升级前的整个数据目录。（v42 重写会去掉该表上 `provider_id` 的 CHECK，`protocol` 的 CHECK 保留。）

## Schema v40 — 模型路由覆盖

Schema v40 为 `provider_models` 增加可空的 `upstream_override` JSON，保存模型显式协议与地址。空值继承原供应商默认配置，不改写账号凭据或现有路由。供应商替换与节点导入原子保存完整模型列表。V5 节点备份携带可选 `upstreamOverride`；没有该字段的旧备份继续继承默认值。旧读取器会拒绝未知字段，不会静默丢弃模型路由设置。降级应恢复升级前的完整数据目录备份。（v42 的重命名把表名改为 `provider_models`，列与语义不变。）

## Schema v31 — 按模型/按协议覆盖

v31 创建 `provider_contract_model_protocol_overrides` 表。每行对应一个合约范围 × 模型 × 协议，`state` 取值 `force_on` / `force_off`；无行即表示“自动”。复合主键为 `(scope_kind, scope_id, model_id, protocol)`。`provider_contract_scopes` 的开关列在 v48 之前仍保留在数据库中以保证向后兼容。effective 合约推导读取 `provider_contract_model_protocol_overrides`。

## Schema v32 — Custom 单协议完整 Endpoint

v32 用 `endpoint_url` 与单值 `upstream_protocol` 替换 `account_custom_configs.base_url`、JSON `upstream_protocols` 和 `auth_scheme`。历史行按 Chat Completions → Responses → Messages 选择协议，拼接对应标准推理后缀，并在同一事务中设为 disabled/pending、删除非所选协议的能力/证据/覆盖。管理员检查后必须显式重新启用迁移的 Custom 账号。

## Schema v35 — Provider 单一身份

v35 去掉 offering 维度。Provider 与 Plan 是同一产品身份，只按 `provider_id` 识别。已知 v34 对映射为 `opencode/go`、`opencode-zen-free/anonymous-free`、`command-code/goat`、`minimax/cn`、`kimi/cn`、`custom/api` 与 `cpa/local`。未知对与复合键冲突在任何写入前 fail closed。重建保留账号、密文字节、日志、定价/目录行、合约、Custom 配置/能力、设置与 access keys。同一 schema 版本还把类型化用户定义供应商存在 `dynamic_providers` 与 `dynamic_provider_models`（两者都在 v42 中改名为 `providers` / `provider_models`）。节点备份导出只含 `providerId` 的 payload V6，并带一份可选/默认空的用户定义供应商定义集合。payload V1–V3，以及除 4、5 或 6 以外的任何版本（包括未来的 V7 包），都会被明确的不支持版本错误拒绝。该 schema 当时的转移包导出 payload V6；HEAD 导出 payload V10。导入 V4/V5 时仍用确定性 1:1 映射重建身份附属行。

在非空 v34 库做破坏性 v35 重建之前，进程会写入一份唯一、不覆盖的同目录快照：

```text
data.sqlite.pre-v35.<timestamp>.bak
data.sqlite.pre-v35.<timestamp>.bak.sha256
```

快照是独立的 v34 SQLite 文件（`VACUUM INTO`，两侧都做 `quick_check`）；sidecar 第一个字段是 `.bak` 的小写 SHA-256。全新空目录直接创建到当前 schema，不写这份副本。恢复前在数据目录内校验 sidecar：

```bash
sha256sum -c data.sqlite.pre-v35.<timestamp>.bak.sha256      # Linux
shasum -a 256 -c data.sqlite.pre-v35.<timestamp>.bak.sha256  # macOS
```

## Schema v36 — Ollama Cloud 用量状态

v36 创建 `ollama_cloud_usage_state` 表。每个已配置账号一行，包含：

- `cookie_cipher` — 抓取 `https://ollama.com/settings` 用量页所用的混淆浏览器会话 Cookie。它使用与账号 Key 相同的混淆设施，明确不是 AEAD；任何 API 都不会返回它，导出载荷也不包含它。
- `status` — `unconfigured`、`ok`、`unauthorized` 或 `failed`。
- `snapshot` — 最近一次成功抓取的脱敏 JSON（5h/7d 窗口、按模型请求数、可选套餐/余额）。仅成功时写入；失败只更新状态列，不清空快照。
- `last_error`、`last_success_at`、`last_attempt_at`、`next_eligible_at`、`failure_streak` — 手动刷新 30 秒限速与最近一次尝试的元数据。

该行以 `account_id` 为键并 `ON DELETE CASCADE`，删除账号会带走用量状态；清除 Cookie 会删除该行并回到未配置。该迁移只做加法：现有表、行和路由事实保持不变。它不新增备份族。回滚仍是既有的整目录恢复。

## Schema v38 — 平台账号归属

v38 新增 `platform_accounts` 与 `platform_links`，保留既有账号 ID、Key、顺序、冷却、模型与日志。父账号地址不可变，关联指向既有 Custom API 账号；建立关联与生成端点在同一事务内完成。存在关联 Key 时禁止删除父账号，删除子账号会删除其关联。

新节点导出使用 V6 负载，不包含平台管理凭证、平台观察密钥和缓存观察值；仍支持 V4 与 V5 导入。导入关联保持未验证，同一父账号 ID 的平台类型或地址冲突会使整笔导入回滚。回滚沿用完整目录备份恢复，v38 不增加另一套备份机制。

## Schema v39 — 预设来源

v39 为用户定义供应商增加可空的 `preset_id`，在资源地址需要自填或名称修改后保留选择的配置模板。它不控制路由，也不是平台实例身份；既有行保持未分类。V5 迁移负载携带该可选字段，仍接受没有此字段的旧负载。

## Schema v37 — Ollama Cloud 计费档位

v37 删除 `ollama_cloud_usage_state`（未发布的 Cookie 抓取，包括混淆 Cookie 与上次成功快照），并创建 `ollama_cloud_billing`：

- `account_id` — 主键，`ON DELETE CASCADE`
- `billing_tier` — `pro` / `max` / `team`

没有行即未配置（账号字段为 `null`）。既有 Ollama 账号迁移后没有行，仍可路由，Key 与日志保持不变。新建必须选择付费档并填写 `accounts.purchase_date`。节点导出/导入携带计费档位。该迁移不新增备份族。回滚仍是既有的整目录恢复。

## Schema v33 — Custom 上游模型身份

v33 新增非空列 `account_model_capabilities.upstream_model`。历史行以 `model_id`
回填，完整保留原先“公开名称 = 上游 ID”的行为。新建 Custom 映射可保留不同的
公开模型名称与精确上游模型 ID；迁移不做后缀规范化，也不生成 Alias。

在任何 v27 写入前，既有（非空）库会得到一份唯一、不覆盖的同目录快照：

```text
data.sqlite.pre-v3.<timestamp>.bak
data.sqlite.pre-v3.<timestamp>.bak.sha256
```

快照是独立的 v26 SQLite 文件（`VACUUM INTO`，两侧都做 `quick_check`）；sidecar 第一个字段是 `.bak` 的小写 SHA-256。全新空目录直接创建到当前 schema，不写这份副本。快照只是回滚点，不能替代整目录备份。恢复前在数据目录内校验 sidecar：

```bash
sha256sum -c data.sqlite.pre-v3.<timestamp>.bak.sha256      # Linux
shasum -a 256 -c data.sqlite.pre-v3.<timestamp>.bak.sha256  # macOS
```

Windows 上用 `Get-FileHash -Algorithm SHA256` 与 sidecar 第一个字段比对。哈希不匹配时，该文件不可用于恢复。

## 回滚与失败的打开

**没有向下迁移。** 回滚是离线的精确文件恢复：

1. 停止所有打开该目录的进程。
2. 按上文校验 sidecar 哈希；不匹配就停止。
3. 把校验过的 `.bak` 复制覆盖 `data.sqlite`，并删除前一个活库留下的 `data.sqlite-wal` / `data.sqlite-shm`。
4. 用同一加密身份启动具备 v26 能力的二进制，或在恢复出的 v26 文件上重试 v27 升级。在 v27 成功打开之后再恢复会丢弃快照之后的全部写入。

失败的 v27 事务会回滚：活库必须仍是 schema 26 且 `sub_gateway_keys` 完好。已有的 pre-v3 文件留在原地；之后成功的 open 会再建一个唯一文件名，而不是覆盖第一份。错误或缺失的 Host cipher 会 fail closed，不会改写 `key_cipher` / `password_cipher`。`ocg-manager-cli status` 会打开数据库并尝试 v27，因此会执行迁移，而不是只读检查 schema。

---

[维护者指南索引](../MAINTAINER.zh-CN.md) · [English](storage-migration.md) · [文档索引](../README.zh-CN.md)
