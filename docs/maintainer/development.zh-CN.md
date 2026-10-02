[English](development.md)

# 开发

## 前置要求

workspace 的 `rust-version` 为 Rust 1.88 或更新版本，这是锁定依赖的要求。当前分支没有根 `package.json`、Vue workspace 或 Tauri crate。Node 24 只用于可选的 `scripts/cli-acceptance.mjs` 检查。原 pnpm 脚本在恢复前端工程后返回。

## 开发模式

用 Cargo 做无头 CLI 的类型检查、测试和构建。去掉默认 feature 的构建不启用 `dsh-local-host`，本机 DSH 与 BYOK 配置功能不可用；推理、控制面、浏览器和 CPA 仍保留。用默认二进制配合隔离数据目录运行。不要把已安装的数据目录用于开发：

```bash
cargo check -p ocg-manager-cli --locked
cargo test -p ocg-manager-cli --locked
cargo build -p ocg-manager-cli --locked
cargo build -p ocg-manager-cli --locked --no-default-features
cargo run -p ocg-manager-cli -- --data-dir tmp/dev-data serve --port 19042
```

部分 Windows 主机的 HNS/WSL/Docker 保留端口范围包含 `9042`，`19042` 可避开该冲突。当前没有 Rust 源码监视：修改网关 crate 后停止进程并重新运行命令。全新宿主初始化状态时会创建主 Gateway Key。用 `api --output` 把 `GET /dashboard/api/v4/connection` 的响应写到私有文件。[CLI 指南](../user/cli.zh-CN.md) 的请求体使用合成占位符。不要把 Key 放在命令行上。

启用 Git hooks 后，`.githooks` 会在暂存 `*.rs` 时运行 `cargo fmt --all`。

`serve` 在启动时恢复自有的 CPA 运行时，退出时关闭该进程。`POST /dashboard/api/v4/external-integrations/cpa/runtime/stop` 清除已保存的运行意图。恢复失败只报告一次，不循环重试。不要让另一个 CPA 实例同时使用同一个 auth 目录。

## 检查

选择能覆盖本次改动边界的最小 Cargo 检查：

| 改动 | 检查 |
| --- | --- |
| 单个 Rust crate | `cargo test -p <package> --locked` |
| Core 行为 | `cargo test -p ocg-core --locked --features ollama-cloud-loopback-test <filter>` |
| CLI | `cargo test -p ocg-manager-cli --locked` |
| CLI 类型检查 | `cargo check -p ocg-manager-cli --locked` |
| CLI 构建 | `cargo build -p ocg-manager-cli --locked` |
| 关闭默认 feature 的 CLI | `cargo build -p ocg-manager-cli --locked --no-default-features` |
| V3 DTO Schema | `cargo test -p ocg-core --locked dashboard_v3::types` |
| V4 DTO Schema | `cargo test -p ocg-core --locked dashboard_v4::types` |

Quality 工作流会加上 `--features ocg-core/ollama-cloud-loopback-test`，以便
Ollama Cloud 网关集成测试安装仅 loopback 的测试接缝。该 feature 默认关闭：应用构建
保持固定的 `https://ollama.com` 源，且不编译该接缝。未开启 feature 的 workspace
`cargo test` 仍会编译 `ollama_cloud_gateway`，但其中用例不会运行。workspace
`[profile.release]` 使用 thin LTO、`strip` 和 `panic = "abort"`。

当前分支的契约门禁是 Rust schema 测试。原来的 `pnpm run contract:v3:check`、
`contract:v4:check`、`build:web` 和 `test:tooling` 属于已移除的前端工程。
未来恢复前端时再接回 Node 生成器和生成的 TypeScript 检查。发布工作流仍保留历史桌面打包步骤，
它不是当前 Quality 门禁。当前 job 见 [CI](ci.zh-CN.md)。
[发布流程](releasing.zh-CN.md) 仍描述已移除的桌面包。

共用 `target/` 的 Cargo 测试、Clippy 和原生构建应顺序执行，并保持构建配置一致以复用编译结果。修复后重跑受影响检查。

Rust 单元测试放在同名子模块：`src/db.rs` 声明 `mod tests;`，测试正文在
`src/db/tests.rs`。不要写断言源码文本、工作流 YAML 或文档正文的测试。

直接 `Database::update_account` 不 bump revision；这是有意的，也不是 CLI
路径。

## 可选验收

`node scripts/cli-acceptance.mjs` 是针对已构建二进制的独立开发者检查。它不是 `quality.yml` 的 job。脚本自己生成合成请求、隔离的 home 和 cipher。不要传入真实 Key、用户配置目录或已安装的数据目录。本页不记录该脚本的运行结果。

```bash
node scripts/cli-acceptance.mjs target/debug/ocg-manager-cli tmp/cli-acceptance
```

Windows 上的二进制名是 `target/debug/ocg-manager-cli.exe`。省略两个参数时，使用该默认二进制，以及 `tmp/ocg3-cli-delivery/` 下带时间戳的目录。

## 请求调试与日志分级

在 `cargo run` 上自行设置落盘变量。当前分支不会自动打开它们：

```bash
OCG_DEBUG_REQUESTS=1 OCG_LOG_LEVEL=debug OCG_DEBUG_DIR="$PWD/.artifacts/debug-requests" \
  cargo run -p ocg-manager-cli -- --data-dir tmp/dev-data serve --port 19042
```

启动输出会显示实际路径和级别。`OCG_DEBUG_REQUESTS=0` 关闭落盘。普通 `serve` 不保存完整请求，日志级别为 `info`。

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
