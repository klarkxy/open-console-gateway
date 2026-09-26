[English](state-and-lifecycle.md)

# 状态、凭据与生命周期

## 状态、凭据与设置

`CoreStateInner`（`state.rs`）由 Gateway、面板与 CLI 共享。

锁顺序：(1) `settings_update`，(2) `db`，(3) `config`，(4) `http_client`， (5) `gateway`，(6) `pricing`，(7) `zen_free_models`，(8) `provider_contracts`，(9) `routing`，(10) `credential_snapshot`。反向获取会造成死锁；持有 `routing` 锁时不应执行 DB 或网络 I/O。异步闸口：设置写同时重绑时， `settings_host_effects`（持久化 → 监听器重绑 → 补偿）先于 `gateway_lifecycle`。这些 await 期间应释放 `parking_lot` 锁。

访问 Key 的权威表是 `access_keys`。两层凭证共用该表（当前 schema 版本，见[存储与迁移](storage-migration.zh-CN.md)）和一份鉴权快照：

- 主 Key：固定 id `00000000-0000-0000-0000-000000000001`，显示名 `"Primary"`。始终启用，没有删除入口。公开 `AppConfig` 与面板 API 暴露 `gateway_key`；经消毒的 config JSON 把 `gateway_key` 存为 `""`。
- 子 Key：非主行，活跃上限 64，软删保留身份/名称并清除明文。只经 `/dashboard/api/v4/keys*` 生命周期 API 变更。CLI 没有子 Key 命令。

主/子 Key 值互斥由 `gateway_keys::ensure_primary_value_allowed` 在 dashboard、settings 与子 Key 启用路径强制。

`AppConfig` 使用 serde 默认值做向后兼容加载。历史未知字段在读取时被忽略，并在下次规范 settings 写回时省略。下游访问根地址优先级：非空 `OCG_CLIENT_ROOT_URL`（只读，不会写回 SQLite）> SQLite 手工值 > 前端按生产 origin / 开发 Gateway 端口自动推导。

**回环监听时** 直接访问跳过登录。带标准反向代理转发头但没 Cookie 的请求仍需登录。**非回环监听** 走单管理员模型：密码以 Argon2 哈希存 SQLite，登录下发 HttpOnly 会话 Cookie。Docker 用 **同时设置的** `OCG_ADMIN_USERNAME` 与 `OCG_ADMIN_PASSWORD` 引导首个管理员；只设一个会启动失败；不提供时由首位注册者创建。

设置页通过 `GET /dashboard/api/v4/settings/check-update` 获取 GitHub Release 元数据。支持升级的已安装桌面运行时可下载、校验签名并安装；开发构建、CLI、Docker 只保留元数据/发布页路径。出站请求只在用户点击时发起。

## 账号生命周期与浏览器运行时

`credentials` 带 `account_type`（`key | managed`）与 `setup_step` （`google_account → opencode_registration → payment → key_verification → ready`）。非托管行为 `key + ready`。遗留 `accounts` 表在 v52 之后已删除。托管草稿立即持久化为空 Key、`enabled=false`；选择器、启用接口和路由都必须同时要求 `ready` 与非空 Key。步骤名 `google_account` 在 UI 上展示为「登录身份」，可跳过。

`AppConfig::default()` 的 `opencode_invite_url` 带演示默认值（`DEFAULT_OPENCODE_INVITE_URL`）。规范化后只接受最长 2048 字符、无用户名密码的 HTTPS URL，主机严格限定为 `opencode.ai` 或 `console.opencode.ai`。面板在 OpenCode Go 供应商的 **设置** 页签编辑该值。创建托管草稿时可编辑邀请链接；与已保存值不同时写回 SQLite。注册/支付/验证码仍由用户在浏览器中完成，Key 由用户复制回填；Open Console Gateway 不会使用 CDP 自动填表或代点支付。

