[English](MAINTAINER.md)

# 维护者指南

本指南面向改代码、发版、调试 Gateway 和验证桌面安装包的人。它记录 HEAD 上实际实现的架构与运行契约。

## 章节

- [仓库结构](maintainer/layout.zh-CN.md) — crate 与目录结构。
- [开发](maintainer/development.zh-CN.md) — 开发循环与检查。
- [架构](maintainer/architecture.zh-CN.md) — 依赖边界、适配器身份、请求流转与文字图。
- [Dashboard API](maintainer/dashboard-api.zh-CN.md) — 冻结的 V3 契约、增量 V4 接口、CAS token 与变更规则。
- [状态、凭据与生命周期](maintainer/state-and-lifecycle.zh-CN.md) — `CoreState`、锁顺序、凭据与持久化。
- [HTTP 路由](maintainer/http-routes.zh-CN.md) — 推理路由、V3/V4 路径、V2 墓碑与 auth/session 路由。
- [运行时不变式](maintainer/runtime-invariants.zh-CN.md) — Gateway、别名、Zen Free、套餐目录、访问 Key、代理与用量同步的详细语义。
- [存储与迁移](maintainer/storage-migration.zh-CN.md) — SQLite schema v49、历史迁移、备份与运维手册。
- [扩展 Open Console Gateway](maintainer/extending.zh-CN.md) — 静态密封的供应商扩展步骤。
- [发布产物](maintainer/release-artifacts.zh-CN.md) — 支持的平台矩阵与包名。
- [CI 工作流](maintainer/ci.zh-CN.md) — quality、release 与 container 工作流。
- [发布流程](maintainer/releasing.zh-CN.md) — 版本 bump、tag、构建与发布检查清单。
- [已知缺口与明确非目标](maintainer/known-debt.zh-CN.md) — 已记录的缺口与有意不做的事项。
- [编码约定](maintainer/conventions.zh-CN.md) — crate DAG、安全边界与文档归属。

## 阅读路径

- **贡献者** — `layout` → `development` → `architecture` → `state-and-lifecycle` → `http-routes` → `conventions`。
- **发版负责人** — `release-artifacts` → `ci` → `releasing` → `known-debt`。
- **UI / 主题工作** — 先读 `DESIGN.md`，再改 `src/theme.ts` 与对应 Vue 页面。

---

[文档索引](README.zh-CN.md) · [English](MAINTAINER.md)
