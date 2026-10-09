[English](gateway.md)

# Gateway 行为

Open Console Gateway 在 `127.0.0.1:9042` 只暴露一个 HTTP 入口，同时讲四种客户端协议，并把请求转给 OpenCode Go、Zen Free、Command Code GOAT、MiniMax CN、Kimi Code CN、Ollama Cloud 或 Custom API 中胜出的合格账号卡。

Ollama Cloud 是可路由的密封固定源 Plan（`https://ollama.com`）：只走 Chat Completions，Bearer。已保存的目录模型会按归一化目录词干生成别名；精确 raw ID 仍可使用。实际上游 429 走通用冷却与回退。

## 端点

仪表盘从本地 Gateway 读取账号提醒和 token 图表摘要。30 天图表使用 UTC 日期，账号到期提醒遵循浏览器日历。刷新这些摘要不会联系供应商；账号读取失败时，不会显示所有账号正常。

Gateway 监听 `http://<bind>:<port>`，暴露以下端点：

| 方法 | 路径 | 用途 |
| --- | --- | --- |
| `POST` | `/v1/chat/completions` | OpenAI Chat Completions |
| `POST` | `/v1/responses` | OpenAI Responses |
| `POST` | `/v1/messages` | Anthropic Messages |
| `GET`  | `/v1/models` | 带鉴权的本地列表：当前合格的公开名称（代码持有的 Go 种子、从目录生成的 CN/GOAT/Ollama Alias、已保存的用户定义 Provider 公开模型，以及合格 Custom ID），每行都带与 enrich 同一快照推导并校验过的协议配置；**别名**页关闭的对外名称不会出现 |
| `POST` | `/v1beta/models/{model}:generateContent` | Gemini 非流式生成；`/v1/...` 同样可用 |
| `POST` | `/v1beta/models/{model}:streamGenerateContent` | Gemini SSE 生成；`/v1/...` 同样可用 |
| `POST` | `/v1beta/models/{model}:countTokens` | 返回 `501`，Gemini CLI 可回退到本地估算 |
| `POST` | `/v1beta/models/{model}:embedContent` | 返回 `501`；当前不支持 embeddings |
| `GET`  | `/dashboard/` | Vue 3 管理面板（HTML） |
| `*`    | `/dashboard/api/v3/...` | 410 墓碑（`dashboardV3Removed`） |
| `*`    | `/dashboard/api/v4/...` | 活的面板 JSON 控制面（CAS 变更、目的地、凭据、挂回的操作处理器、DSH） |
| `*`    | `/dashboard/api/...` | V2 REST 墓碑（已登录返回 410 `dashboardV2Removed`），不含已标明的 V2 鉴权与浏览器 WebSocket 兼容路由 |

默认监听 `127.0.0.1:9042`。CLI 可用 `serve --host 0.0.0.0` 覆盖监听地址，用 `serve --port <port>` 覆盖端口。桌面端同样绑定回环，并由 Tauri 单实例锁防止两个托盘程序争抢端口。没有 HTTP 健康检查端点；Docker 只检查容器内部的 TCP `9042`。

## 鉴权

Gateway API 必须携带 **Key**，支持 `Authorization: Bearer <key>`、`x-api-key: <key>` 或 `x-goog-api-key: <key>` 三种请求头。转发前 Gateway 会移除客户端鉴权头，再注入所选账号的凭据。OpenCode Go 在 Messages 上游使用 `x-api-key`，在 Chat Completions 与 Responses 上游使用 `Authorization: Bearer`。可配置 HTTP 按每条路由已保存的鉴权方式发送 Bearer、`x-api-key` 或 `api-key`。Gateway 不会转发 dashboard 或客户端凭据。

管理面板鉴权取决于监听地址。当前 SPA 使用 `/dashboard/api/v4/auth/status`、`/dashboard/api/v4/auth/register`、`/dashboard/api/v4/auth/login` 与 `/dashboard/api/v4/auth/logout`。注册、登录、退出需要与其他 V4 写入相同的 `expectedRevision` / `processGeneration` token。对应的 `/dashboard/api/auth/...` 路由是已标明的 V2 兼容路由，供缓存的旧页面使用。

- **回环监听（默认）**：Dashboard API 请求的 `Host` 必须为 `localhost` 或回环 IP 字面量；浏览器提供的 `Origin` 必须匹配该主机与端口。跨站请求会被拒绝，注册和登录也不例外。有效本地请求跳过面板登录；但只要带有 `Forwarded`、`x-forwarded-for`、`x-forwarded-proto`、`x-forwarded-host` 或 `x-real-ip` 中任一请求头，仍必须登录。使用公开主机名的反向代理必须连接非回环监听器。客户端还需要 **Key** 才能访问上游端点。桌面端与默认 CLI 都走这个分支。
- **非回环监听**：管理面板由唯一的 **管理员账号** 管控，密码以 Argon2 哈希存在 SQLite 中，登录后下发 HttpOnly 会话 Cookie。携带标准反向代理转发头但没有 Cookie 的请求仍需要登录。Docker 可以用 `OCG_ADMIN_USERNAME` 与 `OCG_ADMIN_PASSWORD` 引导首个管理员；不提供时由首位注册者创建。

## 别名

**别名**管理页从本地 Rust 核心分页读取已保存映射，搜索覆盖全部映射。同一公开名称的分组可能跨页，但映射总数、重叠提示、发布状态和已配置路由顺序始终按完整数据计算。展开能力或打开模型链接时按需读取对应明细。管理列表可以包含当前不符合 `/v1/models` 公布条件的已保存映射。

