[English](MAINTAINER.md)

# 维护者指南

先阅读 [CPA 架构](architecture.zh-CN.md)、[迁移边界](maintainer/cpa-migration.zh-CN.md)与[已完成验证](maintainer/cpa-validation.zh-CN.md)。它们定义新版本；旧内核仍留在源码中等待迁移。

[原生 GUI 章节](architecture.zh-CN.md#9-原生桌面-gui)统一定义已确定的 GPUI/Ely 布局、共享控制/生命周期边界及 Windows/Linux/macOS 验证目标。概念图和平台目标不是运行验收证据；当前分支尚无 GPUI GUI。

## 当前实现参考

- [开发](maintainer/development.zh-CN.md) — 当前 Rust CLI 构建与检查。
- [CLI](user/cli.zh-CN.md) — 已实现命令与操作。
- [HTTP 合约](maintainer/dashboard-api.zh-CN.md)与[路由](maintainer/http-routes.zh-CN.md) — 现有 V4/CAS 行为，不表示 CPA 迁移已完成。
- [存储迁移](maintainer/storage-migration.zh-CN.md) — 当前加密、备份与回滚。
- [应用接入](maintainer/byok-applications.zh-CN.md) — 客户端专用行为，不是另一套执行架构。
- [约定](maintainer/conventions.zh-CN.md) — 设计/源码区分与文档规则。
- [CI](maintainer/ci.zh-CN.md) — 当前检查与旧发布器限制。
- [工作检查点 — 2026-10-05](maintainer/work-checkpoint-2026-10-05.zh-CN.md) — 暂停的 CLI 工作、证据及重装后续接步骤。

## 历史发布参考

[发布产物](maintainer/release-artifacts.zh-CN.md)与[发布流程](maintainer/releasing.zh-CN.md)描述以前的桌面发行，不是 CPA 底层 OCG3 可用的发布器。发布说明保留在 releases/。

本次文档更新记录原生 GUI 方向，不实现 GUI、不迁移运行时/数据库，也不发布。实施时保留完整 CLI 与共享 V4/CAS 控制操作；替换执行职责时保留兼容/安全保证，不另建外层重试调度器。

[文档索引](README.zh-CN.md) · [助手指引](../AGENTS.md)
