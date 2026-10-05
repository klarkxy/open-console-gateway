[English](development.md)

# 开发

## 前置要求

workspace 的 `rust-version` 为 Rust 1.88 或更新版本，这是锁定依赖的要求。当前分支没有根 `package.json`、Vue workspace 或 Tauri crate。Node 24 用于可选的 `scripts/cli-acceptance.mjs` 检查，以及可选的 `scripts/cli-cpa-acceptance.mjs` 程序。这一代不恢复前端工具链，也不重新引入前端包。CLI 包是 `ocg-cli`，可执行文件是 `ocg`。隐式数据目录是 `~/.ocg3`；下面的命令都传入 `--data-dir`，因此不会打开上一代的 `~/.ocg-mgr-cli` 或已安装的配置目录。这里的 CPA 是 `ocg serve` 拥有的那一个本地运行时。已审阅的制品锁已经存在。这个循环里的 `serve` 仍不是已验收的子进程，因为整份 CLI 验收仍待完成。CLI 未完成前，原生 GUI 继续延后。

## 开发模式

用 Cargo 做无头 CLI 的类型检查、测试和构建。去掉默认 feature 的构建不启用 `dsh-local-host`，本机 DSH 与 BYOK 配置功能不可用；推理、控制面、浏览器和 CPA 仍保留。用默认二进制配合隔离数据目录运行。不要把已安装的数据目录用于开发：

```bash
cargo check -p ocg-cli --locked
cargo test -p ocg-cli --locked
cargo build -p ocg-cli --locked
cargo build -p ocg-cli --locked --no-default-features
cargo run -p ocg-cli -- --data-dir tmp/dev-data serve --port 19042
```

部分 Windows 主机的 HNS/WSL/Docker 保留端口范围包含 `9042`，`19042` 可避开该冲突。当前没有 Rust 源码监视：修改网关 crate 后停止进程并重新运行命令。全新宿主初始化状态时会创建主 Gateway Key。用 `api --output` 把 `GET /dashboard/api/v4/connection` 的响应写到私有文件。[CLI 指南](../user/cli.zh-CN.md) 的请求体使用合成占位符。不要把 Key 放在命令行上。

启用 Git hooks 后，`.githooks` 会在暂存 `*.rs` 时运行 `cargo fmt --all`。

`serve` 拥有本地 CPA 子进程。启动是恢复路径，退出是关闭路径。`POST /dashboard/api/v4/external-integrations/cpa/runtime/stop` 清除已保存的运行意图。恢复失败只报告一次，不循环重试。另一个 CPA 实例不得使用同一个 auth 目录。已审阅的锁已经存在，当前 Go 构建身份是 `011f360291c4c026cee9a57c3707ebe5a975b100f4fe54a500fdb0fb6aa6fe4b`。主生产套件和夹具宿主套件都以退出码 0 结束。清单上的 `fullCLIAccepted` 仍为 false，因此产品监听和子进程验收仍待完成。数字和锁摘要见制品信任一节。更早的 `af3c47e4…` 工作日志，以及 `f3886f65…` 夹具在 `processReload` 上的失败，都是历史。本页不记录一次新的 Go、Rust 或 Node 产品运行。

## 检查

选择能覆盖本次改动边界的最小 Cargo 检查：

| 改动 | 检查 |
| --- | --- |
| 单个 Rust crate | `cargo test -p <package> --locked` |
| Core 行为 | `cargo test -p ocg-core --locked --features ollama-cloud-loopback-test <filter>` |
| CLI | `cargo test -p ocg-cli --locked` |
| CLI 类型检查 | `cargo check -p ocg-cli --locked` |
| CLI 构建 | `cargo build -p ocg-cli --locked` |
| 关闭默认 feature 的 CLI | `cargo build -p ocg-cli --locked --no-default-features` |
| V3 DTO Schema | `cargo test -p ocg-core --locked dashboard_v3::types` |
| V4 DTO Schema | `cargo test -p ocg-core --locked dashboard_v4::types` |

Quality 工作流会加上 `--features ocg-core/ollama-cloud-loopback-test`，以便
Ollama Cloud 网关集成测试安装仅 loopback 的测试接缝。该 feature 默认关闭：应用构建
保持固定的 `https://ollama.com` 源，且不编译该接缝。未开启 feature 的 workspace
`cargo test` 仍会编译 `ollama_cloud_gateway`，但其中用例不会运行。workspace
`[profile.release]` 使用 thin LTO、`strip` 和 `panic = "abort"`。

