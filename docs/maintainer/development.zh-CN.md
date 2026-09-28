[English](development.md)

# 开发

## 前置要求

Node.js 22、`package.json` 的 `packageManager` 钉，以及 workspace 的
`rust-version`（锁定依赖要求 Rust 1.88 或更新版本）。原生依赖以 `.github/workflows/release.yml` 在对应 runner
上安装的为准。

## 开发模式

退出已安装的托盘程序，避免占用单实例锁和 `9042` 端口，然后：

```bash
pnpm install
pnpm run dev
```

`pnpm run dev` 以独立的开发默认端口 `OCG_GATEWAY_PORT=19042` 运行 `tauri dev`。
部分 Windows 主机的 HNS/WSL/Docker 保留端口范围包含 `9042`，开发默认端口可避开该冲突。安装版仍默认 `9042`。Vite
提供 `http://127.0.0.1:30001/dashboard/`，并把 `/dashboard/api`（含
WebSocket）代理到该 Gateway 端口。启动前设置 `OCG_GATEWAY_PORT` 可同时覆盖
Tauri 与 Vite；变量生效时，设置页以只读方式显示实际端口。

### 选择开发模式

- `pnpm run dev`（默认）：Tauri 监视 Rust workspace，源码变更时重新编译并重启整个桌面应用，进程内网关和所有在途请求都会中断。额外参数会透传给 Tauri CLI：`pnpm run dev -- --no-watch` 关闭 Rust 监视器，已启动的开发构建会持续服务，直到你手动重启；Dashboard 的 Vite HMR 仍然生效，而已保存的 Rust 改动在下一次手动重启时才生效。
- `pnpm run dev:split`：无头 `ocg-manager-cli` 网关 + Vite，不启动 Tauri 进程。适合 Dashboard、HTTP API、路由与协议开发。网关监听 `OCG_GATEWAY_PORT`（默认 `19042`），使用隔离数据目录（`tmp/dev-data`，可用 `OCG_DEV_DATA_DIR` 覆盖），因此可以和已安装的应用并行运行。没有任何 Rust 源码监视：修改网关相关 crate 后，停止脚本并重新运行以重新编译 `ocg-manager-cli`。`http://127.0.0.1:30001/dashboard/` 的 Dashboard 代理到拆分网关，Vue 改动仍然热更新。首次使用全新数据目录时，在私有终端用 `target/debug/ocg-manager-cli --data-dir tmp/dev-data status --show-key` 获取开发 Gateway Key。
- 桌面宿主开发（托盘、自启动、原生浏览器、更新器）仍需 `pnpm run dev`：CLI 不注册这些宿主能力。

拆分网关与 agent 正在使用的任何网关都是独立进程。重启它仍会中断自身的在途流；如果 agent 不能被打断，让它们继续连已安装的应用或另一个常驻实例。

`pnpm install` 会启用 `.githooks`（暂存 `*.rs` 时运行 `cargo fmt --all`）。

## 检查

以 `package.json` 中的脚本名为准。选能覆盖本次改动边界的最小检查：

| 改动 | 检查 |
| --- | --- |
| 单个前端或脚本测试 | `node --experimental-strip-types --test <file>` |
| Vue / dashboard | 相邻测试，再 `pnpm run build:web` |
| 单个 Rust crate | `cargo test -p <package>` |
| Core / Dashboard V3 | `cargo test -p ocg-core --features ollama-cloud-loopback-test <filter>` |
| Desktop Host | `cargo test -p ocg-manager --lib` |
| V3 或 V4 Schema 或生成类型 | `pnpm run contract:v3:check` / `pnpm run contract:v4:check` |
| `DESIGN.md` / 主题 | `pnpm run design:lint` |

`pnpm run test` 是跨前端/Rust 门禁。`pnpm run test:rust`（以及 quality.yml 的
Linux Rust job）会加上 `--features ocg-core/ollama-cloud-loopback-test`，以便
Ollama Cloud 网关集成测试安装仅 loopback 的测试接缝。该 feature 默认关闭：应用构建
保持固定的 `https://ollama.com` 源，且不编译该接缝。未开启 feature 的 workspace
`cargo test` 仍会编译 `ollama_cloud_gateway`，但其中用例不会运行。`pnpm run test:tooling` 覆盖
`scripts/*.test.mjs`，已包含在 `pnpm run test` 和 Quality 工作流中；完整测试通过后不用再跑一遍。
`pnpm run build` 构建原生发布包（`scripts/release.mjs`）。workspace
`[profile.release]` 使用 thin LTO、`strip` 和 `panic = "abort"`。

本地使用与 CI 一致的 Node.js 22。共用 `target/` 的 Cargo 测试、Clippy、契约生成和
原生构建应顺序执行，保持构建配置一致以复用编译结果。修复后重跑受影响检查，最终
main Quality 承担完整发布门禁。各项检查在本地与 CI 之间的分工见[发布流程](releasing.zh-CN.md)。

## DSH 插件契约测试与真实冒烟

`pnpm run test:dsh:plugin` 就是 `pnpm run test:tooling` 已经运行的那份隔离插件契约测试（`scripts/dsh-plugin-package.test.mjs`）。
它不会启动 DSH、Gateway，也不会走任何依赖真实凭据的路径。

下面两条是**手工验收冒烟**，需要本机真实的 DSH CLI（报告实际安装的版本，不固定版本号），和/或本地编译的
`target/debug/ocg-manager-cli`。它们不属于 `pnpm run test`、`pnpm run test:web`、
`pnpm run test:tooling` 或 CI。

```bash
pnpm run smoke:dsh:plugin
pnpm run smoke:dsh:cli
```

