[English](protocol-conversion.md)

# 协议转换

Open Console Gateway 在一个端口上提供四种客户端协议，再把每份请求转换成上游 Plan 所需的格式。转换过程是确定性的：解析 Alias、检查账号资格、应用适配器上限与已保存的供应商合约、检查按模型/按协议的 effective 状态，然后决定透传或转换。显式关闭上游协议的设置优先于基线支持。

协议选择使用该模型已保存的首选协议，以及已经启用、已配置路由、且已授权给所选 Key 的协议。`MODEL_PROTOCOLS` 仍是离线种子和共享别名参考。下表是这份参考，不是实时选择器。

显式刷新目录时导入官方模型列表和协议基线：Go、Zen 使用 Go 文档，Command Code 使用自己的供应商文档，MiniMax CN / Kimi Code CN 使用文档声明的 Chat 与 Messages 家族。抓取失败或文档未列出的模型不新增协议证据，已保存的首选保持原样。已保存的覆盖与探测证据仍受密封适配器约束。刷新和启用操作见[供应商](providers.zh-CN.md)。推理请求不刷新目录，也不为了发现另一种协议而向上游发请求。

每一次尝试在发送前于本地按这个顺序检查候选：已保存的首选协议，只要它属于已启用、已配置且已授权的集合；然后是同样属于该集合的客户端协议；然后是该模型已保存顺序中其余已授权协议。Gemini 只是客户端格式，不会被当作这些上游协议之一。某个候选无法在转换中保留请求所要求的字段时，改看下一个候选。第一个能够保留这些字段的候选就是这一次尝试的协议。Gateway 不通过 HTTP 协商协议，不探测另一种协议，收到上游 HTTP 400 后也不换协议。凭据与供应商重试仍沿用既有策略，之后的尝试会在自己发送前重新做这次本地选择。上游协议与客户端不同时，**响应体**或 SSE 流转回客户端协议。没有已保存首选时，先检查已授权的客户端协议；若它无法保留请求或未被授权，则按已保存的已授权顺序继续选择。它不会被改写成 Chat Completions。

供应商目录中的全部供应商使用同一顺序，包括用户定义的 Configurable HTTP、Custom API、New API 与 Sub2API 关联 Key，以及 CPA。站点本身可以接受多种上游协议时，OCG 仍只使用该模型已保存并已授权的协议。Gateway 不会为了匹配客户端而新增协议、授权或端点。转换覆盖文本、system、图像、工具调用与结果、推理内容、完成状态、错误与 usage 字段。SSE 用量、错误和终止状态按事件顺序解析，支持同一次响应混用 LF 与 CRLF 事件分隔符。

`GET /v1/models` 上的公开配置见[模型元数据](model-metadata.zh-CN.md)。`ocg.protocols` 列出合格路由上已授权的上游协议。它只在这次响应里推导，不是已保存的协商表。

## 原生不透明历史

已签名的 Messages thinking、redacted thinking `data`，以及加密的 Responses 内容带有无状态标记。前缀为 79 个 ASCII 字节：`ocg-replay-v1:`、64 位十六进制和 `:`。后面是未经改写的原始不透明字节。Gateway 用 SHA-256 哈希这次实际观察到的路由：适配器、上游协议、上游模型、实际请求 URL、凭据 id、凭据版本、目的地、授权连接、绑定和鉴权方式。标记不含 Key、其他秘密、数据库记录或 HMAC。标记只建立与这一条已配置、实际观察到的路由的相等关系。已签名或加密的内容仍由上游校验。

Gateway 在发出 HTTP 请求之前检查标记。路由不符、非空且没有标记的不透明历史、未知版本，以及嵌套或畸形的标记都会被拒绝。Gateway 不会删掉这些字段来让请求继续，也不会把旧对话猜成一种兼容历史。普通文本和工具调用仍然可以跨路由携带。仍走 Messages 的 Messages 请求里，signature 缺失、为 null 或为空的 thinking 在校验和 system 角色提升之后保留原块。这段原生历史不会被静默删除。不带路由标记的旧转换辅助仍会去掉这些未签名块。原生客户端可以按自己的 SDK 把未签名 thinking 变成纯文本。JSON 与 SSE 都把已签名字段留在产生它的那条路由上。若脱敏会破坏已签名的原生内容，Gateway 返回错误，而不是返回一份改写后仍然成功的历史。