当前分支的契约门禁是 Rust schema 测试。原来的 `pnpm run contract:v3:check`、
`contract:v4:check`、`build:web` 和 `test:tooling` 属于已移除的前端工程。
不重新引入前端包或工具链。在运行 `cargo run -p ocg-core --example export_dashboard_v3_schema --locked` 和 `cargo run -p ocg-core --example export_dashboard_v4_schema --locked` 之前，已检入的 `schema/dashboard-api-v3.schema.json` 与 `schema/dashboard-api-v4.schema.json` 保持逐字节不变。本页不手工编辑这些文件，也不记录这些运行。`contract_schema()` 已经包含 `RoutingExplanation`；生成器运行时，schemars 会跟随新字段。未来合约工具变更随实际实现进行。发布工作流仍保留历史桌面打包步骤，
它不是当前 Quality 门禁。当前 job 见 [CI](ci.zh-CN.md)。
[发布流程](releasing.zh-CN.md) 仍描述已移除的桌面包。

共用 `target/` 的 Cargo 测试、Clippy 和原生构建应顺序执行，并保持构建配置一致以复用编译结果。修复后重跑受影响检查。

Rust 单元测试放在同名子模块：`src/db.rs` 声明 `mod tests;`，测试正文在
`src/db/tests.rs`。不要写断言源码文本、工作流 YAML 或文档正文的测试。

直接 `Database::update_account` 不 bump revision；这是有意的，也不是 CLI
路径。

## 可选验收

`node scripts/cli-acceptance.mjs` 是针对已构建二进制的独立开发者检查。它不是 `quality.yml` 的 job。脚本自己生成合成请求、隔离的 home 和 cipher。不要传入真实 Key、用户配置目录或已安装的数据目录。本页不记录该脚本的运行结果。它与 `scripts/cli-cpa-acceptance.mjs` 是两件事。两者中任何一个通过，都不是 CPA 运行时验收。

```bash
node scripts/cli-acceptance.mjs target/debug/ocg tmp/cli-acceptance
```

Windows 上的二进制名是 `target/debug/ocg.exe`。省略两个参数时，使用该默认二进制，以及 `tmp/ocg3-cli-delivery/` 下带时间戳的目录。

## CPA 验收程序

可选程序是 `scripts/cli-cpa-acceptance.mjs`。`--binary` 定位一份可执行文件，该路径不是信任。程序不写入 `runtime/cpa/artifact-lock.json`。操作路径已经实现：`prepareNativeOperator`、`runNativeOperatorWorkflows` 和 `settleBindingApply` 在 `nativeAdmission()` 返回空之后运行。拒绝外部的代理缺失、隔离配置目录缺失，或编译模式标志缺失或两个同时给出时，该函数在 serve、监听或子进程之前返回 pending。已审阅的锁已经存在，因此锁缺失不是当前门槛。候选哈希不是信任。`fullCLIAccepted` 不是该程序的字段，也不是信任。整份 CLI 验收和运行时验收仍待完成。Rust feature `ollama-cloud-loopback-test` 选择 CPA 锁变体：feature 关闭（`cfg` 没有该 feature）是 `production`，feature 打开是 `native-loopback-fixture`。同一个 feature 也是检查一节里的 Ollama Cloud 环回接缝。

```text
node scripts/cli-cpa-acceptance.mjs --binary <ocg.exe> --host-dir <variant-directory> [--scratch <profile-uuid-dir>] --test-feature-cli
node scripts/cli-cpa-acceptance.mjs --binary <ocg.exe> --host-dir <variant-directory> [--scratch <profile-uuid-dir>] --production-feature-off
node scripts/cli-cpa-acceptance.mjs --lock-fixtures
node scripts/cli-cpa-acceptance.mjs --native-fixtures
node scripts/cli-cpa-acceptance.mjs --list
node scripts/cli-cpa-acceptance.mjs --prerequisites --binary <ocg.exe> --host-dir <variant-directory>
```

