[English](development.md)

# 开发

## 前置要求

Node.js 22、`package.json` 的 `packageManager` 钉，以及 workspace 的 `rust-version`（锁定依赖要求 Rust 1.88 或更新版本）。原生依赖以 `.github/workflows/release.yml` 在对应 runner 上安装的为准。

## 开发模式

退出已安装的托盘程序，避免占用单实例锁和 `9042` 端口，然后：

```bash
pnpm install
pnpm run dev
```

`pnpm run dev` 以独立的开发默认端口 `OCG_GATEWAY_PORT=19042` 运行 `tauri dev`。部分 Windows 主机的 HNS/WSL/Docker 保留端口范围包含 `9042`，开发默认端口可避开该冲突。安装版仍默认 `9042`。Vite 提供 `http://127.0.0.1:30001/dashboard/`，并把 `/dashboard/api`（含 WebSocket）代理到该 Gateway 端口。启动前设置 `OCG_GATEWAY_PORT` 可同时覆盖 Tauri 与 Vite；变量生效时，设置页以只读方式显示实际端口。

### 选择开发模式

- `pnpm run dev`（默认）：Tauri 监视 Rust workspace，源码变更时重新编译并重启整个桌面应用，进程内网关和所有在途请求都会中断。额外参数会透传给 Tauri CLI：`pnpm run dev -- --no-watch` 关闭 Rust 监视器，已启动的开发构建会持续服务，直到你手动重启；Dashboard 的 Vite HMR 仍然生效，而已保存的 Rust 改动在下一次手动重启时才生效。
- `pnpm run dev:split`：无头 `ocg-manager-cli` 网关 + Vite，不启动 Tauri 进程。适合 Dashboard、HTTP API、路由与协议开发。网关监听 `OCG_GATEWAY_PORT`（默认 `19042`），使用隔离数据目录（`tmp/dev-data`，可用 `OCG_DEV_DATA_DIR` 覆盖），因此可以和已安装的应用并行运行。没有任何 Rust 源码监视：修改网关相关 crate 后，停止脚本并重新运行以重新编译 `ocg-manager-cli`。`http://127.0.0.1:30001/dashboard/` 的 Dashboard 代理到拆分网关，Vue 改动仍然热更新。首次使用全新数据目录时，在私有终端用 `target/debug/ocg-manager-cli --data-dir tmp/dev-data status --show-key` 获取开发 Gateway Key。
- 桌面宿主开发（托盘、自启动、原生浏览器、更新器）仍需 `pnpm run dev`：CLI 不注册这些宿主能力。

拆分网关与 agent 正在使用的任何网关都是独立进程。重启它仍会中断自身的在途流；如果 agent 不能被打断，让它们继续连已安装的应用或另一个常驻实例。

`pnpm install` 会启用 `.githooks`（暂存 `*.rs` 时运行 `cargo fmt --all`）。

升级到支持启动恢复的版本后，手动**启动**一次托管 CPA。启动成功后，运行意图会跨 Tauri 后端重编译保存；新的后端在后台使用原配置和 auth 目录恢复 CPA。**停止**会清除该意图。宿主退出仍清理自己的子进程，恢复失败只报告一次，不循环重启。不要为了绕过开发重启而让另一个 CPA 实例同时使用同一个 auth 目录。

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

`pnpm run test` 是跨前端/Rust 门禁。`pnpm run test:rust`（以及 quality.yml 的 Linux Rust job）会加上 `--features ocg-core/ollama-cloud-loopback-test`，以便 Ollama Cloud 网关集成测试安装仅 loopback 的测试接缝。该 feature 默认关闭：应用构建保持固定的 `https://ollama.com` 源，且不编译该接缝。未开启 feature 的 workspace `cargo test` 仍会编译 `ollama_cloud_gateway`，但其中用例不会运行。

`pnpm run test:tooling` 覆盖 `scripts/*.test.mjs`，已包含在 `pnpm run test` 和 Quality 工作流中；完整测试通过后不用再跑一遍。

`pnpm run build` 构建原生发布包（`scripts/release.mjs`）。workspace `[profile.release]` 使用 thin LTO、`strip` 和 `panic = "abort"`。

本地使用与 CI 一致的 Node.js 22。共用 `target/` 的 Cargo 测试、Clippy、契约生成和原生构建应顺序执行，保持构建配置一致以复用编译结果。修复后重跑受影响检查，最终 main Quality 承担完整发布门禁。各项检查在本地与 CI 之间的分工见[发布流程](releasing.zh-CN.md)。

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

Rust 单元测试放在同级模块中：`src/db.rs` 声明 `mod tests;`，测试正文在 `src/db/tests.rs`。不要写断言源码文本、工作流 YAML 或文档正文的测试。

CLI 沙箱（只创建 OpenCode Go 卡；不能创建 Custom、子 Key 或设置）：

调试构建的 `serve` 不会自动同步用户 skill。原生正式版 `serve` 和桌面启动会同步内置 skill；正式版冒烟应使用隔离的 `USERPROFILE`（Windows）或 `HOME`（macOS/Linux）。显式 `skill sync` 即使在调试构建中也会写入所选用户目录。下面的 Key 是合成测试值，不要把真实秘密放进 agent 执行的命令参数。

```bash
ocg-manager-cli --data-dir /tmp/ocg-cli-test key add smoke sk-smoke
ocg-manager-cli --data-dir /tmp/ocg-cli-test serve --port 19042
```

直接 `Database::update_account` 不 bump revision；这是有意的，也不是 CLI 路径。

## 本地未签名冒烟（Windows）