标记只在原生客户端转发这段不透明历史时保护它。外部客户端负责会话的序列化。供应商、API 或模型身份变化时，它们的 SDK 可以把原生不透明字段改成纯文本，也可以丢掉该字段。OCG 无法恢复一个从未到达的字段，也无法发现这次丢弃。只有 DSH 插件在基础适配器改写外来的已签名 assistant 消息之前做严格预检。切换协议分组或客户端配置不会迁移旧会话。原生身份变化时，需要新开会话，或者发送已经解析好的历史。带标记的历史确实到达时，同一条已配置路由就是这项保证的边界：上游模型、端点或凭据版本的直接变化会在发出 HTTP 之前拒绝。

| 别名参考偏好 | 模型 |
| --- | --- |
| OpenAI Chat Completions | `glm-5.3-flash`、`glm-5.3`、`glm-5.2`、`glm-5.1`、`glm-5`、`kimi-k3`、`kimi-k2.7-code`、`kimi-k2.6`、`kimi-k2.5`、`deepseek-v4-pro`、`deepseek-v4-flash`、`deepseek-v4-flash-vision-exp`、`mimo-v2.5`、`mimo-v2.5-pro`、`hy3`、`longcat-2.0`、`big-pickle`、`deepseek-v4-flash-free`、`mimo-v2.5-free`、`nemotron-3-ultra-free`、`nemotron-3.5-lightning-free`、`ling-3.0-flash-fin-free`、`hy4-preview` |
| OpenAI Responses | `grok-4.6`、`grok-4.5`、`gpt-5.6-luna`、`muse-spark-1.2`、`muse-spark-1.2-contributor`、`muse-spark-1.2-contributor-free`、`muse-spark-1.3-contributor-free` |
| Anthropic Messages | `minimax-m3`、`minimax-m2.7`、`minimax-m2.7-highspeed`、`minimax-m2.5`、`minimax-m2.5-highspeed`、`qwen3.8-max`、`qwen3.8-flash`、`qwen3.7-max`、`qwen3.7-plus`、`qwen3.6-plus`、`qwen3.5-plus` |

别名配置参考（检入的 2026-09-06 偏好及 2026-08-27 Go `live_supported` 路径）。✓ 表示代码配置中记录了该协议，不保证当前直接透传。模型和协议是否可路由由 Provider 目录与 effective 合约决定。参考配置位于 `crates/ocg-domain/src/protocol.rs` 的 `MODEL_PROTOCOLS`。

`reasoning.effort` 别名只在携带该兼容策略的 OpenCode Go 路由上、于转发或转换前应用：`muse-spark-1.2`、`muse-spark-1.2-contributor`、`muse-spark-1.2-contributor-free` 与 `muse-spark-1.3-contributor-free` 把 `max` 映射为 `xhigh`（上游拒绝 `max`）。用户定义的 HTTP 路由即使上游模型同名也保留原值。其他模型的 `reasoning.effort` 原样透传。

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

Gemini 是客户端格式：Gateway 把 `contents`、纯文本 `systemInstruction`、受支持的 `inlineData` 图片、`functionDeclarations`、函数调用/结果、JSON Schema 输出、生成选项、Google 错误信封、usage 元数据和 SSE 帧，转换到这次尝试选定的上游协议并转回。`v1beta` 与 `v1` 两种 URL 形式都接受。

无法转换的字段返回 `400`：

- 非空 `safetySettings` 无法跨协议执行同一套内容安全阈值，直接返回 `400 INVALID_ARGUMENT`；省略、`null` 或空数组可以使用。`safetySettings` 只影响 Gateway 是否接受请求，不会作为上游执行的提示生效。
- 非空的 `generationConfig.topK` 或 `generationConfig.thinkingConfig` 在发出 HTTP 之前拒绝。转换没有这两种值的精确形式。
- 其他无法跨协议保留的非空生成选项（包括 `seed`、presence/frequency penalty、logprobs 与 media resolution）会返回 `400`，不会静默丢弃。
- `cachedContent`、`fileData`、Google Search、URL Context、Code Execution、多模态 function response、function response 的 schema/behavior、`VALIDATED` 函数调用模式、`candidateCount` 大于 1、非 TEXT 输出模态会返回 `400`。图片请改用 base64 `inlineData`，支持 PNG、JPEG、GIF、WebP。
- `countTokens` 与 `embedContent` 返回 `501 UNIMPLEMENTED`；Gemini CLI 对前者失败可使用本地估算，Gateway 当前没有 embeddings 路由。

---

[用户指南索引](../USER.zh-CN.md) · [English](protocol-conversion.md) · [文档索引](../README.zh-CN.md)