| 参数 | 它定位什么 | 它不做什么 |
| --- | --- | --- |
| `--binary` | 这次运行的 `ocg` 可执行文件。Windows 名称是 `ocg.exe`。 | 它不选择受信任变体。路径不是锁。 |
| `--host-dir` | 含该变体 `manifest.json` 和 `ocg-cpa-host.exe` 的目录。Go API 中的生产默认目录是 `runtime-build`。夹具默认目录是 `runtime-build/native-loopback-fixture`。 | 它不选择信任，不从该目录读取锁，也不接受 `fullCLIAccepted`。 |
| `--scratch` | `tmp/ocg3-cli-delivery/orchestration-20261004/acceptance-work/profiles` 下的一个 UUID 目录。 | 它不是凭据配置目录，也不是宿主目录。 |
| `--test-feature-cli` | 把提供的二进制命名为带 Rust `ollama-cloud-loopback-test` 的构建。期望的锁变体是 `native-loopback-fixture`。 | 它不打开该 feature，不写入映射，也不接受夹具字节。 |
| `--production-feature-off` | 把提供的二进制命名为关闭该 feature 的构建。期望的锁变体是 `production`。 | 它不使生产哈希成为信任。 |
| `--lock-fixtures` | 只做内存中的锁用例。 | 没有产品、监听或子进程。 |
| `--native-fixtures` | 进程内的解析器、映射和场景计划检查。这份锁出现之前的一次独立结果是 `blocked-dependency`、退出码 0、可运行 88、待定 44，并且产品运行时未启动。 | 该结果是历史进程内证据。验收仍待完成。 |
| `--scenario`、`--prerequisites`、`--list` | 既有阶段运行器。 | 它们自身不放行原生产品执行。 |

`--test-feature-cli` 与 `--production-feature-off` 互斥。`--lock-fixtures` 与 `--native-fixtures` 是分开的命令。没有 `--proxy` 参数。调用者提供的代理 URL 不是资产，也不能放宽拒绝规则。没有任何参数写入或替换锁。唯一的锁路径是 `runtime/cpa/artifact-lock.json`。该文件已经存在。文件缺失时，仍会在 `schema v4` 之前、在 `serve` 之前阻断。

| 编译模式 | 锁 `variant` | 清单顶层 `variant` | 清单 `buildTags` | 操作者必须传入的宿主目录 |
| --- | --- | --- | --- | --- |
| `--production-feature-off`，以及每个非原生阶段 | `production` | `production` | `[]` | 生产输出 |
| `--test-feature-cli` | `native-loopback-fixture` | `native-loopback-fixture` | `["ocg_native_loopback_fixture"]` | 夹具输出 |

记录在规范化后的 `os` / `arch` / `variant` 上唯一。构建器把 `windows` / `win32` 规范为 `windows`，把 `linux` 规范为 `linux`，把 `darwin` / `macos` 规范为 `macos`。它把 `amd64` / `x86_64` / `x64` 规范为 `x86_64`，把 `arm64` / `aarch64` 规范为 `aarch64`。两个变体的清单都带有顶层 `os`、`arch` 和 `executable`。在这棵树上，这些字段是 `windows`、`x86_64` 和 `ocg-cpa-host.exe`。本机的选择是当前平台、`verification` 为 `host-suite`，以及该变体。`compiled-only` 不会变成这一选择。操作系统不是 Windows 的夹具记录会被拒绝。生产与夹具的 Windows host-suite 哈希必须不同。所选记录的 SHA-256 必须同时匹配 `manifest.executableSHA256` 和 `<host-dir>/<record.executable>` 的字节。这个匹配检查的是锁记录。清单上的 `executableSHA256` 本身不是生命周期摘要。候选哈希不写入本页。

两个变体在同一组冻结输入上构建时，携带相同的锁级 `sourceCommit`、`sourceVersion`、`protocolVersion`、`buildIdentity`、`hostSHA256`、`overlaySHA256` 和 `requiredCapabilities`。所选变体的清单必须匹配该身份。不同的输入是不同的来源。占位 SHA `baff0e76f37b32f8e16618b6533f9a435dc36b4bc737302745b4b9353ed45542` 不是记录。仅仅与自身匹配的清单哈希不是记录。`manifest.fullCLIAccepted` 被忽略。

程序在 `127.0.0.1` 上用临时端口打开自己的环回 HTTP 代理。产品设置是既有的手动代理（`proxyMode: manual`），由正常的设置路径写入自有子进程配置。该代理在 DNS 之前、拨号之前拒绝每一个非环回 HTTP 目标和每一个非环回 `CONNECT`。它只转发 `127.0.0.1` 和 `localhost`，以便已经接受的自定义上游阶段继续工作。这不是拨号到供应商的许可。这个代理不在时，原生场景不启动。它们不增加直连供应商的客户端，也不把元数据里 `base_url` 为 `127.0.0.1` 当作授权。

