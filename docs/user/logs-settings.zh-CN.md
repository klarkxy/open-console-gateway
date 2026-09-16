[English](logs-settings.md)

# 日志与设置

## 日志

**日志** 视图默认打开 **请求日志**，它是 Gateway 转发请求和显式供应商协议探测的滚动记账本：时间戳、选中的 provider、route account、credential account、模型、状态码、上游错误（如果有），以及上游返回 usage chunk 时的流式用量。探测行的 token 为零、费用不适用、不归因到客户端 Key，也不会出现在运行日志。账号选择前发生的已认证解析、校验或路由失败同样显示在这里，并采用“未解析/Gateway”归因；运行日志只保留进程与控制面事件。可按 provider、route account、credential account、模型、状态、时间范围与客户端 Key 筛选。每条存储行把请求身份与上游身份分开：

- `requested_model` — 客户端发送的公开名称或 Alias
- `resolved_alias` — 存在时解析出的公开 Alias
- `upstream_model` — 实际发送到该账号上游的精确模型 ID

以及 `provider_id`。模型筛选会对这些身份或 `model` 列做精确匹配。原生成本（`native_cost_value`、`native_cost_unit`、 `native_cost_currency`）是可选字段，只有该 Provider 提供足够价格证据时才有值。

当该 Provider 有足够价格证据时，每行还会保留原始供应商成本、额度扣减和实际付费成本。allowance 只改变额度扣减倍率。

- Chat 流式请求会设置 `stream_options.include_usage`，让 OpenAI 兼容上游返回 usage chunk。仍然没有 usage chunk 的行会标 `success_no_usage`。usage chunk 让 token 数量准确；汇总区显示输入 + 输出的总 Tokens。额度消耗按本次选中 Provider 的已验证价格快照估算：OpenCode Go 使用当前快照，Command Code GOAT 使用独立刷新的模型价格与倍率。Ollama Cloud 使用手动刷新的 `https://ollama.com/pricing` 快照且配额倍率为 `1.0`；这些已定价行计入一个月 USD Credits 窗口，可以超过 Pro/Max/Team 软上限且不改变路由。旧日志不会用新价格追溯重算。已登记的 Zen free 模型（`big-pickle`、`mimo-v2.5-free` 等）会记录 token，但 `cost_state=free`，不计入 Go 额度。Custom API 行记 `cost_state=unknown`，不扣供应商额度。展开行可查看请求 ID 与诊断详情。
- `outcome_unknown` 表示上游可能已经完成并扣额，但 Gateway 超时或丢失响应；这类请求不会自动重试，且本地额度消耗保持未知。
- **Key** 筛选把行与汇总统计限定到单个客户端 Key。选项来自日志表本身，因此已停用、已删除或未知的 Key 仍可筛选。**未归因** 筛选没有客户端 Key 归因的行；后台任务会近似归到主 Key。

## 设置

**设置** 视图保存 Gateway 的持久化配置：

- **Gateway 端口**：Gateway 监听端口（默认 `9042`）。桌面版也支持只读的
  `OCG_GATEWAY_PORT` 运行时覆盖；变量生效时设置项会禁用，已保存值不变。
