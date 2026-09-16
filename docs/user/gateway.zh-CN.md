[English](gateway.md)

# Gateway 行为

Open Console Gateway 在 `127.0.0.1:9042` 只暴露一个 HTTP 入口，同时讲四种客户端协议，并把请求转给 OpenCode Go、Zen Free、Command Code GOAT、MiniMax CN、Kimi Code CN、Ollama Cloud 或 Custom API 中胜出的合格账号卡。

Ollama Cloud 是可路由的密封固定源 Plan（`https://ollama.com`）：只走 Chat Completions，Bearer。已保存或原始目录 ID 不会进入 `GET /v1/models`，也不会加入 Go Alias 注册表。实际上游 429 走通用冷却与回退。

## 端点

Gateway 监听 `http://<bind>:<port>`，暴露以下端点：

| 方法 | 路径 | 用途 |
| --- | --- | --- |
| `POST` | `/v1/chat/completions` | OpenAI Chat Completions |
| `POST` | `/v1/responses` | OpenAI Responses |
| `POST` | `/v1/messages` | Anthropic Messages |
| `GET`  | `/v1/models` | 带鉴权的本地列表：代码持有且当前可路由的 Go 与密封 CN Alias、已保存的用户定义 Provider 公开模型，以及当前有有效启用协议的合格 Custom ID；**别名**页关闭的对外名称不会出现 |
| `POST` | `/v1beta/models/{model}:generateContent` | Gemini 非流式生成；`/v1/...` 同样可用 |
| `POST` | `/v1beta/models/{model}:streamGenerateContent` | Gemini SSE 生成；`/v1/...` 同样可用 |
| `POST` | `/v1beta/models/{model}:countTokens` | 返回 `501`，Gemini CLI 可回退到本地估算 |
| `POST` | `/v1beta/models/{model}:embedContent` | 返回 `501`；当前不支持 embeddings |
| `GET`  | `/dashboard/` | Vue 3 管理面板（HTML） |
| `*`    | `/dashboard/api/v3/...` | 冻结的面板 JSON 控制面（CAS 变更）；同时承载面板鉴权与浏览器 WebSocket |
| `*`    | `/dashboard/api/v4/...` | 与 V3 并行的仅增量面板 JSON 控制面（onboarding、凭据/绑定写入、DSH 安装） |
| `*`    | `/dashboard/api/...` | V2 REST 墓碑（已登录返回 410 `dashboardV2Removed`），不含已标明的 V2 鉴权与浏览器 WebSocket 兼容路由 |

默认监听 `127.0.0.1:9042`。CLI 可用 `serve --host 0.0.0.0` 覆盖监听地址，用 `serve --port <port>` 覆盖端口。桌面端同样绑定回环，并由 Tauri 单实例锁防止两个托盘程序争抢端口。没有 HTTP 健康检查端点；Docker 只检查容器内部的 TCP `9042`。

## 鉴权

Gateway API 必须携带 **Key**，支持 `Authorization: Bearer <key>`、`x-api-key: <key>` 或 `x-goog-api-key: <key>` 三种请求头。转发前 Gateway 会移除客户端鉴权头，再注入所选账号的凭据。OpenCode Go 在 Messages 上游使用 `x-api-key`，在 Chat Completions 与 Responses 上游使用 `Authorization: Bearer`。Custom API 由唯一上游协议决定鉴权：Messages 只使用 `x-api-key`，Chat Completions 与 Responses 只使用 Bearer。Gateway 不会转发 dashboard 或客户端凭据。

管理面板鉴权取决于监听地址。当前 SPA 使用 `/dashboard/api/v3/auth/status`、`/dashboard/api/v3/auth/register`、`/dashboard/api/v3/auth/login` 与 `/dashboard/api/v3/auth/logout`。注册、登录、退出需要与其他 V3 写入相同的 `expectedRevision` / `processGeneration` token。对应的 `/dashboard/api/auth/...` 路由是已标明的 V2 兼容路由，供缓存的旧页面使用。

- **回环监听（默认）**：Dashboard API 请求的 `Host` 必须为 `localhost` 或回环 IP 字面量；浏览器提供的 `Origin` 必须匹配该主机与端口。跨站请求会被拒绝，注册和登录也不例外。有效本地请求跳过面板登录；但只要带有 `Forwarded`、`x-forwarded-for`、`x-forwarded-proto`、`x-forwarded-host` 或 `x-real-ip` 中任一请求头，仍必须登录。使用公开主机名的反向代理必须连接非回环监听器。客户端还需要 **Key** 才能访问上游端点。桌面端与默认 CLI 都走这个分支。
- **非回环监听**：管理面板由唯一的 **管理员账号** 管控，密码以 Argon2 哈希存在 SQLite 中，登录后下发 HttpOnly 会话 Cookie。携带标准反向代理转发头但没有 Cookie 的请求仍需要登录。Docker 可以用 `OCG_ADMIN_USERNAME` 与 `OCG_ADMIN_PASSWORD` 引导首个管理员；不提供时由首位注册者创建。