托管状态允许 **向前一步** 或 **回退到任意更早的未完成步骤**。普通 setup PATCH 只写这些步骤变更；独立的 Key 验证请求写入 `ready`。Key 实测返回 `2xx` 时进入 `ready + enabled`；`429` 同样证明 Key 有效并写入冷却；其他 HTTP 响应——包括重定向、`429` 以外的 `4xx` 与 `5xx`——以及网络或超时错误都保持 `key_verification`。

### 托管账号 setup 生命周期

[![托管账号 setup 生命周期](../diagrams/managed-account-lifecycle.visual-check.1440x900.light.png)](https://klarkxy.github.io/open-console-gateway/diagrams/managed-account-lifecycle/)

[在 GitHub Pages 打开交互式流程图](https://klarkxy.github.io/open-console-gateway/diagrams/managed-account-lifecycle/)。

普通 setup PATCH 只能向前一步，或回到更早的未完成步骤；它不会写入 `ready`。独立的 Key 验证请求在收到 `2xx` 或 `429` 时将账号置为 `ready + enabled`。Key 无效等 `4xx` 与重定向让草稿保持 pending 并返回 `400`；网络、超时与 `5xx` 同样保持 pending，但返回 `502`，用户可重试或回退步骤。

官方 Go usage（`go_usage.rs`，`https://opencode.ai/zen/go/v1/usage`）是校准基线，由 `usage_sync.rs` 协调。手动 `POST /dashboard/api/v4/accounts/{id}/usage/refresh` 与后台对账共用同一条 fetch + key CAS + 三窗口校准路径。

ready+enabled 且近 24h 有本地活动的账号约每小时对账，无活动约每天；禁用、非 ready、空 Key 排除。启动时避免轰鸣：全局并发 1、节奏控制、有界抖动，并提供可注入 clock/jitter/fetch 缝。

手动刷新在任何尝试后按账号 15s 节流、并发去重，并遵守 Retry-After / `nextAllowedAt`。本地最大 Go 用量 ≥80% 时最多每 15 分钟加速一次。

真实推理 `429` 仍写现有 cooldown/selector，并额外调度约 1–2 分钟后的官方同步（非 inline）。官方失败或 `status=rate-limited` 不会写推理冷却。成功后按最早 `resetsAt`（有界抖动）调度，同时尊重活跃/非活跃节奏。失败退避：5m → 15m → 1h → 6h；last-success 与上次基线不会被清除。

sync 元数据在 `provider_usage_sync_state`。公开 Go docs 尚未列出该路径。

Zen Free 由数据库持有：可启用、停用、排序，但不能通过通用账号 API 创建或删除。Command Code 账号在 enabled、ready 且 Key 非空时可路由；供应商矩阵控制模型供应，GOAT 预设行默认开启，额外行默认关闭。Custom 声明协议后即可路由；验证为可选。

浏览器：`GET /dashboard/api/v4/browser/capabilities`、 `POST /accounts/{id}/browser`、`DELETE /accounts/{id}/browser-profile` 与 `/browser/sessions/{token}/ws`。浏览目标允许 Google 注册/登录、GitHub 注册/ 登录、配置的邀请 URL 与 OpenCode 控制台（`https://opencode.ai/auth`）。 worker 主机白名单含 `accounts.google.com`、`github.com`、`opencode.ai`、 `console.opencode.ai`、`auth.opencode.ai`。远程会话令牌只在内存中保存，绑定管理员会话并检查 Origin，空闲 30 分钟或总计 4 小时失效。

桌面原生浏览器 hook 由 `src-tauri/src/host/` 注册进 `CoreState`。Vue 仍通过 HTTP 调用。Windows 依次查 Edge、Chrome；macOS 查 Chrome、Edge、Chromium； Linux 从 `PATH` 查 Chrome/Chromium/Edge。外部浏览器使用 `browser-profiles/<account_id>`、`--no-first-run`、 `--no-default-browser-check` 与新窗口，启动参数中不包含 CDP、automation、`--no-sandbox` 或关闭 Web 安全的选项。

`crates/ocg-browser-worker` 每节点只保留一个 Chromium。切换账号先 SIGTERM 当前进程组并等待 Profile 写盘，超时才强制结束。

Sidecar 以 UID/GID 10001、只读根文件系统、零 capability 运行；控制 token 由共享运行时卷随机生成。Chromium 需要建立自身的 user/PID/network namespace 和 renderer seccomp 沙箱，因此 browser 服务使用 `seccomp=unconfined` 且不能启用 `no-new-privileges`。

Sidecar 仍不挂载 SQLite，不发布宿主机端口。浏览器项目桥接网络不能设为 Docker `internal`，因为 Chromium 需要访问 Google/OpenCode 的 HTTPS 出站网络。

Profile 删除先停浏览器，校验账号 ID 防目录穿越，再把新旧 Profile 原子改名暂存；数据库提交成功后清理暂存目录，失败则恢复。重置完成账号不删除 Key；重置注册中账号回到 `google_account`。删除账号的 UI 确认必须说明 Cookie 与 Profile 会一并删除。

## 持久化

`crates/ocg-core/src/db.rs` 定义 SQLite schema、迁移与查询。当前 schema 版本与沿革见 [storage-migration.zh-CN.md](storage-migration.zh-CN.md)。v60 增量保存本机按 Key 额度恢复运行时列 `credentials.quota_recovery_json`，不进入可移植导出。重启保留等待与退避，不保留探测租约。`provider_contracts.rs` 负责供应商合约范围、按模型/按协议覆盖、effective 合约推导与模型协议证据。`models.rs` 定义共享 serde 类型和 `AppConfig`。本机 Key 存放在 `ocg-infra::crypto`（门面 `ocg_core::crypto`）：AES-256-GCM `v2:` 密文，不是 KMS。旧 XOR 仍可解密；正确的 `open_with_cipher` 会在同一事务里把剩余账号 `key_cipher` / `password_cipher` 改写成 v2。Windows 桌面使用 `MachineBoundCipher`；CLI/Docker 使用来自 `OCG_MANAGER_ENCRYPTION_KEY` 或 `<data-dir>/.encryption-key` 的 `StaticKeyCipher`。生产宿主必须调用 `Database::open_with_cipher`，让密文探测使用已经解析的 cipher。比本构建支持的更新 schema 会 fail closed。

升级路径上的历史版本：

- v16：托管 setup 列。
- v21：usage-sync 元数据（现在在 `provider_usage_sync_state`）。
- v22：不可变 provider/offering 绑定、供应商价格/用量、额度窗口、供应商感知转发日志。
- v23：Plan 验证状态、别名 / 上游日志身份、可选原生成本、Custom 配置表。
- v24：转发日志新增实际代理路由段（`auto` / `proxy` / `direct`；历史空串= 未记录）。
- v25：`provider_model_catalogs`（Zen Free 最后一次成功快照）。
- v26：`provider_contract_scopes` 与 `provider_contract_model_protocols`。加法迁移。
- **v27：** 把主 `gateway_key` 与 `sub_gateway_keys` 复制进 `access_keys`；删除 `sub_gateway_keys`；删除遗留 `accounts.usage_sync_*`。数据库到达规范 v26 后，既有（非空）库会在 **任何 v27 写入前** 得到唯一同目录副本 `data.sqlite.pre-v3.<UTC>.bak` 及其 SHA-256 sidecar。全新空目录直接创建 v27，不写这份副本。操作恢复见 [storage-migration.zh-CN.md](storage-migration.zh-CN.md)。
- **v29：** 从目录中移除 SCNet Token Plans，并在迁移期间删除所有现有 SCNet 账号行。
- **v30：** 将 `account_custom_configs.upstream_protocol` 回填为 JSON `upstream_protocols` 集合（1–3 个 chat_completions / responses / messages）；Custom 配置/能力编辑保持账号启用，但将 `verification_status` 重置为 `pending`。
- **v31：** 新增 `provider_contract_model_protocol_overrides` 表以支持按模型/按协议启用，并停止读取已弃用的 `provider_contract_scopes` 开关列。
- **v32：** Custom API 由 `base_url`、协议集合 JSON 与可配置鉴权收敛为一个完整 `endpoint_url` 和一个 `upstream_protocol`。历史 Custom 行按 Chat Completions → Responses → Messages 选择协议，并置为 disabled/pending 供管理员复核；非所选协议状态在同一迁移事务中移除。
- **v33：** 新增非空 `account_model_capabilities.upstream_model`，由 `model_id` 回填。
- **v34：** 新增 CPA 接入单例配置。
- **v35：** 经 fail-closed 预检与重建后，将 Provider 与 Plan 身份收敛为 `provider_id`；同时持久化类型化用户定义 Provider。pre-v35 备份与回滚流程见[存储与迁移](storage-migration.zh-CN.md)。
- **v36：** 增量创建 `ollama_cloud_usage_state`，用于未发布的 Cookie 用量抓取。
- **v37：** 删除 `ollama_cloud_usage_state` 且不动账号 Key 与日志，并创建 `ollama_cloud_billing`。

GUI 数据目录：Windows `%USERPROFILE%\.ocg-mgr` 或 macOS/Linux `~/.ocg-mgr`。 CLI 默认 `~/.ocg-mgr-cli`。Docker 将 SQLite、Key 与 `.encryption-key` 放在 `ocg-data`，长期 Cookie 与浏览器状态放在 `ocg-browser-profiles`。两卷都是高敏感持久状态，必须在服务停止后成对备份；`ocg-browser-runtime` 只含运行时控制 token，不应加入备份。浏览器 Profile 不由 Open Console Gateway 加密。

转发日志插入走 `ocg-infra::sqlite_logs`（每个辅助恰好一条显式语句）。调用方拥有时间戳、诊断、费用策略、脱敏与事务。

## 节点边界

每个节点由自己的面板独立管理。

## 生命周期类别

这四类必须分开。一类不能用于取消另一类。

| 类别 | 启动 | 停止 | 说明 |
| --- | --- | --- | --- |
| **Gateway 监听器**（`GatewayLifecycle`） | `start_gateway` / `bind` | `stop`（只发信号）或 `stop_and_wait`（CLI） | TCP 绑定、面板信任、转发日志回填、HTTP 服务。重绑感知槽位（同端口先停后绑，新端口先绑）。不启动也不取消进程级 worker。 |
| **控制面 worker**（`ControlPlaneWorkers`） | 由 `start_gateway` 调用 `ensure_started`（每个 `CoreState` 一次） | 无 —— 拥有该 `CoreState` 被 drop 时退出 | 官方用量对账。没有公开 cancel API。监听器停止不会杀死它。 |
| **桌面能力** | Tauri setup：自启（Windows x64 / macOS / Linux x64 release/已安装）、Dock（macOS）、升级 starter | 进程退出 | Host 注册的 capability。CLI/Docker 不注册 hook。HTTP 设置表单仍按能力门控 `auto_start` 与 `show_dock_icon`。 |
| **浏览器运行时** | 桌面原生 hook；Docker 远程 worker | 账号切换 / Profile 重置 / 进程退出 | 原生浏览器与 Sidecar 是同一 `BrowserRuntime` 槽的不同宿主。 |

Tauri `src/lib.rs`：启动用 `start_gateway`（监听器 + 用量 worker）；退出用 `host::gateway::stop_listener`（只停监听器）。设置端口变更经 `GatewayLifecycle` / `settings_host_effects` 重绑，并用配置指纹做补偿；并发失败的端口写入不会覆盖成功的超时写入。

升级器注册为 `CoreState` starter。`src-tauri/capabilities/default.json` 没有 updater 权限。升级器出站遵循进程级 **默认段** 代理策略（含 List 模式）。

---

[维护者指南索引](../MAINTAINER.zh-CN.md) · [English](state-and-lifecycle.md) · [文档索引](../README.zh-CN.md)
