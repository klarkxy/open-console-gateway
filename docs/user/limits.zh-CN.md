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
- 流式 token 数量仅在上游发出 usage chunk 时准确；Chat 流式请求会设置 `stream_options.include_usage`。Gateway 不会为新请求估算价格。没有记录到的费用保持未知，不会显示成零或免费。没有 usage 时日志记为 `success_no_usage`。
- 浏览器向导只提供人工页面操作，不自动注册 Google、处理验证码、支付、抓取网页或提取 Key。
- 已安装的 Windows x64、macOS 和 Linux x64 桌面版可以在用户登录时把 Open Console Gateway 拉起到托盘；开发构建、CLI、Docker 不暴露面板里的 `auto_start` 开关。Docker Compose 另由 `restart: unless-stopped` 在 Docker daemon 重启后恢复服务。
- macOS 桌面版可以在设置中隐藏 Dock 图标而只保留菜单栏图标；Windows、Linux、 CLI 与 Docker 不暴露 `show_dock_icon` 开关。
- 不发布 Windows / Linux ARM64、32 位 x86 构建；不支持 RPM、Snap、应用商店包、 Windows Authenticode 正式签名、Apple 公证。该口径仅覆盖桌面安装包；容器镜像（`ghcr.io/klarkxy/opencode-go-mgr` 及其 `-browser` 侧车）发布 `linux/amd64` 与 `linux/arm64`。支持升级的已安装桌面版可在设置页安装签名 Release；开发构建、CLI、Docker 使用直接/手动升级路径。
- Command Code GOAT 是已上线的固定官方源路由。其公开 `/models` 目录在 **供应商** 页显式刷新，账号卡点 **刷新额度** 时也会刷新；GOAT 预设与新发现的模型在已知受支持协议时默认开启，但 GOAT 首次快照仅默认开启套餐包含的模型，该次快照中的其余行保持关闭，可手动开启。GOAT 目录刷新更新模型目录；Key 鉴权从推理 401/403 观察。账号卡可显式 **刷新额度**，从 Command Code 官方 CLI 使用的第一方 `/alpha/billing/credits` 端点读取官方百分比窗口。读数里有百分比时，窗口按该百分比相对 100 显示，并保留该重置时间。金额不会被改写成百分比。之后的请求不会把价格累加到百分比上。公开 Provider API 文档未列出该端点，GOAT 不做自动同步。之后仍可以手工保存一个百分比。既没有官方读数、也没有手工百分比时，窗口保持不可用，不会显示成 0。Custom API 已在 [账号](accounts.zh-CN.md) 的受信管理员边界下上线路由。没有记录到的费用保持未知，不会显示成零或免费。手工积分余额不会因完成的请求被扣减。没有通用的官方用量路径，目录和协议控制在 **供应商** 页。
- 账号卡在供应商提供相应数据时展示三类依据：定时额度窗口、官网余额和手工积分余额。没有读到的观测保持不可用，不会显示成 0。OpenCode Go 与 GOAT 的官方窗口按观测到的百分比相对 100 显示。OpenCode Go、GOAT 和 Ollama 可以保存手工百分比。Go 使用 5 小时、本周和本月窗口。Ollama 使用月窗口。Zen、MiniMax、Kimi、Custom 和 CPA 不显示这个编辑；元数据明确关闭手工校准时也不显示。第一次保存的百分比只是该额度窗口。再次打开页面，或页面仍打开时再读一次，都会显示这个百分比，不会把它补成一份完整的计费状态。手工积分、现金余额和平台站点已观测的消费历史仍然分开。手工积分余额的分桶、授予、月度续期和过期仍然分开；**校准用量** 用于手工改正已保存余额。完成的请求不会扣减这份余额，产品也不使用按 token 费率或货币换算。未知保持未知，不会记成零或免费。余额为空或未知不会停用 Key，也不改变路由。官网剩余余额是已观测的钱包，不是按本地价格估算的本月或历史花费。
- Ollama Cloud 在本产品中没有官方用量 API，也不会按请求价格估算每月积分。账号表单仍显示 Pro、Max 或 Team，以及购买日期。手工百分比不要求填写价格。月百分比可以在选定档位之前保存。周百分比不被接受。没有官方或手工用量观测时，用量保持不可用，不会显示成 0。既有账号仍可路由。以前保存的计费行留在本机，不会重算。已有窗口满格也不会写冷却、停用账号或改变路由。真实上游 `429` 走通用冷却/回退。
- Zen Free 路由使用账号卡的启用开关和列表位置。
- 未知模型名在所有受支持的客户端格式上返回 `400`。客户端应发送带鉴权的 `GET /v1/models` 公布的、当前有有效启用协议的别名或合格 Custom ID。受保护的 `GET /dashboard/api/v4/application-models` 列出已保存目录中可解析且协议已启用的 Go 名称。它不查阅价格快照，也不是那份完整客户端列表。

---

[用户指南索引](../USER.zh-CN.md) · [English](limits.md) · [文档索引](../README.zh-CN.md)