## 别名

客户端发送 **别名**：本地注册表中的稳定小写 kebab-case 名称。内置 Alias 权威由代码持有：OpenCode Go 静态协议表加上精确密封的 MiniMax CN、Kimi CN 与选定 GOAT 长名称映射。Alias 拼写仍可大小写折叠，例如 `GLM-5.2`。

带鉴权的 `GET /v1/models` 先按注册表顺序列出当前可路由的代码授权 Alias，再并入已保存的用户定义 Provider 公开模型，以及不与这些 Alias 冲突、同样有有效启用协议的合格 Custom 能力 ID（`owned_by` 为 `custom`）。**别名**页关闭的对外名称不会出现在该列表中，但仍可按名称调用。该列表使用已保存的本地状态。显式目录刷新更新已保存的供应商映射与合约。列表读取不会写转发日志。已保存的 Zen `-free` 行保留精确 raw pin 并公布去掉后缀后的 Alias；Command 模型可以加入任一代码持有的 Alias；MiniMax/Kimi 模型只激活精确密封的 CN 映射；未来未知的 Command/MiniMax/Kimi 行不能动态创建任意 Alias。合格 Custom ID 来自 enabled + ready 且有 Key 的 Custom 账号（验证为可选）。

受保护的 `GET /dashboard/api/v3/application-models` 是另一份本地列表：当前可路由的 OpenCode Go 别名与当前 OpenCode Go 价格快照求交。highspeed 变体继承基价行。空交集返回 `[]`。该列表不含 Custom ID，并使用已保存的本地状态。

`/v1/models` 可以让 Zen、Command Code、MiniMax 或 Kimi 映射通过代码持有的 Alias 对外供应，也可以公布密封映射中的供应商专属 Alias。Command 会去掉 Provider 命名空间；`-paid` / `-free` 只有在短 Alias 已获授权时才去掉；`nvidia/nemotron-3-ultra-550b-a55b` 映射为 `nemotron-3-ultra`，有语义的变体不会按长度截断。只有精确保存目录行仍存在，且至少一个供应商 mapping 仍有已启用协议时才公布该 Alias。无法匹配代码授权 Alias 的 Command/MiniMax/Kimi 目录 ID 只能按精确原始 ID 使用，不会作为新 Alias 出现在列表里。合格 Custom 声明 ID 即使含 `/` 也可以出现；它们不会折成 kebab 别名。`application-models` 仍是更窄的 Go 与价格交集列表。

原始上游 ID 在注册表中恰好对应一个 mapping 时，会钉在该 mapping 上——不跨 Plan 回退，也不做 Zen prefer 覆盖——然后才检查可路由性。内置 raw ID 严格区分大小写；名称里含 `/`、`_` 或空白时同样不会折叠成 kebab 别名（`glm/5.2` 不是 `glm-5.2`）。Custom 能力 ID 保持原有的大小写折叠匹配。精确 raw ID 映射到多个 Plan 时（含合格 Custom 能力与另一 Plan）返回 `400`，错误码 `ambiguous_model_id`，且不会调用上游。未知名称——既非静态授权 Alias、精确保存的内置 raw ID，也非合格 Custom ID——在所有受支持的客户端格式上返回 `400`：Chat Completions、Responses、Messages，以及 Gemini `generateContent` / `streamGenerateContent`。canonical kebab 别名 `deepseek-v4-flash` 因为存在于静态 Go 表且有对应的 Zen `-free` 行，才可以在已启用的 Go、Zen 与 Command Code mapping 中选择；唯一原始 ID `deepseek/deepseek-v4-flash` 只钉在 Command Code。Zen 的 `foo-free` 始终保留精确 raw pin，并按 `-free` 后缀公布 Alias `foo`。

转发日志把请求身份与上游身份分开记录：

- `requested_model` — 客户端发送的公开名称或 Alias
- `resolved_alias` — 存在时解析出的公开 Alias
- `upstream_model` — 实际发送到该账号上游的精确模型 ID

以及 `provider_id`。原生成本字段可选。

---

[用户指南索引](../USER.zh-CN.md) · [English](gateway.md) · [文档索引](../README.zh-CN.md)
