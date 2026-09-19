# Gateway Lab

无 UI、仅开发态的 Gateway 测试 CLI：在回环地址上模拟多个上游 endpoint。它不是产品界面，不是守护进程，也不能代替真实 Gateway。

默认是**确定性本地模拟**。只有在 live 场景被显式武装、并且下面两个环境变量都存在时，才会向远端发 OpenAI Chat Completions 兼容请求，模型固定为 `minimax-m3`。

## 命令

```powershell
node tools/gateway-lab/cli.mjs serve --profile default
node tools/gateway-lab/cli.mjs register --gateway http://127.0.0.1:<port> --runtime .artifacts/gateway-lab/<run-id>/runtime.json
node tools/gateway-lab/cli.mjs scenario --runtime .artifacts/gateway-lab/<run-id>/runtime.json --name http_429 --endpoint chat --model upstream-chat
node tools/gateway-lab/cli.mjs reset --runtime .artifacts/gateway-lab/<run-id>/runtime.json
node tools/gateway-lab/cli.mjs verify --cli .\target\debug\ocg-manager-cli.exe --suite local
```

`--suite` 为 `local`（默认）、`live` 或 `all`。

兼容入口（保持原 routing-lab 黑盒套件）：

```powershell
node scripts/routing-lab/run.mjs --cli .\target\debug\ocg-manager-cli.exe
```

## 它做什么

- 只用 Node.js 22 内置 HTTP / fetch / `node:test`。不引入依赖、数据库、容器或 UI。
- 只允许回环监听（`127.0.0.1`、`::1`、`localhost`）。非回环 `--host` / profile 主机会被拒绝。推理口用临时端口；控制面在独立回环端口上提供 reset / script / journal。`scenario` 必须带 `--endpoint`（或 `--listener`）以及 `--model` 或 `--key`；缺选择器是校验错误，不会生成空 isolation key。
- 默认 profile：Chat Completions、Responses、Messages 三个独立 endpoint（各自 Key、各自 `/v1/models`、各自公开模型），以及跨 slot 的同名公开模型 `lab-route`。
- 三层模型身份：公开名（客户端 → Gateway）、上游名（Gateway → 实验室）、live 出站固定 `minimax-m3`。自定义 live 模型是**无效的，不可配置**；profile 与客户端构造会拒绝其它值。返回只改写协议里的模型字段，不改正文或工具参数。
- `register` 只走 Dashboard V4 + CAS（`POST /dashboard/api/v4/onboarding/commit` 和正常启用），不直接写数据库。
- 状态在内存中，重启即重置。报告在 `.artifacts/gateway-lab/<run-id>/`。
- 自动清理只停本进程拥有的 PID 与监听。

## 本地 / 远端证据边界

| 套件 | 远端 HTTP | 通过条件 |
| --- | --- | --- |
| `local` | 必须为 **0** | 只用合成 JSON/SSE，并断言 `remoteCalls=0` |
| `live` | 向 `OCG_LAB_REMOTE_CHAT_URL` 带 `OCG_LAB_REMOTE_KEY` 发 Chat Completions | 以远端真实状态为准；远端失败不得本地伪成功 |
| `all` | 先本地（零远端），再 live（若已配置） | 缺环境变量记为 `NOT_RUN`，不算通过 |

Live 默认：并发 1、`max_tokens` 512、超时 60 秒、单次 verify 最多 24 次远端调用、无自动重试。Chat / Responses / Messages 由实验室自己的小型适配器转换；Gateway 转换是被测对象，不是 oracle。

报告不含 Key 或完整提示词，只保留指纹 / digest。

## 报告状态

`PASS` / `FAIL` / `UNSUPPORTED` / `NOT_RUN`。只有 `PASS` 算通过。任何 `UNSUPPORTED` 或 `NOT_RUN` 都会使进程以 **2** 退出。失败为 **1**。本地报告带覆盖清单，按稳定 `scenarioId` 与 `evidenceKind`（`gateway_black_box`、`rust_integration`、`lab_fixture`、`live_remote`）计分。直连实验室的 fixture 不能充当 Gateway 行为证据。批准矩阵里未实际执行的项记为 `NOT_RUN`，因此同样退出 2。本地 verify 可按参数数组 spawn 精确的 `cargo test -p ocg-core --test <file> <fn> -- --exact` 作为 `rust_integration` 证据。

每条场景记录二进制摘要、预期/实际上游命中、远端次数、可复现命令。

## 不在范围

- 改生产 Rust / Vue / schema / `package.json`
- 修复 Gateway 产品缺陷（只以带复现命令的 `FAIL` 行报告）
- 从磁盘读取或记录真实密钥
- 自动恢复、守护进程、容器、发布集成
- 把 Gateway 协议实现当 live 转换 oracle

## 测试

```powershell
node --test tools/gateway-lab/test/*.test.mjs
node --test scripts/routing-lab.test.mjs
```
