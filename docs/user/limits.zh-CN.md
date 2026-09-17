[English](limits.md)

# 限制

本页列出明确错误与未实现的表面。推荐/支持协议矩阵见
[协议转换](protocol-conversion.zh-CN.md)。

- 未实现 `/embeddings`。Gemini `embedContent` 会被路由，但返回 Google 风格的 `501 UNIMPLEMENTED`。
- Gemini `countTokens` 同样返回 `501`；Gemini CLI 预期回退到本地估算。只有 `generateContent` 与 `streamGenerateContent` 会真正转发。
- 非空 Gemini `safetySettings` 返回 `400`，因为不同上游协议无法保留其安全语义； `null` 与空数组不携带策略，可以接受。
- Gemini `cachedContent`、`fileData`、Google Search 工具、`urlContext`、Code Execution、多模态 function response、function response 的 schema/behavior、 `VALIDATED` 函数调用、`candidateCount` 大于 1、非 TEXT 输出模态返回 `400`。图片请改用 base64 `inlineData`，支持 PNG、JPEG、GIF、WebP。
- Gemini `topK` 与 `thinkingConfig` 只作为跨协议兼容提示接受；Chat Completions 或 Messages 原生上游可能忽略或实现不同语义，不保证与 Gemini 原生后端的采样和思考行为等价。
- 其他无法保留的非空生成选项（包括 `seed`、presence/frequency penalty、logprobs 与 media resolution）返回 `400`，不会静默丢弃。
- Responses 是无状态端点：必须设置 `store: false`。`previous_response_id`、 `conversation`、`store: true`、`background: true` 全部直接 `400` 拒绝，不会静默忽略。
- Responses 支持图片 URL 与 data URL；`input_image.file_id` 返回 `400`，因为 Gateway 没有 Files API。
- 跨协议转换无法保留约束的结构化输出与自定义工具语法会返回 `400`。
- Responses 的 `web_search`、`web_search_preview`、`tool_search` 等托管工具在 OpenCode-Go 上无法运行；自动工具模式下会被丢弃，显式强制使用则返回 `400`。 function、custom、namespace 工具正常转换。
- 流式 token 数量仅在上游发出 usage chunk 时准确；Chat 流式请求会设置 `stream_options.include_usage`。额度消耗使用当前 OpenCode Go 价格快照。没有 usage 时日志记为 `success_no_usage`。
- 浏览器向导只提供人工页面操作，不自动注册 Google、处理验证码、支付、抓取网页或提取 Key。
- 已安装的 Windows x64、macOS 和 Linux x64 桌面版可以在用户登录时把 Open Console Gateway 拉起到托盘；开发构建、CLI、Docker 不暴露面板里的 `auto_start` 开关。Docker Compose 另由 `restart: unless-stopped` 在 Docker daemon 重启后恢复服务。
- macOS 桌面版可以在设置中隐藏 Dock 图标而只保留菜单栏图标；Windows、Linux、 CLI 与 Docker 不暴露 `show_dock_icon` 开关。
- 不发布 Windows / Linux ARM64、32 位 x86 构建；不支持 RPM、Snap、应用商店包、 Windows Authenticode 正式签名、Apple 公证。该口径仅覆盖桌面安装包；容器镜像（`ghcr.io/klarkxy/opencode-go-mgr` 及其 `-browser` 侧车）发布 `linux/amd64` 与 `linux/arm64`。支持升级的已安装桌面版可在设置页安装签名 Release；1.4.1、开发构建、CLI、Docker 使用直接/手动升级路径。
- Command Code GOAT 是已上线的固定官方源路由。其公开 `/models` 目录只在 **供应商** 页显式刷新；GOAT 预设默认开启，额外模型默认关闭。GOAT 目录刷新更新模型目录；Key 鉴权从推理 401/403 观察。已验证价格快照通过每个已定价模型可保存的手动倍率估算新请求成本。账号卡可显式从 Command Code 官方 CLI 使用的第一方 `/alpha/billing/credits` 端点校准本地 `$14 / $35 / $70` 三个窗口，再继续累计 OCG 内已定价日志。公开 Provider API 文档未列出该端点，GOAT 不做自动同步，并保留手工修正。Custom API 已在 [账号](accounts.zh-CN.md) 的受信管理员边界下上线路由；不计价，也没有官方用量路径，其目录、协议与价格控制作为隔离的 `CustomEndpoint` 范围呈现在 **供应商** 页。
- Ollama Cloud 每月 USD Credits 用量是本地已定价日志的软估算。实际已用可以超过 Pro `$60` / Max `$300` / Team `$1000` 上限；面板把进度条钳在 100% 并显示超出量。进度条满了绝不写冷却、不停用账号、不改路由。新建 Ollama 账号必须明确选择 Pro/Max/Team 并填写购买日期。无计费行的既有账号仍可路由且无进度条。真实上游 `429` 仍走既有通用冷却/回退。
- Zen Free 路由使用账号卡的启用开关和列表位置。没有 Deny / Explicit / Prefer 策略。
- 未知模型名在所有受支持的客户端格式上返回 `400`。客户端应发送带鉴权的 `GET /v1/models` 公布的、当前有有效启用协议的别名或合格 Custom ID。受保护的 `GET /dashboard/api/v3/application-models` 是 Go 别名 ∩ 当前价格快照，不是那份完整客户端列表。

---

[用户指南索引](../USER.zh-CN.md) · [English](limits.md) · [文档索引](../README.zh-CN.md)
