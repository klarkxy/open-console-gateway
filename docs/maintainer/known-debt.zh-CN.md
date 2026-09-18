[English](known-debt.md)

# 已知缺口与明确非目标

本页描述当前实现与项目范围。非目标是设计边界，不是对贡献者个人工作流的要求。
调整范围时，应在提案或 PR 中说明兼容性与维护影响，并同步修改代码和文档。

## 已知缺口

- `auto_start` 受能力门控：Windows x64、macOS 和 Linux x64 的 release / 已安装 Tauri 进程注入登录自启同步钩子。开发构建、CLI、Docker 面板不暴露该开关。Dock 可见性仅 macOS Tauri。
- 生成的 Tauri schema 文件会让 diff 变吵；只在 Tauri 配置确实改动时才需要修改它们。
- 流式用量仅在上游发出 usage chunk 时精确；Chat 流式请求会设置 `stream_options.include_usage`。没有 chunk 时 Go 行记为 `success_no_usage`； Zen 无 usage 的成功仍为 `success` / `free`。
- 浏览器会话使用 `browser-profiles/<account_id>`；旧 `profiles/<account_id>` Profile 不会被原生浏览器复用，受影响的用户需要重新登录。旧路径仅保留用于重置/删除时的安全清理。
- Responses 端点是无状态。`previous_response_id`、`conversation`、 `store: true`、`background: true` 返回 `400`。详见 `protocol.rs` 和[限制](../user/limits.zh-CN.md)。
- Gemini 是客户端兼容格式。转发、`400` 与 `501` 行为见[限制](../user/limits.zh-CN.md)和[协议转换](../user/protocol-conversion.zh-CN.md)。
- Command Code GOAT 账号用量来自官方 CLI 使用、但公开 Provider API 未文档化的第一方 `/alpha/billing/credits` 端点。其响应稳定性没有公开契约保证，因此手工刷新会校验精确 GOAT 上限，并在 schema 或套餐漂移时 fail closed。公开模型目录仍不能验证已保存 Key，因此鉴权失败只能从真实推理 401/403 得知。Custom API 仍是独立的已上线路由，遵循受信管理员边界（`custom.rs` + `custom_http.rs`）。
- 按模型/按协议覆盖在 V3。Custom 账号级按协议探测没有 V3 端点；账号侧探测路径返回 410。Custom 验证与模型发现是现行路径。
- V4 操作摘要密钥（`dashboard_operation_digest_key`）与账号 Key 存放在同一个 SQLite 文件中（账号 Key 为 AES-256-GCM `v2:` 密文）。
- `accounts` 行持有冷却列与 Key 材料；V4 身份模型是建立在这些行之上的附属投影。

## 明确非目标

- 动态适配器/插件加载、用户自定义适配器实现，或持有 SQLite、`CoreState`、原始
  `reqwest::Client` 的适配器。类型化用户定义 Provider 仍受支持，但它只是绑定到
  密封 Configurable HTTP 适配器的数据。
- 远端节点同步、Admin API 或多租户控制面。
- 把 Tauri `invoke` 或 WebView command 用作面板数据路径。
- 在 `GET /v1/models` 或 `GET /dashboard/api/v3/application-models` 上做请求时上游发现。
- GOAT 官方权威用量 API，或把其公开目录当作 Key 验证。
- `/embeddings`、Gemini `embedContent`（501），或把 Gemini `countTokens` 做成真实上游计数（501 供 Gemini CLI 回退本地估算）。
- Gemini 作为上游协议。
- 自动轮询价格或 Zen 目录。
- 旧 WebView Profile 跨引擎复用。
- 数据库降级，或让旧二进制打开更新后的 schema。
- Windows/Linux ARM64 桌面包、32 位 x86、RPM、Snap、应用商店包、Windows Authenticode 或 Apple 公证；此项不排除已支持的 Linux ARM64 容器镜像。
- 在 GitHub provenance 之外再加一份 Cosign 镜像签名。

---

[维护者指南索引](../MAINTAINER.zh-CN.md) · [English](known-debt.md) · [文档索引](../README.zh-CN.md)