Custom API 使用已获注册表授权的共享别名时，会列入同一分组，即使内置映射已停用。冲突的原始上游 ID 仍受注册表的歧义检查约束。

客户端发送 **别名**：本地注册表中的稳定小写 kebab-case 名称。Go 种子继续由代码持有；新增 MiniMax CN、Kimi CN、GOAT 与 Ollama 别名从完整的已保存目录生成。已保存旧别名保持不变。Alias 拼写仍可大小写折叠，例如 `GLM-5.2`。

带鉴权的 `GET /v1/models` 按注册表顺序返回当前合格的公开名称：代码授权 Alias、从目录生成的 MiniMax CN/Kimi CN/GOAT Alias，再并入已保存的用户定义 Provider 公开模型，以及不与这些 Alias 冲突的合格 Custom 能力 ID（`owned_by` 为 `custom`）。合格条件与元数据 enrich 使用同一快照：目的地和模型已启用，名称解析到该映射，并且有一把已启用、ready、绑定已启用、范围允许该模型、在路由需要时持有 Key、并持有端点授权的凭据。仅目录启用、没有这组承载条件的名称不会出现。冷却、探测标记和鉴权错误历史不会把已有可用配置的行从列表里拿掉。**别名**页关闭的对外名称不会出现在该列表中，但仍可按名称调用。该列表使用已保存的本地状态。显式目录刷新更新已保存的供应商映射与合约。列表读取不会写转发日志。已保存的 Zen `-free` 行保留精确 raw pin 并公布去掉后缀后的 Alias；新发现的 CN/GOAT 行从完整已保存目录的最后一节生成唯一归一化别名，已保存旧别名继续按原样保留。保存的 Kimi 滚动名称保持不变，新发现的 `k3` 使用 `k3`。归一化后重名或占用 Go 原始名称的行不公布别名；每一行都保留精确 raw ID。合格 Custom ID 使用同一组合格快照（验证为可选）。每一行已公布的 `ocg` 对象都是 schemaVersion 2，并包含已校验的 `protocols.preferred` 与 `protocols.supported`。见[模型元数据](model-metadata.zh-CN.md)。

受保护的 `GET /dashboard/api/v4/application-models` 是另一份本地列表：已保存目录中可解析且协议已启用的 Go 名称。它不查阅价格快照。空列表返回 `[]`。该列表不含 Custom ID，并使用已保存的本地状态。

`/v1/models` 可以让 Zen、Command Code、MiniMax 或 Kimi 映射通过代码持有或目录生成的 Alias 对外供应，也可以公布密封映射中的供应商专属 Alias。Command 会去掉 Provider 命名空间，并从完整已保存目录的最后一个斜杠分段生成小写 kebab Alias；`-paid` / `-free` 只有在短 Alias 已获授权时才去掉，命名与目录行顺序无关。旧有的 `nvidia/nemotron-3-ultra-550b-a55b` 仍映射为 `nemotron-3-ultra`，有语义的变体不会按长度截断。归一化冲突以及占用 Go raw-only 名称的别名不公布；Ollama 同词干有多个标签时使用含完整标签的归一化别名，最多一个管理员钉定标签拥有短词干。每一行仍保留精确 raw ID。同一供应商内，已保存的自定义或兼容名称保留原目标；新增行生成的名称与它冲突时，保留精确原始 ID，并需设置不同的公开名称。只有精确保存目录行仍存在，且至少一个供应商 mapping 仍有已启用协议时才公布该 Alias；进入 `/v1/models` 的行仍需要上述合格承载配置，仅目录启用不够。合格 Custom 声明 ID 即使含 `/` 也可以出现；它们不会折成 kebab 别名。`application-models` 仍是更窄的 Go 列表，不查阅价格快照。

原始上游 ID 在注册表中恰好对应一个 mapping 时，会钉在该 mapping 上——不跨 Plan 回退，也不做 Zen prefer 覆盖——然后才检查可路由性。内置 raw ID 严格区分大小写。生成的别名使用归一化后的最后一个斜杠分段；即使同时有别名，含 `/`、`_` 或空白的 raw ID 仍是精确 pin。Custom 能力 ID 保持原有的大小写折叠匹配。精确 raw ID 映射到多个 Plan 时（含合格 Custom 能力与另一 Plan）返回 `400`，错误码 `ambiguous_model_id`，且不会调用上游。未知名称——既非授权 Alias、精确保存的内置 raw ID，也非合格 Custom ID——在所有受支持的客户端格式上返回 `400`：Chat Completions、Responses、Messages，以及 Gemini `generateContent` / `streamGenerateContent`。canonical kebab 别名 `deepseek-v4-flash` 在完整已保存目录授权时可以在已启用的 Go、Zen 与 Command Code mapping 中选择；唯一原始 ID `deepseek/deepseek-v4-flash` 只钉在 Command Code。Zen 的 `foo-free` 始终保留精确 raw pin，并按 `-free` 后缀公布 Alias `foo`。

转发日志把请求身份与上游身份分开记录：

- `requested_model` — 客户端发送的公开名称或 Alias
- `resolved_alias` — 存在时解析出的公开 Alias
- `upstream_model` — 实际发送到该账号上游的精确模型 ID

以及 `provider_id`。新请求不记录价格。没有记录到的费用保持未知，不会显示成零或免费。旧日志只保留当时已经记下的费用，不会重算。

---

[用户指南索引](../USER.zh-CN.md) · [English](gateway.md) · [文档索引](../README.zh-CN.md)
