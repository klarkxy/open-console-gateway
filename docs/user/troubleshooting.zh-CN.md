[English](troubleshooting.md)

# 常见问题

Open Console Gateway 出问题，通常先怀疑有别的进程占了 `127.0.0.1:9042`——本地 Gateway 的端口，向来不太空闲。下文还覆盖陈旧 SPA、冲突写入、账号冷却，以及看起来能跑但实际仍是 `pending` 草稿的 Plan；Gateway 宁可报错，也不会替你猜一个可能多收钱的请求。

- **Windows 上打开应用没反应、找不到托盘图标。**重新打开桌面版会尝试恢复已有实例的托盘并打开管理界面。新实例会先创建托盘，再启动 Gateway；启动失败会显示错误弹窗。如果端口被 `ocg-manager-cli.exe` 占用，弹窗会显示进程号和程序路径，可确认停止旧服务并重试。当前请求会中断，账号、配置和数据不会删除；取消则保留旧服务。其他程序占用端口时只显示诊断信息，需要先退出该程序。CLI 本身仍支持无托盘运行。Windows 可能把托盘图标收进折叠菜单，程序无法强制它固定显示在任务栏。若旧桌面实例已卡死，需在任务管理器结束它后重新打开。仅源码开发时可用 `scripts/free-dev-port.mjs` 清理 `30001` 上的残留 Vite 进程；它不会释放 `9042`，也不会释放桌面端单实例锁。
- **上游返回 `401 Unauthorized`。**Zen Free 原样返回。OpenCode Go 只在结构化错误为 `CreditsError` 时换号并记录 `auth_error`；续费后重新保存同一个 Key 即可清除。`ModelError`、未知或畸形的 OpenCode 401 仍原样返回。Custom API 的 `401` 会换到下一张合格卡片并记录 `auth_error`。要确认 OpenCode Go Key 本身是否失效，请执行 `key ping <id>` 或发一次真实客户端请求。托管账号 Key 验证与 Custom **验证连接** 在各自流程里拿到 401 时仍会记录 `auth_error`。
- **面板提示页面版本与服务不匹配。**缓存的旧 SPA 命中了已退役的 `/dashboard/api` REST（而不是 `/dashboard/api/v3`），收到 HTTP 410。请刷新页面；若仍失败，安装匹配的桌面、CLI 或 Docker 版本。
- **面板保存失败并提示冲突 / 409。**同一运行进程中的另一个标签页已经先写入。SPA 会根据服务端的 `revisionConflict` 刷新受影响数据，但不会自动重放变更；确认当前值后再次提交。
- **本地进度条满格但请求依然成功。**这是 **假熔断**——本地估算不是上游账单。继续使用即可，Gateway 会继续转发。
- **本地进度条满格，Gateway 返回 `429`。**这是 **真熔断**。等 `cooldown_until` 到期，或在 **账号** 视图手动解除冷却。
- **Gateway 返回 `429` 并提示 "all accounts cooling down"。**所有已启用账号都在冷却。等最近的恢复时间，或新增/启用其他账号。
- **Gateway 因模型名返回 `400`。**请发送带鉴权的 `GET /v1/models` 公布的别名或合格 Custom ID。含 `/`、`_` 或空白的名称是原始 ID，不是 kebab 别名。未知名称和重叠的原始 ID 会 fail-closed，且不会调用上游。
- **Command Code GOAT 没有产生路由。**确认账号已启用、ready 且 Key 非空，并检查 **供应商** 矩阵中该模型的受支持协议是否开启。公开 `/models` 刷新不验证 Key；真实无效 Key 会在推理时返回 401/403。
- **保存 Custom API 后仍无法路由。**请确认账号已启用且 ready、Key 非空且请求模型已声明。验证不会改变开关。验证动作只用所选协议向解析后的推理 Endpoint 发送一次最小请求，并要求返回 `2xx` JSON；更改 API 地址、Key、声明模型或协议会使验证状态变为 `pending`，但保持该卡当前的启用状态。
- **Gemini 请求因 `safetySettings` 返回 `400`。**Gateway 无法把 Google 的安全阈值等价映射到 Chat/Messages 上游，因此拒绝非空数组。删除该字段后重试；Chat/Messages 上游使用自己的策略。
- **Docker 首次注册的 `OCG_ADMIN_PASSWORD` 没生效。**这两个变量只在数据库还没有管理员时生效，请使用数据库里已有的管理员账号。只有在确认备份有效且确实要完全重置时才重建 `ocg-data` 与 `ocg-browser-profiles`——这会删除全部账号、凭据、设置、Cookie 和浏览器 Profile。
- **SmartScreen / Gatekeeper 弹窗警告。**当前 Windows 包未签名、macOS 应用使用 ad-hoc 签名。首次启动请用 **Open Anyway** 放行，警告本身不代表篡改。

---

[用户指南索引](../USER.zh-CN.md) · [English](troubleshooting.md) · [文档索引](../README.zh-CN.md)
