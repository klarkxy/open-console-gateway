[English](byok-applications.md)

# 本机 BYOK 应用

应用页包含 Codex、Kimi Code、MiniMax Code 和 ZCode 四个固定的原生适配。这是独立自定义供应商配置，与旧版 [Codex 方案](codex-integration-proposal.zh-CN.md) 中的原生登录代理不同。DSH 保留插件接入流程。

已鉴权的 V4 接口是 `GET|POST|DELETE /dashboard/api/v4/applications/byok/{client}` 与 `POST .../{client}/recover`。写入需要当前 revision、process generation 和检查得到的文件指纹。配置接口不再接受 Key、模型选择、元数据覆盖或默认模型选择。控制层持有设置锁，从带鉴权 `/v1/models` 使用的同一发布器导出全部精确公开名称，拒绝空目录，然后创建或复用名称为 `codex`、`kimi-code`、`minimax-code` 或 `zcode` 的已启用普通 Key。DSH 默认使用 `dsh`；可选 `keyId` 保留旧 API 调用兼容性。创建 Key 后立即推进 revision，即使后续原生写入失败也如此。GET 不创建 Key，也不构建模型选择目录。移除和恢复不依赖原 Key 或模型仍然存在。原生 CLI 与 Tauri 注册共享 Host；不含 `dsh-local-host` 的构建返回 `unsupported_runtime`。

不设模型数量上限，不按工具能力过滤，不要求补齐元数据。未知的原生可选上限直接省略，不编造数值。原生模型目录包含配置时的完整发布结果，通过现有更新配置操作刷新；DSH 继续动态发现模型。

## 格式基线

源码对照日期为 2026-09-28。以下提交描述适配器所采用的格式，不代表所有已安装桌面版本都已验证可以加载。

| 客户端 | 源码基线 | OCG 管理的配置 |
| --- | --- | --- |
| Codex | [0.153.4 schema](https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/core/config.schema.json) | `model_providers.ocg`、Responses 协议和私有 Codex `ModelsResponse` 目录 |
| Kimi Code | [配置服务](https://github.com/MoonshotAI/kimi-code/blob/4fbe065442179435c43d3c3dc8d11bb408b3fd30/packages/agent-core-v2/src/app/config/configService.ts) | TOML `providers.ocg` 和 `models."ocg/<公开名称>"`；请求模型仍为精确公开名称 |
| MiniMax Code | [本地供应商写入](https://github.com/MiniMax-AI/minimax-code/blob/2aed5ca703c3359dd028af51e6c4cbc6a5e15c46/packages/config/src/local-model-provider-write.ts) | YAML `custom_provider.ocg`、Chat Completions 和兼容文件锁 |
| ZCode | [文件 codec](https://github.com/zai-org/ZCode/blob/29628c9acdb81b703bbd4080c207a0e7ce5e276e/packages/provider-node/src/provider-config-file-codec.ts) | `schemaVersion: 1`、Personal Provider 与稀疏模型规则、兼容的所有者标记锁 |

Codex 的模型目录属于全局选择，配置时启用 OCG 目录。Host 在既有文件锁与指纹校验范围内，保留仍在发布列表中的 OCG 默认模型，否则选择导出的第一个模型。不要仅因旧 schema 包含字段就使用嵌套 `profiles` 或根 `profile`：当前[配置说明](https://learn.chatgpt.com/docs/config-file/config-advanced)采用独立 Profile 文件，当前 App Server 也拒绝这些旧字段。

0.153.4 的 `ModelsResponse` 反序列化要求每个条目提供 `base_instructions` 或 `model_messages.instructions_template`；仅满足 `ModelInfo` 字段结构并不足够。使用固定保存于 `resources/codex-byok/` 的未修改官方通用兜底指令，来源为 [`codex-rs/models-manager/prompt.md`](https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/models-manager/prompt.md)。原生 CLI 与桌面包均需保留 Apache 许可证和来源说明。升级时应验证完整目录加载器，包括这一外层反序列化要求。

MiniMax 的默认选择采用 `custom_provider:ocg/<公开名称>`。公开名称内部的斜线必须保留：[模型名称解析器](https://github.com/MiniMax-AI/minimax-code/blob/2aed5ca703c3359dd028af51e6c4cbc6a5e15c46/packages/local-runtime-v2/src/service/model-system/resolution/model-key.ts)只按第一条斜线拆分，[供应商前缀](https://github.com/MiniMax-AI/minimax-code/blob/2aed5ca703c3359dd028af51e6c4cbc6a5e15c46/packages/config/src/model-availability.ts)为 `custom_provider:`。

## 所有权与恢复

适配器管理各格式对应的供应商和模型字段，并记录最后写入的值。保留其他内容，拒绝接管同名的非 OCG 条目，不得删除模型却保留指向它的默认选择。只有默认值仍匹配 OCG 写入的选择时，才进行还原。

Host 在自身数据目录保存私有的原始备份、所有权记录和操作日志。恢复前先验证所有当前文件状态与备份哈希，支持 Codex 两份文件处于不同写入阶段的情况。文件与管理记录一起恢复，移除后结束本次所有权。含凭据的文件收紧权限，诊断响应不得携带配置片段或 Key。目标、目录、管理记录和备份路径中的符号链接或重解析点不得使操作越过已检查的路径。

Codex、Kimi 写入前要求用户关闭客户端，其进程内写入服务不提供共享外部锁。MiniMax、ZCode 遵循上游文件锁约定，不接管未知或仍在使用的锁。JSON/YAML 序列化保留无关字段值；MiniMax YAML 注释仅保留在原始备份中。TOML 编辑保留注释。

Host 通过 `runtime_log` 共享的控制台 sink 向上进程的 stderr 逐事件输出一行运维信息：写入完成、带原因类别的拒绝、fingerprint 过期、非 OCG 所有的 `ocg` 冲突、所属字段被外部修改、存在未完成写入，以及回滚完成。消息只包含客户端名和文件路径，sink 不会收到 Key、请求体或带凭据的 URL。拒绝事件只记录 `ByokErrorKind` 而非响应原文，未脱敏的报错文本无法进入日志。面向用户的界面仍以 Dashboard 可见的 receipt 为准。

## 验证

运行 `pnpm run contract:v4:check`、`pnpm run build:web`、BYOK 前端领域/状态/组件测试，以及 Applications 行为测试。原生特性下运行 Rust 的 `dashboard_v4::byok_applications`、`byok_application_host` 筛选测试，并执行相关 DSH 回归。先构建原生 CLI，再运行 `node scripts/byok-applications-smoke.mjs`；脚本只使用隔离目录和模拟凭据。另行验证 no-default-features CLI 的能力边界。

格式解析、配置保存、客户端加载和真实推理是不同的证据。升级适配器时，使用目标源码 schema 检查生成文件。真实桌面激活、工具、附件和多轮推理仍需单独验证，不能用保存成功代替这些行为的验证。

---

[维护者指南索引](../MAINTAINER.zh-CN.md) · [English](byok-applications.md) · [文档索引](../README.zh-CN.md)