测试资产留在 UUID 临时配置目录下。它们不是已安装的 CLI 主目录。Codex、Claude、Kimi Code 和 Grok CLI 文件通过 `CODEX_HOME`、`CLAUDE_CONFIG_DIR`、`KIMI_CODE_HOME` 和 `GROK_HOME` 放在 `<profile>/home/native/...`。暂存的 CPA 认证是 `<data-dir>/cpa/auth/<basename>.json`，且仅在该目录已经存在之后。端点映射是 `ocg` 子进程上的进程环境 `OCG_CPA_TEST_ENDPOINTS`，且仅在变体准入之后。`--native-fixtures` 不安装该映射。关闭 feature 的准入可以安装它，只为证明生产子进程会忽略它。未知键、重复键、键 `cpa` 和畸形源会在构建子进程环境之前被拒绝。Antigravity 发现保持 `supported: false`，产品原因是 `Antigravity local credential storage is not compatible with this import; use login.` 该程序不调用 OAuth start。

普通 CLI 没有 `fakeReady` 或 `policyReady` 开关。单元测试调用 `artifact.rs` 里的私有注入字节辅助函数。那些辅助函数不是产品入口。解析器测试通过不是已接受的宿主。经 Node 检查的夹具测试不是运行时验收。

同一个 `serve` 进程拥有启动和关闭，如开发模式一节所述。`install`、`rollback` 和 `apply` 不读取 `OCG_CPA_BASE_URL`，包括畸形值。该变量不重定向自有宿主，也不阻断这三项操作。已保存的历史行和字节继续保留。显式的遗留拒绝留在[面板 API](dashboard-api.zh-CN.md#自有-cpa-控制)的控制路由上。

## 制品信任与证据等级

产品信任是编译期嵌入的 `crates/ocg-core/../../runtime/cpa/artifact-lock.json`，即仓库文件 `runtime/cpa/artifact-lock.json`。在写入这份已审阅的锁之前，文件缺失会导致编译失败。空文件、非 UTF-8 或 schema 非法的嵌入在 `verify` 时失败关闭。产品不从磁盘或 `OCG_CPA_HOST_DIR` 加载锁，也不接受清单 `executableSHA256` 作为摘要。`PINNED_SHA256`（`baff0e76f37b32f8e16618b6533f9a435dc36b4bc737302745b4b9353ed45542`）仍定义在 `artifact.rs` 中。它不是已接受的摘要，也不是回退。`selected_trusted_sha()` 是唯一的产品生命周期摘要。`verify(dir)` 是唯一的产品清单校验器。两者读取同一份嵌入、同一套 `parse_lock` 规则，以及同一组规范化的 `(os, arch, variant)`。`resolve_dir`、显式目录和 `OCG_CPA_HOST_DIR` 只定位 `manifest.json` 与可执行文件字节。清单所在位置不是信任。

`apply` 在渲染子进程之前调用 `load_artifact` 和 `artifact::install`。`load_artifact` 是对显式目录或 `OCG_CPA_HOST_DIR` 做 `resolve_dir`，然后 `verify`。`record.artifact_sha256` 是已校验的 `artifact.sha256`。`install` 在创建版本目录之前拒绝每一个不同于 `selected_trusted_sha()` 的摘要，包括历史占位值。`skip_spawn` 只跳过 `launch` 内部的子进程。`synthetic_ready` 复制 `record.artifact_sha256`，不选择信任。`accept_ready` 和 `accept_restored` 仍要求该摘要等于 `selected_trusted_sha()`。`write_managed` 把 `asset_sha256` 存为 `selected_trusted_sha()`。当前版本字符串保持 `PINNED_VERSION_CANONICAL`（`8.0.10`）。配置文件不存在时，`write_managed` 在读取摘要之前返回。`owned_device_launch` 要求 `artifact_sha256` 与 `selected_trusted_sha()` 精确、区分大小写地相等，然后 `installed_executable` 检查标记、常规文件、重解析点、权限和流式字节。文件存在本身不够。不同的摘要，包括历史占位值，返回 `pinned CPA executable is not installed`。`execution_report` 和非跳过的启动路径继续对已保存的摘要调用 `installed_executable`。它们不另造第二个摘要。`version_accepted` 仍返回 `pinned CPA install only accepts v8.0.10`。

源码常量，而不是已接受的运行时，是 `PINNED_COMMIT` `6fecc6e5567912661654a4eaf9b8f5436facd1c2`、`PINNED_VERSION` `v8.0.10`、`PINNED_VERSION_CANONICAL` `8.0.10`、16 个 `REQUIRED_CAPABILITIES`，以及锁 schema 1。在这棵 Windows 树上，Linux 与 macOS 记录保持 `compiled-only` 证据。`verification` 是 `host-suite` 或 `compiled-only`。两者仍要求字节哈希。

| 证据 | 它确立什么 |
| --- | --- |
| 源码 | 候选 Rust 与程序文本。根锁已经存在。该文件缺失时，嵌入仍会导致编译失败。 |
| 单元 | 私有的注入字节辅助函数。它们不是产品入口。解析器通过不是已接受的宿主。`verified_ready` 不能由单元测试设置。播种数据库再调用 `explain` 不是在线 Ready 证明。 |
| Host-suite | 所选平台和变体上、锁的 `verification` 为 `host-suite`，仍带字节哈希。在这棵 Windows 树上，这是 Windows 记录。操作系统不是 Windows 的夹具记录会被拒绝。 |
| CLI | `ocg` 的命令行为。解析器退出码 0 不是 CLI 验收。整份 CLI 验收仍待完成。 |
| 运行时 | 已审阅的锁、匹配的宿主字节，以及产品监听加上子进程。锁已经存在。监听和子进程验收仍待完成。经 Node 检查的夹具测试不是这一级。 |
| Compiled-only | 锁的 `verification` 为 `compiled-only`，仍带字节哈希。它不是交互验收。在这棵 Windows 树上，Linux 与 macOS 记录留在这一级。 |

当前的制品与编译事实：

- 已审阅的根锁 `runtime/cpa/artifact-lock.json` 已经存在。它是 schema 1、UTF-8、四条记录，文件 SHA-256 是 `bf89d3a47323fc17281a42ee4d7874e03249cfe27dc765297055b65dde3a7839`。构建身份是 `011f360291c4c026cee9a57c3707ebe5a975b100f4fe54a500fdb0fb6aa6fe4b`。记录是 Windows `x86_64` 生产 `host-suite`、Windows `x86_64` 夹具 `host-suite`、Linux `x86_64` `compiled-only`，以及 macOS `aarch64` `compiled-only`。`selected_trusted_sha()` 仍是唯一的产品生命周期摘要，`verify(dir)` 仍是唯一的产品清单校验器。没有放宽的校验器，也没有调用者提供的信任。清单上的 `fullCLIAccepted` 仍为 false。这份锁不是整份 CLI 声明。
- 两份当前清单都记录 `windows`、`x86_64` 和 `ocg-cpa-host.exe`，源提交 `6fecc6e5567912661654a4eaf9b8f5436facd1c2`，源版本 `v8.0.10`，以及同一个构建身份。生产清单还记录仅编译的 `linux` / `x86_64` / `ocg-cpa-host-linux-amd64` 和 `macos` / `aarch64` / `ocg-cpa-host-darwin-arm64`。夹具清单不记录其他平台。Go 源码审阅 `tmp/ocg3-cli-delivery/orchestration-20261004/runtime-build/registration-mutation-fence-independent-review.md` 对这个身份的结论是 READY。`tmp/ocg3-cli-delivery/orchestration-20261004/runtime-build/primary-registration-mutation-suite-exits.json` 里的主套件退出码都是 0：生产宿主 85.060 秒、SDK 0.141 秒，夹具宿主 78.033 秒、SDK 0.123 秒。该记录里的 `wholeCLIAccepted` 仍为 false。本页不记录一次新的构建器运行。
- `documented_runtime_dir()` 已在 `cfg(test)` 下实现，只定位生产基目录，或在 `ollama-cloud-loopback-test` 下定位 `native-loopback-fixture`。`verify` 和 `install` 使用编译所选的信任。本页没有运行 `verify`。
- `RoutingResolvedMapping.migrationRequired` 已经实现，包括只有目录的历史远程行。省略该字段的旧载荷为 false。已检入的 schema 文件是 Rust 示例导出。两份已构建 `ocg` 二进制的 `schema v3` 和 `schema v4` 与这些文件一致：四次通过，记录在 `tmp/ocg3-cli-delivery/orchestration-20261004/integration-work/primary-compiled-schema-check.json`。检查点 A 构建了两份二进制，并只编译、未运行工作区测试。检查点 B，`task_614235fe74`，正在运行 Rust 断言、Clippy、默认构建、关闭默认 feature 的构建和操作流程。该次通过尚未被接受。本页没有运行 Cargo，也没有启动产品运行时。线路见[自有 CPA 路由解释](dashboard-api.zh-CN.md#自有-cpa-路由解释)。已保存历史远程行的显式迁移仍未实现。

收口保持为 false。程序的 CLI 参数没有变化。`2026-10-04T10:39:26.282Z` 的独立 `--native-fixtures` 结果和 `2026-10-04T10:39:26.423Z` 的 `--lock-fixtures` 结果早于这份锁。它们记录真实锁不存在、收口为 false，并且没有产品运行时。它们仍是历史进程内证据。`harness-source-receipt.md` 是更早的记录。下面的选择性缺口留在 `native-harness-work/internal-evidence-map.md`。

隔离与恢复发送 PATCH `{enabled:false}` 和 `{enabled:true}`，然后使用既有的 `waitAutomaticApply`。`unchanged` 阶段作为产品依赖而停止。它不把目标凭据描述为缺失。隔离的操作路径没有运行。该程序修正的有界源码审阅已经记录，其中包括一次更早的、退出码为 0 的 `node --check`。产品变更钩子、受信任的宿主字节和整份 CLI 运行时验收仍是分开的前提。

内部证据图保留下面的选择性缺口。它审阅的身份 `f3886f65568469a0e8d4c0a1d56e12f5916415cd7fb328cc9c61c7778f5f2135`，以及其后一次在 `processReload` 上失败、子进程输出为空的夹具全套，都是历史。身份 `011f360291c4c026cee9a57c3707ebe5a975b100f4fe54a500fdb0fb6aa6fe4b` 的两套主套件都以退出码 0 结束。这一对结果取代了更早的 `processReload` 失败，因此 `processReload` 不是当前的受阻原因。当前开放的验收是仍在运行的检查点 B。选择器缺失不是产品能力缺失。不要根据源码或防护证据把这些标签标成操作通过。

- `source-compact`：`TestOCGNativeDispatchGuardFixture` 的共享 compact，文件 `overlay/sdk/cliproxy/auth/ocg_fixture_rewrite_test.go`。直接防护是 `GenerationKindExecute`，没有 compact 执行器，也没有 HTTP。既有上游 `CodexExecutorCompact` 与 `AntigravityCompactionAltResponsesCompact` 测试在已记录的 SDK 运行过滤器之外。它们不证明完整的原生 OAuth 或操作 compact。compact 执行器的执行仍是操作缺口。
- `internal`：`TestOCGNativeDispatchGuard` 中父级与 internal 保持分开的许可，文件 `ocg_dispatch_test.go`。直接 SDK 防护上的 id 与消耗是分开的，没有原生供应商 HTTP。可配置 HTTP 的 `TestReloadStampBlocksHeldFollowup` 和 internal-resend 不能代替原生 internal 执行。
- `refresh-resend`：`TestNativeRefreshResendAndFacts`、`TestNativeOrdinaryRefreshKeepsRegistrationEpoch` 和 `TestNativeValidatedPinRefreshStaysOnPinnedAuth`。已执行的 Go 合成 Codex 非流式令牌刷新、401、第二次发送、材料、epoch、用量和固定认证 A，没有 B，也没有 evil。普通 CLI、真实 OAuth 和每一个供应商仍开放。
- `stream-refresh`：上游 `TestManager_ExecuteStream_UnauthorizedRefreshesCurrentAuthBeforeFallback`。在这张图里只是源码，位于已记录的 SDK 过滤器之外，执行器是 mock。已执行的原生 HTTP 流式刷新和最终 pin 仍开放。
- `original-host`：不匹配和宿主变更不消耗，被标记的错误宿主不会被修复。已执行的是防护拒绝、不消耗和改写，没有物理 HTTP。公开 CLI 不能独立设置子进程的出站 Host。
- `generation-kinds`：正向的 `TestNativeAntigravityExecuteReportsChatPin` 辅助分支。没有已执行的、按种类收窄或撤销授权的测试。辅助分支不是强制执行证据。

在展示 `Codex.StreamBootstrapBuffering` 已启用并且该流实际运行之前，`stream-bootstrap` 保持待定。这一待定行是启用证据。CLI 导入、发现、授权和应用仍是证据缺口。场景列表保持完整。

## 请求调试与日志分级

在 `cargo run` 上自行设置落盘变量。当前分支不会自动打开它们：

```bash
OCG_DEBUG_REQUESTS=1 OCG_LOG_LEVEL=debug OCG_DEBUG_DIR="$PWD/.artifacts/debug-requests" \
  cargo run -p ocg-cli -- --data-dir tmp/dev-data serve --port 19042
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
