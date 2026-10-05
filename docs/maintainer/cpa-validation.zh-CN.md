[English](cpa-validation.md)

# CPA 架构决策的验证依据

日期：2026-10-02。结论用于支持[CPA 底层路线](../architecture.zh-CN.md)，不是生产验收或全部厂商支持声明。用户已决定停止进一步实验；同步扩展留给后续实施。

## 冻结环境与证据

使用官方 CPA **v8.0.10** Windows amd64 二进制，tag commit `6fecc6e5567912661654a4eaf9b8f5436facd1c2`。发布压缩包 SHA256 为 `d73c3b78faeb9d4c6e80890f0242146557a5032dfbb788474d06cdb64544b325`，实际二进制为 `01d0523c19e6a659a6b8b89f91c3bc72a8e45d733ac3de6d680346792bf09f0e`。

两个合成 Key、隔离配置/认证目录、回环模拟 GOAT 上游。所有配置的推理目的地均为模拟器；这不是抓包证明或“全部联网为零”的声明，CPA 可能读取官方模型目录。

- [默认行为程序](../../scripts/cpa-goat-acceptance.mjs)：22 个场景，11 项符合所设期望、11 项存在差异；期望同时包含旧 OCG 保证和期望的 GOAT 精确额度，差异不等于 CPA 缺陷评分。
- [插件验证程序](../../scripts/cpa-plugin-acceptance.mjs)、[实验探针](../../scripts/cpa-plugin-probe.c)、[构建脚本](../../scripts/build-cpa-plugin-probe.ps1)：实际加载 C ABI DLL，19 个断言完成，其中包含证明不安全行为的断言。
- 插件 SHA256：`2224a4cae4878cc1b2936c784eaee0f720faf57cfe5955bc55c525b2d2bbcbc9`；插件验证程序：`1f8da7782f14e243fbecb69d6a6afa750e2233cdae645866f995f927d0ee934e`；该轮原始报告：`27902c68f80e44ab8c3d6e85447c81ab0fe0686bd0f25e267005d446bf141c5f`。
- 默认行为原始报告 SHA256：`9179c55e6115c2856977d98f96560a03a439ad538366e2ca1a6dd62399c3ada3`。实验产物保留在本次任务附件中，不把操作者本机路径写成产品路径。

插件使用 cJSON v1.7.19（MIT）与 Windows 构建工具；这是实验依赖，未加入 OCG 产品依赖。构建输出记录下载来源与依赖摘要。实验进程退出、端口关闭、模拟上游关闭均有记录；插件轮次包括重启共 20 个进程。

## 已观察到的能力与缺口

| 边界 | 默认 CPA / 插件实测 | 对新设计的影响 |
| --- | --- | --- |
| 原生协议与流 | 测试覆盖 Chat SSE 与 Messages/Responses 文本转换；已有可见输出后没有向 B 重放 | 复用 CPA；不能外推全部协议/工具调用/厂商支持 |
| 明确 GOAT 截止时间 | 默认冷却没有遵守声明的短/长截止时间；重启后未保留这个 Plan 时间 | 精确额度需要独立的可信证据与准入语义 |
| Key 与模型范围 | 默认 Plan 429 未阻止该 Key 的另一模型；普通 429 本身有 Key 隔离 | Plan 范围不能等同于 CPA 默认模型冷却 |
| 插件额度学习 | usage 包含失败状态、正文和 AuthID；记录后能跨模型阻止 A、到期恢复、重启保留 | 轻插件能够承接部分功能，不需要搬回整个旧内核 |
| 异步学习空窗 | 人为延迟 usage 2.5 秒，另一模型在截止时间记录前复用了 A | 异步回调不能独立提供严格额度保证；不表示每次正常请求都会竞态 |
| 结果未知 | 断连、200 正文丢失、输出前空流触发 A→B，即使 `request-retry: 0` | 必须在 CPA 内部处理选择性不重放 |
| 粗粒度限制 | `max-retry-credentials: 1` 或发送前阻止第二次尝试可以拦住部分重放，也会失去明确 429 的正常换 Key | 不能作为完整产品策略 |
| 配置 stop 规则 | HTTP 500 加匹配正文的 stop 生效；同类配置未停止本轮断连、正文丢失与空流的重放 | 错误规则并未覆盖全部未知结果 |
| hook 失败 | 拦截器错误被跳过；非法 scheduler AuthID 回退原生选择；正常 scheduler 错误和明确 Reject 则能阻止发送 | 强制额度约束不能假设插件失败时自动阻断 |
| 尝试记录 | 失败 A 与成功 B 有各自执行/凭据信息，能关联同一客户端请求 | 用于 OCG 日志投影，不另维护发送账本与重试循环 |

## 可用扩展点与未实现部分

v8.0.10 有请求拦截、scheduler、executor、usage 和完成通知。请求拦截能参与发送前检查；完成通知是异步观察。SDK 还提供 `Hook.OnResult` 与 `ResultPolicy` 类型，但本次未实施或实测它们能否满足完整同步约束。

参考固定版本的[插件接口](https://github.com/router-for-me/CLIProxyAPI/blob/v8.0.10/sdk/pluginapi/types.go)、[生命周期示例](https://github.com/router-for-me/CLIProxyAPI/blob/v8.0.10/examples/plugin/request-lifecycle/README.md)和[SDK 结果接口](https://github.com/router-for-me/CLIProxyAPI/blob/v8.0.10/sdk/cliproxy/auth/conductor.go)。

新设计要求在下一次尝试前同步收到实际凭据身份和执行事实，先发布可信额度限制，再决定是否允许换 Key。SDK 封装或小范围 CPA 补丁是实施选项；实验插件不被直接采纳为生产强制策略。

## 覆盖限制

未使用真实 Go/GOAT 账户，未验证其他操作系统、厂商官方额度 API、账号共享额度、多进程状态、热重载、凭据轮换或生产存储安全。模拟错误正文匹配已知 GOAT 格式，不证明真实服务所有响应都一致。金额/剩余额度未通过 reset 时间推导。

本轮结论支持选择 CPA 和缩减自研责任；不表示 CPA 迁移、同步限额、选择性禁止重放或完整 CLI 网关已经交付。
