[English](provider-presets.md)

# Plan 与 API 预设

创建预设或显式重新套用预设时，系统会保存文档所列的整组路由。固定地址的预设不会从运行时主机名推导替换路径；更新 OCG 也不会改写已保存的连接。Azure 和 Bedrock 需填写资源或地区专属的 Responses URL。地址符合官方 Azure OpenAI 或 Bedrock Runtime 的主机与路径格式时，OCG 会补齐同一资源的其他已确认路由；自定义主机或路径则保留为一条可手动编辑的路由。Azure 静态 API Key 使用 `api-key` 请求头，Microsoft Entra 令牌使用 Bearer。Bedrock 没有兼容的 `GET /models`，需自行填写模型 ID。Gemini 原生 Google API 不属于这三种上游格式，因此预设使用官方列明的 OpenAI 兼容接口。

要更新早期预设创建的连接，进入**供应商 → 编辑连接**，点击**采用预设协议**，检查受影响的 Key 与地址后保存。仅更新 OCG 不会改写已有路由和授权。

MiniMax 国内/API 与国际预设会保存独立的 Chat Completions、Responses 和 Messages 路由。Chat 和 Responses 使用 Bearer；Messages 使用 `x-api-key`。完整端点会被保存，面板不会猜测其他路径。MiMo API 与 MiMo Token Plan（国内）同样保存 Bearer 鉴权的 Chat `/v1/chat/completions`、Responses `/v1/responses` 和 Messages `/anthropic/v1/messages` 三条路由。选择规则见[协议默认值](providers.zh-CN.md#协议默认值与连接测试)。

从 **账号 → 新增账号** 或 **供应商 → 添加供应商** 浏览 Plan/API 预设。两个按钮打开同一套账号页选择器。已有连接只列出仍有账号的内置供应商和已保存的用户定义供应商；未使用的内置模板与创建模板一起出现在新服务里。预设按厂商分组，地区或套餐变体使用紧凑选择器；搜索包含厂商、变体、预设名称与 ID，以及端点主机。选择器在 Key 字段之前显示只读连接摘要。固定预设提供地址、协议、鉴权与可修改的默认模型；Azure、Bedrock 仍需填写资源或地区地址，以及部署或模型信息。完成预设必须填写 Key，并同时创建供应商和首个账号；**保存草稿** 可以省略 Key。Custom API 与手动配置仍保留完整设置。

预设是构建期数据，不是逐厂商的代码：`crates/ocg-domain/build.rs` 在构建时把 `resources/provider-presets.json` 编译成静态的 `PRESET_OFFERINGS` 表，面板直接渲染该表。新增或修正预设就是改这份 JSON；本页只描述行为，不逐行复制内容。

预设通过 Dashboard V4 onboarding commit 原子保存为普通用户定义供应商，参与账号排序、故障回退、模型路由和请求日志，保存后仍可编辑。它不会新增独立适配器，也不会在保存前自动创建账号。

从预设导入模型时，对外名称只取最后一段：`vendor/model` 显示为 `model`；准确上游 ID 仍为 `vendor/model`。两个字段都可编辑。如果不同上游 ID 的最后一段相同，保存前请为它们指定不同的对外名称。

升级时，OCG 会将已保存且未经修改的预设生成名称改为最后一段。手动改过名称或最后一段重名的映射保留原名，供你检查。对外模型名更改后，仍使用旧前缀名称的客户端也需要更新模型设置。

切换预设会清除上一渠道的 Key 和模型映射，再填入所选渠道的默认模型。链接指向运营方文档、控制台，不携带 CC-Switch 推广参数。获取模型只修改草稿；模型测试可能收费，仍需现有的明确确认。请选择支持当前上游协议的模型；仅用于图片、音频、视频或向量嵌入的模型不属于此对话网关预设流程。

默认模型取自对应运营方的模型文档或调用示例，是可调整的起点，不代表已验证你的账号权限。保存不会调用上游、自动获取全部模型或进行付费测试。模板更新不会改写已有供应商的连接；上文所述的 v64 名称迁移是一次性例外。

## 拉取目录、测试与重新编辑

获取模型只更新草稿候选列表。空结果和截断结果会明确提示；选择要导入的模型后再保存。目录拉取成功不代表每个模型都接受所选协议。

选择模型后执行**测试模型**，使用模型显式覆盖的协议和地址；没有覆盖则继承供应商默认配置。成功要求响应符合所选协议，不能只凭 HTTP 200 判定，也不能证明该供应商的流式、工具调用等高级功能已经通过验证。

测试不会改变路由或优先级，**保存供应商**才提交你编辑的配置。可配置 HTTP 测试使用所选连接的有效路由，不读回或替换已保存的 Key。关联平台的 Key 继续遵守端点由父账号管理的约束。普通动态与迁移后的 Custom HTTP 连接都可使用连接所有的按模型协议/地址覆盖。

保存后重新编辑会保留所选预设，包括 Azure 等自填资源地址的场景。这用于保留模板提示和目录拉取限制，不代表修改后的端点已经被认证为官方地址。没有预设来源的记录只使用无歧义的既有配置线索，不重写手动模型名称。修改端点、Key、模型或协议会清除过时的测试结果。

## 覆盖范围与来源

于 **2026-09-08** 对照 CC-Switch 的 Claude、Codex、Gemini、OpenCode、OpenClaw 与 Hermes [预设源码 `f3b18df`](https://github.com/farion1231/cc-switch/tree/f3b18df12007d0fd79fd8ad8d310880664015197/src/config)。CC-Switch 的分类只用于发现候选，不能直接作为可信判断，例如 Azure、xAI 也可能被标为 third_party。预设的端点与鉴权选择均已按运营方文档核对。

每一条预设是可用的配置模板，不代表已使用真实 Key 完成在线推理，也不代表账号已经获得模型权限。除下文说明的匹配 DeepSeek/智谱官网 API 预设外，新增供应商不自动同步官方额度或余额。产品不会估算请求价格。Coding/Token Plan Key、不同地区 API Key 必须与所选端点匹配，并遵循上游套餐允许的使用范围。

协议路由已于 **2026-09-24** 按各运营方文档核对，表示服务提供的格式，不保证每个模型或每把 Key 都可使用。腾讯 TokenHub、阿里云 Responses、AtlasCloud、PPIO 与 Novita 存在逐模型差异。运营方只公布 Base URL 时，预设的完整 Messages 地址采用标准 `/v1/messages` 后缀。StreamLake 的 Messages 代理地址对应默认模型 `kat-coder-pro-v2.5`，更换模型时需复核地址。Anthropic 的 Chat 兼容接口用于评估，完整 Claude 能力应使用原生 Messages。预填路由就是 JSON 里的 `protocolRoutes`，在你填写资源专属地址后套用。

下文是**能力概览**，不再逐行镜像。权威预设清单是 `resources/provider-presets.json`：每条包含 ID、显示名、协议路由、鉴权方式、运营方文档链接（`docsUrl`）和可修改的默认模型。构建时它会被编译为 `PRESET_OFFERINGS`，应用内选择器实时渲染同一份数据，因此本页不再复制这些字段。

**协议族。**所有预设至少提供 OpenAI 兼容面：Chat Completions 是通用的；Responses 与 Anthropic Messages 按运营方补齐，Messages 路由按运营方不同使用 `x-api-key` 或 Bearer 鉴权（各条的 `protocolRoutes` 记录了具体方案）。多数官方厂商预设三种格式齐全——例如 DeepSeek、Kimi/Moonshot、智谱 GLM、MiniMax、腾讯混元/TokenHub、阿里云百炼/QwenCloud、Volcengine Ark/Doubao、StepFun API、小米 MiMo、OpenRouter、优云智算 Compshare ModelVerse 与 AtlasCloud——一把 Key 可同时服务 Chat、Responses 与 Messages 客户端；Coding 与 Token Plan 变体按运营方文档为该套餐提供的子集预填。少数预设仅保留运营方文档列明的 Chat 面（例如经 OpenAI 兼容端点的 Google Gemini、Z.AI GLM API、NVIDIA API Catalog 与 ModelScope）。Anthropic 相反：原生 Messages 为主，附加 Chat 兼容路由。

**厂商变体与品牌标识。**多变体厂商族（腾讯、智谱、阿里云/QwenCloud、Volcengine/BytePlus、百度千帆、StepFun、小米、MiniMax、StreamLake、SiliconFlow、优云智算）在选择器中集中在同一品牌标识下。`src/assets/provider-logos/` 下有 CC0 资源（Anthropic、Google、DeepSeek、Ollama、NVIDIA、OpenRouter、阿里云、字节跳动、百度）的厂商族会显示该品牌 SVG，其他厂商使用带首字母的染色 monogram 块。内置 Plan Kimi Code CN、MiniMax CN 与 Ollama Cloud 虽不在选择器内，也同样展示厂商品牌标识。

**自填资源地址。**Azure OpenAI v1 与 AWS Bedrock 在填写资源或地区 URL 之前只提供模板；其预设描述该地址对应的文档路由组。

**余额面板。**只有 DeepSeek API 与智谱 GLM API 预设带有下文所述的官网余额行为；其他预设不接入官方额度或余额同步。这些预设都不显示价格表。

**StepFun 点数。**Step Plan（国内）保留手工积分余额，见下文 StepFun 章节。

KAT-Coder 的完整 Chat 地址与 Bearer 鉴权，根据官方 OpenAI 兼容客户端配置推导：Base URL 加标准 `/chat/completions` 后缀；Messages 地址按官方 Claude 代理 Base URL 加 `/v1/messages`。Coding Plan 的用途须符合 [StreamLake 订阅条款](https://www.streamlake.ai/document/DOC/mjzrrkirccgntfkz46)。百灵采用当前官方 `api.ant-ling.com` 域名。

**待核实项：**未能从已获取的官网文档证实 CC-Switch 的百度个人 Token Plan `/v2/tokenplan/personal` 路径，因此不提供此预设。已分别加入官网有据可查的千帆通用 API、已有 Coding Plan 与团队 Token Plan；个人 Key 不应填入团队端点。

## 已有集成与排除项

- **OpenCode Go**、**Kimi Code CN** 和 **MiniMax CN Token Plan** 保留已有内置路由与用量行为。订阅场景继续选择这些供应商；Moonshot、MiniMax API 预设覆盖独立 API 或不同地区场景。
- Azure 使用当前 v1 API，需要填写资源地址和部署名称，不代建资源、不刷新 Entra ID 令牌。Bedrock 使用官方 OpenAI 兼容 API 与 API Key；未实现 AWS AK/SK 签名。
- 不加入 New API / One API / Sub2API 分发站、订阅反代、仅有推广信息的中转渠道，以及运营主体或 API 来源无法确认的服务。仅有 CC-Switch 的 aggregator 标记不足以入选。
- 本次选择了有独立服务文档的聚合或推理平台：OpenRouter、SiliconFlow、NVIDIA、ModelScope、PPIO、七牛、Novita、优云智算和 AtlasCloud；没有整批导入 CC-Switch 的中转目录。
- 浏览器订阅登录、GitHub Copilot OAuth、Codex OAuth、Grok OAuth 不属于 API Key 预设；已有 CPA 集成保持独立。

初始模型 ID 依据运营方文档复核，不直接复制 CC-Switch 快照。你可按当前目录或控制台更换、补充模型，并保留上游 ID 原始拼写。即使没有模型列表接口，也可以填写明确的模型映射后保存。

---

[新增供应商](add-provider.zh-CN.md) · [供应商](providers.zh-CN.md) · [English](provider-presets.md)

## 官网 API 余额

DeepSeek API 与智谱 GLM API 预设可以显示官网余额，但保存的预设、API 类型、鉴权和官网目的地必须仍然匹配。推理继续使用 Configurable HTTP。任意 Custom API、改成中转地址的预设以及 Coding Plan 不会继承这项余额读取。没有参考价格面板，也没有价格刷新。

在 **账号** 页，卡片展示已观测的剩余余额。它不是额度条，也不是按本地价格估算的本月或历史花费。没有读到的余额保持不可用，不会显示成 0。DeepSeek 的 **刷新余额** 仅在点击时用所选 Key 读取官网 `/user/balance`。剩余数字是官网总额；仅当赠送余额大于 0 时才另标赠送（已包含在总额内）。刷新失败保留上次成功结果，不改变启停、认证状态、冷却或路由。换 Key 或修改地址后不会沿用旧余额。智谱明确显示未接入公开余额 API，不生成假余额，也不调用未经证实的控制台接口。

Gateway 不会拉取价格表，也不会用已保存的参考单价估算新请求。没有记录到的费用保持未知，不会显示成零或免费。以前保存的价格留在本机，不会重算。CNY 余额不会换算成美元费用，读到余额也不会扣减钱包。把模型路由改到其他地址后，不会继承这份官网余额。

来源：[DeepSeek 余额](https://api-docs.deepseek.com/api/get-user-balance/)。

## StepFun API (CN) 余额与 Step Plan (CN) 点数

StepFun API (CN) 在 `api.stepfun.com` 的普通路径（不含 `/step_plan`）可以用所选 Key 刷新当前余额，入口与 DeepSeek、Moonshot 相同。官网钱包余额不变。没有读到的余额保持不可用，不会显示成 0。

Step Plan (CN) 在 Open Console Gateway 里没有官网用量 API。账号卡保留手工积分余额。Step 预设是一笔数量和月度续期，不是 token 费率。分桶、授予、月度续期和过期仍然分开，需要改正已保存余额时使用 **校准用量**。完成的请求不会扣减这份余额。产品不要求填写按 token 费率或货币换算。没有记下的数量保持未知，不会记成零或免费。余额为空或未知不会停止路由。剩余不含已过期的授予，卡片也不会编造一个共用重置时间。不需要控制台 Cookie 或登录。

可配置 HTTP 目的地可以保留同一份手工积分余额。Open Console Gateway 不会为它拉取费率文档。
