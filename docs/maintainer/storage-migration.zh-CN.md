[English](storage-migration.md)

# 存储与迁移

本页是升级、备份与回滚的运维约定。schema 细节见 [持久化](state-and-lifecycle.zh-CN.md#持久化)。

## 数据目录与加密身份

每次打开数据库都使用 Host 解析的 cipher（CLI、桌面、Docker 均为 `Database::open_with_cipher`）。迁移前会检查已有账号密文，解密错误会 fail closed。Key 存储使用非认证混淆，成功解码为 UTF-8 本身不能证明 cipher 身份正确。请保留原 cipher；改写密文无法修复不匹配。

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

`CURRENT_SCHEMA_VERSION = 45`（`crates/ocg-core/src/db.rs`）。打开历史库会先规范迁移到 v26，再由 v27 重写把主 Key 与全部 `sub_gateway_keys` 行复制进一张 `access_keys` 表（主 Key 固定 id `00000000-0000-0000-0000-000000000001`），删除 `sub_gateway_keys`，并删除 `accounts` 上遗留的五列 `usage_sync_*`（用量同步元数据在 `provider_usage_sync_state`）。v33 新增 Custom 精确上游模型身份；v34 新增 CPA 单例配置表，但不会导入或导出 CPA 状态。v35 把 Provider/Plan 身份收成只有 `provider_id`：先预检每一个已知的 v34 provider/offering 对，未知对与会丢数据的复合键冲突在写入前失败，再重建受影响的表，使 offering 列不存在。v36 增量创建过 `ollama_cloud_usage_state`（未发布的 Cookie 用量抓取）。v37 删除该表且不动账号 Key 与日志，并创建 `ollama_cloud_billing`。v42 把类型化用户定义 Provider 表与密封 Adapter 种子目录统一：把 `dynamic_providers` / `dynamic_provider_models` 重命名为 `providers` / `provider_models`，新增 `origin`（`builtin` | `preset` | `custom`）、`adapter_kind`、`offering`（`plan` | `api`）与 `endpoint_per_account` 列，把七个密封 builtin 适配器（OpenCode Go、Zen Free、Command Code GOAT、MiniMax CN、Kimi CN、Ollama Cloud、Custom API——但不含静态外部接入 CPA）以 `builtin` 行种入表中，这些行的属性列只是展示镜像，并在 dynamic 读路径上加 `origin` 过滤。v41 的 `provider_model_protocol_preferences` 表上 `provider_id` CHECK 已被去掉（`protocol ∈ ('chat_completions', 'messages')` 的 CHECK 保留）。账号 `key_cipher` / `password_cipher` 用 Host cipher 就地校验，**不会重新加密**。v44 增量创建 `dashboard_operations`，供 V4 幂等提交使用，不另写迁移前备份（与 v43 相同）。v45 增量创建身份/凭据/绑定附属表与 `accounts.identity_id`，不另写迁移前备份（与 v43/v44 相同）。

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
- `quota_pool_members` — `pool_id`、`account_id`（回填时每个身份一名成员；该身份上的第二份凭据加入同一池）

附属行在同一事务中显式删除（DDL 声明了 `ON DELETE CASCADE`，但进程未启用 foreign-key pragma）。打开数据库时，v45 一致性检查用幂等回填补齐缺失的附属行；若仍不一致则 fail closed。

迁移规则：每个既有账号恰好对应一个身份（`label` = 账号名，置信度 `opaque`）、一份凭据（`version` 1），以及一条绑定到该账号 connection 的记录（内置供应商 / 动态供应商 / Custom 账号自己的 connection），`model_scope=all`，`enabled` 等于账号启用状态。路由排序读取既有 `accounts.sort_order`，不另存一份。`legacy_identity_map` 记录账号 → 身份 / 凭据 / 绑定。尚未 `ready` 的托管账号写入一条 `onboarding_tasks`，状态 `in_progress`、步骤为当前步；已 ready 的托管账号不编造历史。只有已经公布购买/到期日的密封内置 Provider 账号才写入 `subscription_records`，`source` 为 `legacy_manual`。用户定义与 Custom API 账号不写：日期保持未知，不以零定价（D07）。平台关联：被关联 Key 的身份变为 `declared`，`authority_site` = 父账号 `base_url`；每个平台父账号自有身份，并带一份 `platform_observer` 凭据（管理凭据，从不用于推理）。父账号与被关联 Key 永不合并；关系保持已声明、未验证（D04）。冷却列不搬迁：投影为额度窗口（generic / 5h / week / month → subject `credential`；free → subject `egress` `free_channel`，declared、authoritative），精确保留已存时刻。未知指标为 `null`，绝不为零。迁移为每个身份创建一个额度池（`subject` 为 credential / 身份 id，`relation_confidence` 为 unknown，`policy_mode` 为 authoritative_limit），从不写 `verified`。之后该身份上的第二份凭据加入同一池并把关系标为 `declared`。路由遵守已存的 `model_scope` 与绑定 `enabled`。

每一次账号插入（V3 创建、托管创建、用户定义供应商首把 Key、V4 onboarding commit、节点导入）都通过与本迁移共用的唯一映射器，在同一事务写入附属行。平台关联 / 解除关联在同一事务更新被关联身份的置信度与站点。

轮换、绑定编辑与第二份凭据写入是建立在这些附属表上的 V4 CAS 路径。迁移负载仍为 V5。本切片不改账号页 UI。

## Schema v44 — 面板操作记录

v44 增量创建 `dashboard_operations`，供 V4 幂等控制面提交使用：

- `operation_id` — 主键
- `kind`
- `payload_digest` — 对语义载荷（`operationId`、`connection`、含密钥的 `authorization`、`targets`）的 hex HMAC-SHA256；CAS 令牌不参与
- `result_json` — 已存的无密钥结果
- `created_at`

摘要密钥是每库一份的随机 32 字节，惰性写入 `settings` 的 `dashboard_operation_digest_key`，任何 API 都不会返回它。该密钥与混淆后的账号 Key 同库存放，因此沿用既有本地存储威胁模型；它避免把已存摘要做成密钥的无键哈希，并不能防御持有数据库文件的攻击者。插入时会清理超过 30 天的行。该迁移只做加法，不另写迁移前备份（与 v43 相同）。回滚仍是既有的整目录恢复。

## Schema v42 — 统一的供应商表

v42 把 `dynamic_providers` / `dynamic_provider_models` 重命名为 `providers` / `provider_models`，并为 `providers` 新增四列：

- `origin` —— `builtin` | `preset` | `custom`。builtin 行是七个密封 Adapter 种子（OpenCode Go、Zen Free、Command Code GOAT、MiniMax CN、Kimi CN、Ollama Cloud、Custom API）的展示镜像；CPA 是静态外部接入，**不**进表。preset 行跟随 preset 派生的 dynamic 供应商，custom 行跟随手工创建的 dynamic 供应商。
- `adapter_kind` —— builtin 行镜像密封 `ProviderAdapterKind`；每条 dynamic 行的值都是 `configurable_http`。
- `offering` —— `plan` | `api`。builtin 行从 `ocg_domain::provider::builtin_offering(provider_id)` 取；dynamic 行通过 `ocg_domain::provider::preset_offering(preset_id)` 从 `preset_id` 推导（仅 custom 的行默认为 `api`）。
- `endpoint_per_account` —— builtin 行除 Custom API（值为 `1`）外都为 `0`；dynamic 行一律 `0`。

v41 的 `provider_model_protocol_preferences` 表被重建，去掉了它原本的 `provider_id` CHECK（现在 `origin` 可查，row 可以属于 builtin 或 dynamic id）；`protocol ∈ ('chat_completions', 'messages')` 的 CHECK 保留到 v43。该 CHECK 是 v41 schema 中唯一引用 origin 概念的 provider_id 约束，因此不需要改其他表。

## Schema v43 — 首选协议 CHECK 与互斥单选修复

v43 重建 `provider_model_protocol_preferences`，使 `protocol` 可以是 `chat_completions`、`responses` 或 `messages`。随后删除 MiniMax/Kimi 上与另一条 Chat/Messages `force_on` 成对的 `force_off` 覆盖，恢复 Auto，以便两条 available 协议都能透传。Go 上对 unavailable 兄弟协议的 `force_off` 保留。V5 导入在内存中做同样的 exclusive-available 修复。不另写快照文件。回滚需恢复升级前的整个数据目录。

v42 **不**改 v35 的 Provider 单一身份契约：builtin 适配器路由、CPA 接入、Custom API 与 dynamic Configurable HTTP 绑定行为都保持原样。dynamic 读路径都加 `origin IN ('preset', 'custom')`，使 builtin 种子不会进入路由。V5 节点迁移负载仍只携带 dynamic 定义；builtin 行从注册表推导，导入时从 `preset_id` 推导 `origin` / `offering`，以保持跨版本兼容。

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

v31 创建 `provider_contract_model_protocol_overrides` 表。每行对应一个合约范围 × 模型 × 协议，`state` 取值 `force_on` / `force_off`；无行即表示“自动”。复合主键为 `(scope_kind, scope_id, model_id, protocol)`。`provider_contract_scopes` 的开关列仍保留在数据库中以保证向后兼容。effective 合约推导读取 `provider_contract_model_protocol_overrides`。

## Schema v32 — Custom 单协议完整 Endpoint

v32 用 `endpoint_url` 与单值 `upstream_protocol` 替换 `account_custom_configs.base_url`、JSON `upstream_protocols` 和 `auth_scheme`。历史行按 Chat Completions → Responses → Messages 选择协议，拼接对应标准推理后缀，并在同一事务中设为 disabled/pending、删除非所选协议的能力/证据/覆盖。管理员检查后必须显式重新启用迁移的 Custom 账号。

## Schema v35 — Provider 单一身份

v35 去掉 offering 维度。Provider 与 Plan 是同一产品身份，只按 `provider_id` 识别。已知 v34 对映射为 `opencode/go`、`opencode-zen-free/anonymous-free`、`command-code/goat`、`minimax/cn`、`kimi/cn`、`custom/api` 与 `cpa/local`。未知对与复合键冲突在任何写入前 fail closed。重建保留账号、密文字节、日志、定价/目录行、合约、Custom 配置/能力、设置与 access keys。同一 schema 版本还把类型化用户定义供应商存在 `dynamic_providers` 与 `dynamic_provider_models`（两者都在 v42 中改名为 `providers` / `provider_models`）。节点备份导出只含 `providerId` 的 payload V4，并带一份可选/默认空的用户定义供应商定义集合。payload V1–V3，以及除 4 或 5 以外的任何版本（包括未来的 V6 包），都会被明确的不支持版本错误拒绝。本二进制仍导出 payload V5。

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

新节点导出使用 V5 负载，不包含平台管理凭证和缓存观察值；仍支持 V4 导入。导入关联保持未验证，同一父账号 ID 的平台类型或地址冲突会使整笔导入回滚。回滚沿用完整目录备份恢复，v38 不增加另一套备份机制。

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

## Schema v39 — 预设来源

v39 为用户定义供应商增加可空的 `preset_id`，在资源地址需要自填或名称修改后保留选择的配置模板。它不控制路由，也不是平台实例身份；既有行保持未分类。V5 迁移负载携带该可选字段，仍接受没有此字段的旧负载。