- **路由方案**：严格优先级、全局粘性或轮询。三种方案都会先过滤不兼容、禁用、冷却或本请求已失败的卡片，再使用同一份全局卡片顺序。同一时刻只能启用一种基础方案。
- **对话粘性**：叠加开关，不是第四种路由方案。开启后优先使用请求头 `X-OCG-Conversation-Id`；未提供时用 Prompt 指纹（system / tools / 首条 user）。无法生成会话 key 时回退基础路由。相似提示词可能绑到同一账号。
- **出站代理**：不区分账号。`自动（系统 / 环境）`、`手动 HTTP 代理`、`强制直连` 是进程全局策略；**按模型名单**（下一条）则按模型分流聊天转发。`自动（系统 / 环境）` 会读取 `HTTP_PROXY`、`HTTPS_PROXY`、`ALL_PROXY`、`NO_PROXY`，Windows 还会读取系统代理；没有代理时直接连接。`手动 HTTP 代理` 将所有 HTTP/HTTPS 目标严格送往一个 `http://` 或 `https://` 代理（例如 `http://127.0.0.1:7890`），代理失败时不会静默回退直连；`强制直连` 则忽略系统和环境代理。代理 URL 不能包含账号密码。前三种模式下，该策略覆盖模型转发（OpenCode Go、Zen Free、Command Code GOAT、MiniMax CN、Kimi Code CN 与 Custom API）、账号 Key 测试与 Custom 验证、OpenCode Go 官方用量接口、价格刷新、Release 检查以及已安装桌面版的签名升级下载等核心 HTTP 请求；带鉴权的 `GET /v1/models` 与受保护的 `GET /dashboard/api/v3/application-models` 是本地列表，不走这条出站路径。浏览器 Sidecar 不在此设置范围内。**测试连接**使用尚未保存的表单值访问密封的 OpenCode Go 官方源，收到任意 HTTP 状态都表示网络链路可用，且不会发起模型推理或产生模型费用。名单模式下它只验证方向默认段，不能代表名单内模型的真实转发路径。
- **按模型名单**（第四种代理模式）：按模型而不是进程全局分流聊天转发。选择方向并勾选启用协议的 Provider contract、合格 Custom capability、用户定义 Provider mapping 与当前 CPA 目录中的精确上游模型 ID；公开 Alias 不会混入。名单不支持通配符或自由文本。旧配置里已从这些来源消失的 ID 保持 inert，并在下次保存时剔除。**白名单**方向下，名单内模型走代理地址，名单外模型直连（忽略系统/环境代理，与强制直连同义）；**黑名单**方向相反：名单内模型直连，其余模型走代理地址。两个方向都要求填写代理地址；空名单或空地址无法保存。非聊天出站（价格刷新、官方用量同步、升级检查、签名升级下载）始终走方向的默认段——白名单为直连、黑名单为代理地址，因此从 `手动 HTTP 代理` 切到白名单后，这类流量会改为直连。账号 Key 测试与**测试连接** 同样只验证默认段，不能代表名单内模型的真实转发路径。free 通道模型可以入名单，但 Zen free 额度按出口 IP 共享，走代理会改变额度归属。每条转发日志（含成功行）都会在详情中记录实际路由段（`proxy` / `direct` / `auto`）；没有记录路由段的行显示为未记录。以名单模式保存的配置无法被名单模式出现之前的构建打开；回滚到这类构建前，请先切回手动或强制直连模式。
- **下游访问根地址**：见 [接入中心](dashboard.zh-CN.md#接入中心)。
- **登录后自动启动**：已安装的 Windows x64、macOS 和 Linux x64 桌面版暴露此开关；开发构建、CLI、Docker 面板不显示。Linux AppImage 启动项指向 AppImage 文件，请保留该文件的位置；移动后需重新开关自启动。
- **Dock 图标**：只有 macOS 桌面版暴露此开关；关闭后应用仍保留菜单栏图标， Windows、Linux、CLI 与 Docker 面板不显示。
- **连接 / 非流式 / 流式空闲超时**：默认分别为 30、900、300 秒。非流式值是整个请求的总时限；流式空闲值按相邻响应 chunk 之间的等待时间执行。已保存的超时若与完整的旧默认值（30/120/300）完全一致，启动时会迁移为新默认值；自定义取值保留。
- **检查更新 / 立即升级**：支持升级的已安装桌面版会检查 GitHub 最新 Release，并可下载、校验签名、原地替换已有副本。数据目录与开机启动设置保留。开发构建、CLI、Docker 仍显示发布页并手动升级。主机必须能访问 GitHub；检查或安装失败不影响 Gateway 转发。
- **Zen Free**：在账号卡上直接启用或关闭；在 **供应商** 页刷新 Free 模型目录、查看协议证据，并切换 Chat Completions / Responses / Messages。

配置项写入 SQLite，下次启动时重新加载。Settings 资源从不包含 Key 明文。保存使用与其他 Dashboard V3 写入相同的 `expectedRevision` / `processGeneration` token。检查更新是按需动作。

---

[用户指南索引](../USER.zh-CN.md) · [English](logs-settings.md) · [文档索引](../README.zh-CN.md)
