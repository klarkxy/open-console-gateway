[English](protocol-conversion.md)

# 协议转换

Open Console Gateway 在一个端口上提供四种客户端协议，再把每份请求转换成上游 Plan 所需的格式。转换过程是确定性的：解析 Alias、检查账号资格、应用适配器上限与已保存的供应商合约、检查按模型/按协议的 effective 状态，然后决定透传或转换。显式关闭上游协议的设置优先于基线支持。

协议选择使用已保存的供应商合约。显式刷新目录时导入官方模型列表和协议基线：Go、Zen 使用 Go 文档，Command Code 使用自己的供应商文档，MiniMax CN / Kimi Code CN 使用文档声明的 Chat 与 Messages 家族。Go、Zen、Command Code 在文档缺少模型或抓取失败时回退到 Chat。已保存的覆盖与探测证据仍受密封适配器约束。刷新和启用操作见[供应商](providers.zh-CN.md)；推理请求不会刷新目录，也不会通过尝试另一种上游协议来发现支持。

客户端协议在当前模型合约中启用时，请求和响应透传；否则 **请求体** 转到已启用的首选协议，或适配器回退顺序中的第一个可用协议，**响应体** 或 SSE 流转回客户端协议。Gemini 始终转换到已启用的上游协议。供应商目录里的全部供应商（含用户定义 Configurable HTTP，每条 mapping 一个协议）共用这条规则。Custom API 同样转到该账号声明的上游协议，再遵守该端点的合约与按模型覆盖。CPA 不在此转换默认控制范围内，行为不变。转换覆盖文本、system、图像、工具调用与结果、推理内容、完成状态、错误与 usage 字段。SSE 用量、错误和终止状态按事件顺序解析，支持同一次响应混用 LF 与 CRLF 事件分隔符。

下表记录代码内的别名配置，不代表供应商当前可用性。刷新写入的官方基线与已保存的启用状态决定 **供应商** 页显示的实际默认协议和可用协议。

| 别名参考偏好 | 模型 |
| --- | --- |
| OpenAI Chat Completions | `glm-5.3-flash`、`glm-5.3`、`glm-5.2`、`glm-5.1`、`glm-5`、`kimi-k3`、`kimi-k2.7-code`、`kimi-k2.6`、`kimi-k2.5`、`deepseek-v4-pro`、`deepseek-v4-flash`、`deepseek-v4-flash-vision-exp`、`mimo-v2.5`、`mimo-v2.5-pro`、`hy3`、`longcat-2.0`、`big-pickle`、`deepseek-v4-flash-free`、`mimo-v2.5-free`、`nemotron-3-ultra-free`、`nemotron-3.5-lightning-free`、`ling-3.0-flash-fin-free`、`hy4-preview` |
| OpenAI Responses | `grok-4.6`、`grok-4.5`、`gpt-5.6-luna`、`muse-spark-1.2`、`muse-spark-1.2-contributor`、`muse-spark-1.2-contributor-free`、`muse-spark-1.3-contributor-free` |
| Anthropic Messages | `minimax-m3`、`minimax-m2.7`、`minimax-m2.7-highspeed`、`minimax-m2.5`、`minimax-m2.5-highspeed`、`qwen3.8-max`、`qwen3.8-flash`、`qwen3.7-max`、`qwen3.7-plus`、`qwen3.6-plus`、`qwen3.5-plus` |

别名配置参考（检入的 2026-09-06 偏好及 2026-08-27 Go `live_supported` 路径）。✓ 表示代码配置中记录了该协议，不保证当前直接透传。模型和协议是否可路由由 Provider 目录与 effective 合约决定。参考配置位于 `crates/ocg-domain/src/protocol.rs` 的 `MODEL_PROTOCOLS`。

`reasoning.effort` 别名（转发或转换前应用）：`muse-spark-1.2`、 `muse-spark-1.2-contributor`、`muse-spark-1.2-contributor-free` 与 `muse-spark-1.3-contributor-free` 把 `max` 映射为 `xhigh`（上游拒绝 `max`）；其他模型的 `reasoning.effort` 原样透传。

