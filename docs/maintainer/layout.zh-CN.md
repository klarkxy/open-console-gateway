[English](layout.md)

# 仓库结构

```
crates/ocg-domain          ID、目录、协议策略
crates/ocg-gateway         无 I/O 的 alias、AttemptSpec、selector、JSON 转换
crates/ocg-infra           加密、代理/推理 HTTP、日志 SQL
crates/ocg-core            SQLite、Dashboard V3、适配器、执行器
crates/ocg-cli             ocg-manager-cli：serve / key / status
crates/ocg-browser-worker  Linux Chromium sidecar（不依赖 ocg-*）
src/                       Vue 3 面板（只走 HTTP Dashboard V3 + V4）
src-tauri/                 Desktop Host capability，注册进 CoreState
schema/                    冻结的 dashboard-api-v3 与增量的 dashboard-api-v4 schema
docs/                      USER / MAINTAINER / 防滥用
scripts/                   发版、契约、冒烟
```

Workspace 成员和 `rust-version` 在根目录 `Cargo.toml`。面板 HTTP 客户端：
`src/api/dashboard-v3.ts`（共享传输层）与 `src/api/dashboard-v4.ts`，以及
`src/api/dashboard.ts` / `src/api/providers.ts` / `src/api/connections.ts` 的
presenter。镜像相关：`Dockerfile`、
`Dockerfile.browser`、`compose.yaml`、`compose.example.yaml`、
`docker-bake.hcl`。

---

[维护者指南索引](../MAINTAINER.zh-CN.md) · [English](layout.md) · [文档索引](../README.zh-CN.md)
