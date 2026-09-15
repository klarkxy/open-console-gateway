[English](conventions.md)

# 编码约定

- **保持 crate DAG**。domain 与 gateway 保持无 I/O。门面按条目再导出。适配器返回 `AttemptSpec`。`forward_once` 是一次上游调用。Dashboard V3 不导入 `gateway`。
- **前端不新增 Tauri `invoke()` 路径**。Vue 主数据路径是 HTTP `/dashboard/api/v3` 与 `/dashboard/api/v4`。
- **受保护的 V2 REST 保持墓碑状态**。新面板 JSON 属于 V3 或 V4。410 墓碑保留。
- **安全边界不能为了简化而削弱**。Gateway 鉴权、Key 混淆、URL 校验、冷却写入、SSE 透传以及 ConnectionInfo 密钥边界均保留。
- **不引入远端同步**。每个节点由自己的面板管理。
- **`auto_start` 与 `show_dock_icon` 受能力门控**。Windows x64、macOS 和 Linux x64 的 release / 已安装 Tauri 进程注入登录自启同步钩子；Dock 仅 macOS Tauri。
- **本地 Alias 列表保持本地**。带鉴权的 `GET /v1/models` 与面板 `application-models` 读取已保存状态，不在请求时增加上游发现。目录刷新是独立、显式的控制面操作，使用各 Provider 支持的数据源。两份列表的收录规则不同；请求日志使用 `requested_model`、`resolved_alias` 与 `upstream_model`。
- **尊重 `parking_lot::Mutex` 不可重入**。调用另一个持锁函数前先 `drop` guard。

## 文档

- 当前行为以代码为准。按 `AGENTS.md` 中的权威来源指针核对。
- 根 README 是落地页。能力表放在 `docs/user/`；维护流程放在 `docs/maintainer/`。成对英文与 `.zh-CN.md` 保持标题结构、链接与 TOC 锚点对齐。
- `DESIGN.md` 与 `src/theme.ts` 负责视觉 token 和面向用户的 **Key** 名称。包清单与 `compose.example.yaml` 负责版本钉。
- 只描述当前行为。已知缺口放入 `docs/user/limits.md` 或 `docs/maintainer/known-debt.md`。用肯定句写；只有当该页的首次读者合理会假设相反情况时才补充否定。
- 仓库文档与 `AGENTS.md` 只记录共享的项目事实。个人模型选择、智能体角色和本机工具路径放在用户级配置中。
  贡献者可以使用自己的编辑器、助手和评审工作流；仓库要求针对最终改动及其验证。
- 区分当前行为与项目设计决策。设计约束应说明其兼容性、归属或易用性目的。
  单次验证的环境、版本、覆盖范围与未执行检查写入当前发布说明与 CI 结果；一次排除不代表后续发布豁免。
  示例使用占位符或有文档的产品默认值，不使用维护者的私有路径、账号或网络配置。

---

[维护者指南索引](../MAINTAINER.zh-CN.md) · [English](conventions.md) · [文档索引](../README.zh-CN.md)