`smoke:dsh:plugin` 使用已安装的 DSH CLI（Windows：`%APPDATA%/npm/node_modules/@deepseek-ai/dsh/lib/bin.js`），
配合隔离的 `DSH_HOME` 和本机 loopback 的 models/chat 桩。`smoke:dsh:cli` 对带
`dsh-local-host` 的原生 `ocg-manager-cli serve` 调用 `GET|POST|DELETE /dashboard/api/v4/applications/dsh`。
若 CLI 构建时未启用该能力，加 `--expect-unsupported`；若要覆盖相对路径的 `--data-dir` /
`DSH_HOME`，加 `--relative-roots`。默认冒烟检查 Profile 发现、验证已停止的 Web Profile 需要运行会话，并通过 OCG 验证 Web HTTP 安装生命周期；
运行 `node scripts/dsh-headless-cli-smoke.mjs --scan-user-homes` 可验证隔离的
`.dsh` 与 `.dsh-editor` 两个 Home 之间的目标选择。

`node scripts/dsh-web-runtime-smoke.mjs --ocg` 通过 OCG V4 API，在隔离的已安装 DSH Web 运行时中验证安装、替换和卸载。加 `--desktop` 可验证已安装的官方 Desktop Host。这些冒烟使用临时 Profile，不调用真实模型供应商。

Rust 单元测试放在同名子模块：`src/db.rs` 声明 `mod tests;`，测试正文在
`src/db/tests.rs`。不要写断言源码文本、工作流 YAML 或文档正文的测试。

CLI 沙箱（只创建 OpenCode Go 卡；不能创建 Custom、子 Key 或设置）：

调试构建的 `serve` 不会自动同步用户 skill。原生正式版 `serve` 和桌面启动会同步内置 skill；正式版冒烟应使用隔离的 `USERPROFILE`（Windows）或 `HOME`（macOS/Linux）。显式 `skill sync` 即使在调试构建中也会写入所选用户目录。下面的 Key 是合成测试值，不要把真实秘密放进 agent 执行的命令参数。

```bash
ocg-manager-cli --data-dir /tmp/ocg-cli-test key add smoke sk-smoke
ocg-manager-cli --data-dir /tmp/ocg-cli-test serve --port 19042
```

直接 `Database::update_account` 不 bump revision；这是有意的，也不是 CLI
路径。

## 本地未签名冒烟（Windows）

从托盘退出已安装的 release。对齐 `package.json`、
`src-tauri/tauri.conf.json`、两份 `Cargo.toml` 和 `compose.example.yaml`
中的版本，然后运行 `pnpm run build`。

没有 `TAURI_SIGNING_PRIVATE_KEY` 时只生成普通本地包，不能用于应用内升级。
可选签名变量：`TAURI_SIGNING_PRIVATE_KEY`、
`TAURI_SIGNING_PRIVATE_KEY_PASSWORD`、`TAURI_UPDATER_PUBLIC_KEY`（必须匹配
`src-tauri/updater-public-key.sha256`），以及
`OCG_REQUIRE_UPDATER_ARTIFACTS=1`。

本地 Tauri 构建可能改写 `src-tauri/Cargo.toml` 与
`src-tauri/gen/schemas/*.json`——只保留有意修改。

## 请求调试与日志分级

`pnpm run dev` 同时默认设置 `OCG_DEBUG_REQUESTS=1`、`OCG_LOG_LEVEL=debug`，
以及 `OCG_DEBUG_DIR=<仓库>/.artifacts/debug-requests`。启动输出会显示实际路径和级别。
显式环境变量可覆盖默认值；设置 `OCG_DEBUG_REQUESTS=0` 可关闭落盘。
普通 CLI／已安装应用启动默认不保存完整请求，日志级别为 `info`。

通过鉴权且未超过大小限制的推理 POST，会在协议解析前保存一份 `client` JSON；
每次上游尝试会在协议转换、请求规范化之后，最终鉴权检查之前保存一份 `upstream` JSON。
文件通过响应头 `x-ocg-request-id` 关联，包含尝试编号、URI、请求头和完整 JSON 正文，
保留消息、工具和内嵌媒体，凭证字段和已知鉴权密钥会脱敏。存在上游文件表示请求已准备，
不保证实际已发出。非法 JSON／二进制正文只保存长度和哈希，并明确标记
`invalid_json_omitted`；未鉴权及超限正文不保存。响应正文和 SSE 不落盘、不缓冲。
这是保留正文内容的调试记录，不是逐字节 HTTP 抓包。

文件包含私人会话内容。默认目录已被 Git 忽略；自定义目录也应保持私有。
Windows 文件继承目录 ACL，Unix 新目录／文件权限分别为 0700／0600。
文件完整写入后才重命名发布，落盘失败会警告但不阻断转发。
每次成功写入后删除最旧的本功能记录，保留最近 1,000 个文件，客户端和上游分别计数；
这是文件数量上限，不是字节配额。`.partial` 是未完成文件，不能视为完整记录。

`OCG_LOG_LEVEL` 在启动时指定运行日志的最低保存级别：`trace`、`debug`、`info`、
`warn` 或 `error`。`trace` 增加不含正文的请求结构及指纹；`debug` 增加请求接收、
上游尝试准备、上游响应头及等待时间；`info` 包含响应就绪和尝试结束状态；
`warn`／`error` 记录拒绝和失败诊断。响应就绪不代表流式结束，流的最终结果由尝试结束记录体现。
已有生命周期及控制面事件使用相同阈值，请求统计和计费记录不受分级过滤影响。
“日志 → 运行日志”支持级别、分类和请求 ID 筛选，先筛选，再取最近 200 条。


---

[维护者指南索引](../MAINTAINER.zh-CN.md) · [English](development.md) · [文档索引](../README.zh-CN.md)