| 模型 | 推荐 | Chat | Responses | Messages |
| --- | --- | :---: | :---: | :---: |
| `grok-4.6` | Responses | | ✓ | |
| `grok-4.5` | Responses | | ✓ | |
| `glm-5.3-flash` | Chat | ✓ | | |
| `glm-5.3` | Chat | ✓ | | |
| `glm-5.2` | Chat | ✓ | | |
| `glm-5.1` | Chat | ✓ | | |
| `glm-5` | Chat | ✓ | | |
| `gpt-5.6-luna` | Responses | | ✓ | |
| `muse-spark-1.2` | Responses | | ✓ | |
| `muse-spark-1.2-contributor` | Responses | | ✓ | |
| `muse-spark-1.2-contributor-free` | Responses | | ✓ | |
| `muse-spark-1.3-contributor-free` | Responses | | ✓ | |
| `kimi-k3` | Chat | ✓ | | ✓ |
| `kimi-k2.7-code` | Chat | ✓ | | |
| `kimi-k2.6` | Chat | ✓ | | |
| `kimi-k2.5` | Chat | ✓ | | |
| `deepseek-v4-pro` | Chat | ✓ | ✓ | ✓ |
| `deepseek-v4-flash` | Chat | ✓ | ✓ | ✓ |
| `deepseek-v4-flash-vision-exp` | Chat | ✓ | ✓ | ✓ |
| `mimo-v2.5` | Chat | ✓ | | |
| `mimo-v2.5-pro` | Chat | ✓ | | |
| `hy3` | Chat | ✓ | | |
| `longcat-2.0` | Chat | ✓ | | |
| `big-pickle` | Chat | ✓ | | |
| `deepseek-v4-flash-free` | Chat | | | |
| `mimo-v2.5-free` | Chat | ✓ | | |
| `nemotron-3-ultra-free` | Chat | ✓ | | |
| `nemotron-3.5-lightning-free` | Chat | ✓ | | |
| `ling-3.0-flash-fin-free` | Chat | ✓ | | |
| `hy4-preview` | Chat | ✓ | | |
| `minimax-m3` | Messages | ✓ | | ✓ |
| `minimax-m2.7` | Messages | | | ✓ |
| `minimax-m2.7-highspeed` | Messages | | | |
| `minimax-m2.5` | Messages | ✓ | | ✓ |
| `minimax-m2.5-highspeed` | Messages | | | |
| `qwen3.8-max` | Messages | ✓ | | ✓ |
| `qwen3.8-flash` | Messages | | | ✓ |
| `qwen3.7-max` | Messages | ✓ | | ✓ |
| `qwen3.7-plus` | Messages | ✓ | | ✓ |
| `qwen3.6-plus` | Messages | ✓ | | ✓ |
| `qwen3.5-plus` | Messages | ✓ | | ✓ |

未知模型名在所有支持的客户端格式上直接返回 `400`——Chat Completions、Responses、Messages，以及 Gemini `generateContent` / `streamGenerateContent`。见 [别名](gateway.zh-CN.md#别名)。

Gateway 协议端点默认最多接受 64 MiB 的 JSON 请求体。可在启动桌面应用、CLI 或容器前设置环境变量 `OCG_MAX_REQUEST_BODY_BYTES`，用正整数指定字节数，例如 `134217728` 表示 128 MiB；修改后需重启进程。无效、零值或超出整数范围的值会产生警告并回退到默认的 64 MiB。此项仅通过环境变量配置，不改变 Dashboard 的请求体上限。

这是传输上限，不是上下文窗口；超限请求返回 `413 Payload Too Large`。提高上限会增加每个并发请求可缓冲的内存，包括认证前的缓冲。若 Open Console Gateway 前面还有反向代理，其请求体上限需至少与 Gateway 配置一致，否则请求可能还没到达 Gateway 就被代理拒绝。

## Responses 是无状态端点

下列字段会直接 `400` 拒绝，不会静默忽略：

- `previous_response_id`
- `conversation`
- `store: true` 或任何不是 `false` 的 `store`
- `background: true`
- `input_image.file_id`（Gateway 没有 Files API）

function、custom、namespace 工具正常转换。`web_search`、`web_search_preview`、`tool_search` 等托管工具无法在转换后的 OpenCode-Go 路径上执行：若它们是唯一工具或被强制使用，Gateway 会在出站前返回 `400`，而不是悄悄去掉后继续生成。若请求里还留有 function 工具，托管声明仍可按带版本标记的 `legacy_compat` 策略丢弃，并记录这次降级；已保存的协议配置不会被改写。原生 Responses 透传会保留托管工具。

## Gemini 是客户端兼容层

Gemini 是客户端格式：Gateway 把 `contents`、纯文本 `systemInstruction`、受支持的 `inlineData` 图片、`functionDeclarations`、函数调用/结果、JSON Schema 输出、生成选项、Google 错误信封、usage 元数据和 SSE 帧，转换到已知模型的 Chat Completions 或 Messages 原生协议并转回。`v1beta` 与 `v1` 两种 URL 形式都接受。

无法转换的字段返回 `400`：

- 非空 `safetySettings` 无法跨协议执行同一套内容安全阈值，直接返回 `400 INVALID_ARGUMENT`；省略、`null` 或空数组可以使用。`safetySettings` 只影响 Gateway 是否接受请求，不会作为上游执行的提示生效。
- `generationConfig.topK` 与 `generationConfig.thinkingConfig` 只作为跨协议兼容提示接受；采样、推理预算和 thoughts 展示不保证与 Google Gemini 等价，实际能力由所选 OpenCode-Go 模型决定。
- 其他无法跨协议保留的非空生成选项（包括 `seed`、presence/frequency penalty、 logprobs 与 media resolution）会返回 `400`，不会静默丢弃。
- `cachedContent`、`fileData`、Google Search、URL Context、Code Execution、多模态 function response、function response 的 schema/behavior、`VALIDATED` 函数调用模式、`candidateCount` 大于 1、非 TEXT 输出模态会返回 `400`。图片请改用 base64 `inlineData`，支持 PNG、JPEG、GIF、WebP。
- `countTokens` 与 `embedContent` 返回 `501 UNIMPLEMENTED`；Gemini CLI 对前者失败可使用本地估算，Gateway 当前没有 embeddings 路由。

---

[用户指南索引](../USER.zh-CN.md) · [English](protocol-conversion.md) · [文档索引](../README.zh-CN.md)