从托盘退出已安装的 release。对齐 `package.json`、`src-tauri/tauri.conf.json`、两份 `Cargo.toml` 和 `compose.example.yaml` 中的版本，然后运行 `pnpm run build`。

没有 `TAURI_SIGNING_PRIVATE_KEY` 时只生成普通本地包，不能用于应用内升级。可选签名变量：`TAURI_SIGNING_PRIVATE_KEY`、`TAURI_SIGNING_PRIVATE_KEY_PASSWORD`、`TAURI_UPDATER_PUBLIC_KEY`（必须匹配 `src-tauri/updater-public-key.sha256`），以及 `OCG_REQUIRE_UPDATER_ARTIFACTS=1`。

本地 Tauri 构建可能改写 `src-tauri/Cargo.toml` 与 `src-tauri/gen/schemas/*.json`——只保留有意修改。

## 请求调试与日志分级

程序诊断统一通过 `tracing` 写入 stderr，桌面、CLI 和浏览器 worker 在启动前初始化。
`RUST_LOG` 独立控制这一通道：普通启动默认 `warn,ocg=info`，开发脚本默认
`warn,ocg=debug`；显式值覆盖默认值，例如 `RUST_LOG=warn,ocg_core::gateway=trace`
可打开网关的详细事件。无效过滤表达式回退到普通默认值并发出警告。
每行包含时间、级别和来源，请求事件还带请求 ID 和尝试编号。请求正文、Key
和上游错误正文不应进入程序日志。CLI 命令结果和原生启动提示保留现有输出。
这一通道不新增 SQLite 日志存储。

取得单实例所有权的桌面进程，以及解析完数据目录之后的普通本机 CLI，还会追加
`<data-dir>/logs/program.log`。单条已格式化事件若会超过 10 MiB，会先轮转；
保留当前文件和 `program.log.1` 至 `program.log.4`。进程在生命周期内独占
`<data-dir>/logs/.program-log.lock`，并且不删除该锁文件。格式化缓冲最多累积
10 MiB，超出部分按 UTF-8 边界截断并写入 `[truncated]`，然后只脱敏一次，再分别
送给 stderr 和文件。脱敏会去掉终端转义，遮盖凭证和头部字段，遮盖 URL 中的
userinfo，并省略 body 与 payload 的值。stderr 已关闭时仍然写文件。`logs`
或其任一祖先若是符号链接、junction 或其他重解析点，则拒绝文件输出，并且不改
链接目标。已存在的目录和日志文件必须收成私有权限（Unix `0700`/`0600`，或仅
当前用户与 LocalSystem 的受保护 Windows DACL），做不到就关闭文件输出。已经
大于 10 MiB 的 `program.log` 或归档也会拒绝激活，原字节保持不变。
`OCG_PROGRAM_LOG_FILE=off` 时只保留 stderr。Docker 的 `--no-default-features`
CLI 构建不包含 `program-log-file` feature，因此只写 stderr；CPA supervisor、
浏览器 worker 和第二个桌面实例同样不打开该文件。文件锁、打开、收紧权限或轮转
失败时，文件输出自行关闭，stderr 继续可用。

旧的 `OCG_LOG_LEVEL` 环境变量仅用于旧混合记录的兼容数据库写入接口。应用不再向混合的
`gateway_logs` 写入新行。操作回执保存于 `operation_logs`，逻辑请求由原有
`forward_logs` 尝试记录在 SQL 中投影，不新增请求存储。这两个通道不看程序
日志级别。日志页默认显示逻辑请求，用户操作与旧混合历史各自独立。基础历史
没有新增自动清理；已有 `diagnostic_json` 在数据库打开时清理超过 30 天的内容，
基础行与汇总保留。

`pnpm run dev` 默认设置 `OCG_DEBUG_REQUESTS=1` 和
`OCG_DEBUG_DIR=<仓库>/.artifacts/debug-requests`，启动时显示程序过滤表达式
与捕获目录。显式值覆盖默认值；`OCG_DEBUG_REQUESTS=0` 关闭捕获。普通 CLI
和已安装应用默认不保存正文。开发脚本不再默认设置 `OCG_LOG_LEVEL`。

通过鉴权且未超过大小限制的推理 POST，在协议解析前保存 `client` JSON；准备
好的上游尝试在协议转换后、最终鉴权检查前保存 `upstream` JSON，存在该文件
不证明已经发送。记录含请求 ID、尝试编号、URI、头部和 JSON 内容，凭证字段
与已知鉴权密钥脱敏。响应正文和 SSE 不捕获、不缓冲。非法 JSON、二进制或
过大的内容只保存长度、哈希和明确的省略标记。

编码后的附件每个最多 2 MiB，本功能文件合计最多 100 MiB、保留 7 天、最多
1,000 个。启动和写入时清理本功能的普通文件，包括旧 UUID 命名和遗留的
`.partial`；其他文件、链接、子目录保持原样。没有后台清理计时器。进程内互斥
和每次写入持有的文件锁保护跨进程清理与发布，竞争时跳过这次捕获。目录与文件
使用私有权限，Windows 为当前用户与 LocalSystem 的受保护 DACL，空文件在
写入内容前即收紧权限。符号链接和重解析点祖先被拒绝，完整写入后才重命名发布，
失败只发出警告，不改变转发结果。捕获包含会话内容，默认目录被 Git 忽略。

`RUST_LOG` 的 trace 只输出正文长度和指纹；debug 描述接收与准备，info 描述
响应就绪及尝试结果，warn/error 描述失败而不带上游正文。响应就绪不代表流式
结束。请求统计和用量不受这一过滤影响。


---

[维护者指南索引](../MAINTAINER.zh-CN.md) · [English](development.md) · [文档索引](../README.zh-CN.md)
