[English](provider-presets.md)

# Plan 与 API 预设

默认协议和地址复核于 **2026-09-09**：xAI 使用 Responses，MiniMax 国内/国际使用 Messages 与 Bearer 鉴权，依据运营方当前推荐与 REST 文档。OpenAI/Azure/Bedrock 保留 Responses，Anthropic 保留 Messages，其他预设保留文档中的 Chat 兼容默认值。Gemini 原生 Google API 不属于这三种上游格式，因此预设使用官方列明的 OpenAI 兼容接口。“文档支持”不等于“官方唯一推荐”。模板只初始化新草稿，不改写已经保存的选择。

MiniMax 官方 Chat 与 Messages 地址分别使用 `/v1`、`/anthropic/v1` 前缀。覆盖模型协议时明确填写对应地址，面板不猜测其他路径。[当前 MiniMax Messages 鉴权](https://platform.minimax.io/docs/api-reference/text-chat-anthropic)为 Bearer，不能仅因协议格式兼容 Anthropic 就推断为 x-api-key。选择规则见[协议默认值](providers.zh-CN.md#协议默认值与连接测试)。

从 **账号 → 新增账号** 或 **供应商 → 添加供应商** 浏览 Plan/API 预设。两个按钮打开同一套账号页选择器。已有连接只列出仍有账号的内置供应商和已保存的用户定义供应商；未使用的内置模板与创建模板一起出现在新服务里。预设按厂商分组，地区或套餐变体使用紧凑选择器；搜索包含厂商、变体、预设名称与 ID，以及端点主机。选择器在 Key 字段之前显示只读连接摘要。固定预设提供地址、协议、鉴权与可修改的默认模型；Azure、Bedrock 仍需填写资源或地区地址，以及部署或模型信息。完成预设必须填写 Key，并同时创建供应商和首个账号；**保存草稿** 可以省略 Key。Custom API 与手动配置仍保留完整设置。

预设通过 Dashboard V4 onboarding commit 原子保存为普通用户定义供应商，参与账号排序、故障回退、模型路由和请求日志，保存后仍可编辑。它不会新增独立适配器，也不会在保存前自动创建账号。

在预设下导入模型时，对外名称会加预设前缀，例如 `openrouter/vendor/model`；准确上游 ID 仍为 `vendor/model`，两个字段都可编辑。手动配置的导入不添加预设前缀。

切换预设会清除上一渠道的 Key 和模型映射，再填入所选渠道的默认模型。链接指向运营方文档、控制台，不携带 CC-Switch 推广参数。获取模型只修改草稿；模型测试可能收费，仍需现有的明确确认。请选择支持当前上游协议的模型；仅用于图片、音频、视频或向量嵌入的模型不属于此对话网关预设流程。

默认模型取自对应运营方的模型文档或调用示例，是可调整的起点，不代表已验证你的账号权限。保存不会调用上游、自动获取全部模型或进行付费测试。模板更新不会改写已有供应商的连接和映射。

## 拉取目录、测试与重新编辑

获取模型只更新草稿候选列表。空结果和截断结果会明确提示；选择要导入的模型后再保存。目录拉取成功不代表每个模型都接受所选协议。

选择模型后执行**测试模型**，使用模型显式覆盖的协议和地址；没有覆盖则继承供应商默认配置。成功要求响应符合所选协议，不能只凭 HTTP 200 判定，也不能证明该供应商的流式、工具调用等高级功能已经通过验证。

测试不会改变路由或优先级，**保存供应商**才提交你编辑的配置。Custom API 账号测试其已配置的账号协议；编辑时填写临时测试 Key，不读回或替换已保存的 Key。关联平台的 Key 继续遵守端点由父账号管理的约束。模型覆盖适用于用户定义供应商，账号所有的 Custom 端点仍保持单协议。

保存后重新编辑会保留所选预设，包括 Azure 等自填资源地址的场景。这用于保留模板提示、目录拉取限制和模型导入命名，不代表修改后的端点已经被认证为官方地址。没有预设来源的记录只使用无歧义的既有配置线索，不重写手动模型名称。修改端点、Key、模型或协议会清除过时的测试结果。

## 覆盖范围与来源

于 **2026-09-08** 对照 CC-Switch 的 Claude、Codex、Gemini、OpenCode、OpenClaw 与 Hermes [预设源码 `f3b18df`](https://github.com/farion1231/cc-switch/tree/f3b18df12007d0fd79fd8ad8d310880664015197/src/config)。CC-Switch 的分类只用于发现候选，不能直接作为可信判断，例如 Azure、xAI 也可能被标为 third_party。下表的端点与鉴权选择均以运营方文档核对。

每一行是可用的配置模板，不代表已使用真实 Key 完成在线推理，也不代表账号已经获得模型权限。新增供应商仍为未定价：不同步官方额度、余额和价格。Coding/Token Plan Key、不同地区 API Key 必须与所选端点匹配，并遵循上游套餐允许的使用范围。

下表按厂商族分组，每个预设变体各占一行；多变体厂商（腾讯、智谱、阿里云、Volcengine/BytePlus、百度千帆、StepFun、小米、MiniMax、StreamLake、SiliconFlow、Compshare）的所有变体集中在同一品牌标识下。`src/assets/provider-logos/` 下有 CC0 资源（Anthropic、Google、DeepSeek、Ollama、NVIDIA、OpenRouter、阿里云、字节跳动、百度）的厂商族会在选择器中显示该品牌 SVG，其他厂商使用带首字母的染色 monogram 块。内置 Plan Kimi Code CN、MiniMax CN 与 Ollama Cloud 虽不在选择器内，也同样展示厂商品牌标识。

| 预设 | 上游协议 | 鉴权 | 运营方文档 | 默认模型 |
| --- | --- | --- | --- | --- |
| OpenAI API | responses | bearer | [API 文档](https://developers.openai.com/api/reference/overview) | [`gpt-5.6-luna`](https://developers.openai.com/api/docs/models/gpt-5.6-luna) |
| Anthropic API | messages | x-api-key | [API 文档](https://platform.claude.com/docs/en/api/overview) | [`claude-haiku-4-5-20251001`](https://platform.claude.com/docs/en/models/overview) |
| Google Gemini API | chat_completions | bearer | [API 文档](https://ai.google.dev/gemini-api/docs/openai) | [`gemini-3.8-flash`](https://ai.google.dev/gemini-api/docs/openai) |
| xAI (Grok) API | responses | bearer | [API 文档](https://docs.x.ai/developers/quickstart) | [`grok-4.6`](https://docs.x.ai/developers/models) |
| Azure OpenAI v1 | responses | bearer | [API 文档](https://learn.microsoft.com/en-us/azure/foundry/openai/api-version-lifecycle) | 客户模型／部署 |
| AWS Bedrock (API Key) | responses | bearer | [API 文档](https://docs.aws.amazon.com/bedrock/latest/userguide/bedrock-mantle.html) | 客户模型／部署 |
| DeepSeek API | chat_completions | bearer | [API 文档](https://api-docs.deepseek.com/) | [`deepseek-v4-flash`](https://api-docs.deepseek.com/) |
| Kimi / Moonshot API | chat_completions | bearer | [API 文档](https://platform.kimi.com/docs/api/overview) | [`kimi-k2.6`](https://platform.kimi.com/docs/api/models) |
| Zhipu GLM API | chat_completions | bearer | [API 文档](https://docs.bigmodel.cn/cn/guide/develop/http/introduction) | [`glm-5.3`](https://docs.bigmodel.cn/cn/guide/develop/http/introduction) |
| Zhipu GLM Coding Plan | chat_completions | bearer | [API 文档](https://docs.bigmodel.cn/cn/guide/develop/cursor) | [`glm-5.3`](https://docs.bigmodel.cn/cn/guide/develop/cursor) |
| Z.AI GLM API | chat_completions | bearer | [API 文档](https://docs.z.ai/api-reference/introduction) | [`glm-5.3`](https://docs.z.ai/api-reference/introduction) |
| Z.AI GLM Coding Plan | chat_completions | bearer | [API 文档](https://docs.z.ai/devpack/tool/others) | [`glm-5.3`](https://docs.z.ai/devpack/tool/others) |
| MiniMax API (CN) | messages | bearer | [API 文档](https://platform.minimaxi.com/docs/api-reference/text-anthropic-api) | [`MiniMax-M2.5`](https://platform.minimaxi.com/docs/api-reference/text-anthropic-api) |
| MiniMax API / Token Plan (Global) | messages | bearer | [API 文档](https://platform.minimax.io/docs/api-reference/text-anthropic-api) | [`MiniMax-M3`](https://platform.minimax.io/docs/api-reference/text-chat-anthropic) |
| LongCat API | chat_completions | bearer | [API 文档](https://longcat.chat/platform/docs/api/chat.html) | [`LongCat-2.0`](https://longcat.chat/platform/docs/api/chat) |
| Tencent Hunyuan / TokenHub API | chat_completions | bearer | [API 文档](https://cloud.tencent.com/document/product/1823/130078) | [`hy3`](https://cloud.tencent.com/document/product/1823/132252) |
| Tencent Token Plan (CN) | chat_completions | bearer | [API 文档](https://cloud.tencent.com/document/product/1823/130119) | [`tc-code-latest`](https://cloud.tencent.com/document/product/1823/130119) |
| Tencent Token Plan (Global) | chat_completions | bearer | [API 文档](https://www.tencentcloud.com/document/product/1300/81037) | [`auto`](https://www.tencentcloud.com/document/product/1300/81037) |
| Tencent Token Plan Enterprise Pro (CN) | chat_completions | bearer | [API 文档](https://cloud.tencent.com/document/product/1823/130659) | [`auto`](https://cloud.tencent.com/document/product/1823/130659) |
| Tencent Token Plan Enterprise Pro (Global) | chat_completions | bearer | [API 文档](https://www.tencentcloud.com/document/product/1300/81489) | [`auto`](https://www.tencentcloud.com/document/product/1300/81489) |
| Tencent Token Plan Enterprise Lite (CN) | chat_completions | bearer | [API 文档](https://cloud.tencent.com/document/product/1823/131173) | [`auto`](https://cloud.tencent.com/document/product/1823/131173) |
| Tencent Token Plan Enterprise Lite (Global) | chat_completions | bearer | [API 文档](https://www.tencentcloud.com/document/product/1300/81490) | [`auto`](https://www.tencentcloud.com/document/product/1300/81490) |
| Alibaba Bailian API (CN) | chat_completions | bearer | [API 文档](https://help.aliyun.com/zh/model-studio/base-url) | [`qwen3.8-flash`](https://help.aliyun.com/zh/model-studio/text-generation-model) |
| Alibaba Bailian Coding Plan (CN) | chat_completions | bearer | [API 文档](https://help.aliyun.com/zh/model-studio/coding-plan) | [`qwen3.7-plus`](https://help.aliyun.com/zh/model-studio/coding-plan) |
| QwenCloud API (Global) | chat_completions | bearer | [API 文档](https://www.alibabacloud.com/help/en/model-studio/base-url) | [`qwen3.8-flash`](https://www.alibabacloud.com/help/en/model-studio/qwen3-8-flash) |
| QwenCloud Coding Plan (Global) | chat_completions | bearer | [API 文档](https://www.alibabacloud.com/help/en/model-studio/coding-plan) | [`qwen3.7-plus`](https://www.alibabacloud.com/help/en/model-studio/coding-plan) |
| QwenCloud Token Plan (Global) | chat_completions | bearer | [API 文档](https://www.alibabacloud.com/help/en/model-studio/base-url) | [`qwen3.6-flash`](https://www.alibabacloud.com/help/en/model-studio/token-plan-team-overview) |
| Volcengine Ark / Doubao API | chat_completions | bearer | [API 文档](https://www.volcengine.com/docs/ark/chat-api) | [`doubao-seed-2-1-pro-260628`](https://www.volcengine.com/docs/ark/chat-api) |
| Volcengine Agent Plan | chat_completions | bearer | [API 文档](https://www.volcengine.com/docs/82379/2160841) | [`doubao-seed-1.6`](https://www.volcengine.com/docs/82379/2160841) |
| Volcengine Coding Plan | chat_completions | bearer | [API 文档](https://www.volcengine.com/docs/82379/1928261) | [`ark-code-latest`](https://www.volcengine.com/docs/82379/1928261) |
| BytePlus Coding Plan | chat_completions | bearer | [API 文档](https://docs.byteplus.com/en/docs/ModelArk/1928261) | [`ark-code-latest`](https://docs.byteplus.com/en/docs/ModelArk/1928261) |
| Baidu Qianfan API | chat_completions | bearer | [API 文档](https://cloud.baidu.com/doc/qianfan/s/Hmh4suq26) | [`ernie-5.0`](https://cloud.baidu.com/doc/qianfan/s/Hmh4suq26) |
| Baidu Qianfan Coding Plan | chat_completions | bearer | [API 文档](https://cloud.baidu.com/doc/qianfan/s/imlg0beiu) | [`qianfan-code-latest`](https://cloud.baidu.com/doc/qianfan/s/imlg0beiu) |
| Baidu Qianfan Token Plan (Team) | chat_completions | bearer | [API 文档](https://cloud.baidu.com/doc/qianfan/s/smqeup7hm) | [`glm-5.2`](https://cloud.baidu.com/doc/qianfan/s/smqeup7hm) |
| StepFun API (CN) | chat_completions | bearer | [API 文档](https://platform.stepfun.com/docs/zh/api-reference/chat/chat-completion-create) | [`step-3.5-flash`](https://platform.stepfun.com/docs/zh/api-reference/chat/chat-completion-create) |
| StepFun Step Plan (CN) | chat_completions | bearer | [API 文档](https://platform.stepfun.com/docs/zh/step-plan/integrations/reasoning-api) | [`step-router-v1`](https://platform.stepfun.com/docs/zh/step-plan/integrations/reasoning-api) |
| StepFun API (Global) | chat_completions | bearer | [API 文档](https://platform.stepfun.ai/docs/en/api-reference/chat/chat-completion-create) | [`step-3.5-flash`](https://platform.stepfun.ai/docs/en/api-reference/chat/chat-completion-create) |
| StepFun Step Plan (Global) | chat_completions | bearer | [API 文档](https://platform.stepfun.ai/docs/en/step-plan/integrations/reasoning-api) | [`step-3.5-flash`](https://platform.stepfun.ai/docs/en/step-plan/integrations/reasoning-api) |
| Xiaomi MiMo API | chat_completions | bearer | [API 文档](https://mimo.mi.com/docs/zh-CN/quick-start/faq/api-integration) | [`mimo-v2.5`](https://mimo.mi.com/docs/zh-CN/quick-start/faq/api-integration) |
| Xiaomi MiMo Token Plan (CN) | chat_completions | bearer | [API 文档](https://mimo.mi.com/docs/zh-CN/tokenplan/integration/tools-overview) | [`mimo-v2.5`](https://mimo.mi.com/docs/zh-CN/tokenplan/integration/tools-overview) |
| BaiLing / Ant Ling API | chat_completions | bearer | [API 文档](https://developer.ant-ling.com/zh-CN/docs/api-reference/openai/) | [`Ling-3.0-flash`](https://developer.ant-ling.com/zh-CN/docs/api-reference/openai/) |
| KAT-Coder / StreamLake API | chat_completions | bearer | [API 文档](https://www.streamlake.ai/document/DOC/mg6k6nlp8j6qxicx4c9) | [`kat-coder-pro-v2.5`](https://www.streamlake.ai/document/DOC/mg6k6nlp8j6qxicx4c9) |
| KAT-Coder / StreamLake Coding Plan | chat_completions | bearer | [API 文档](https://www.streamlake.ai/document/DOC/mg6k6nlp8j6qxicx4c9) | [`kat-coder-pro-v2.5`](https://www.streamlake.ai/document/DOC/mg6k6nlp8j6qxicx4c9) |
| OpenRouter | chat_completions | bearer | [API 文档](https://openrouter.ai/docs/quickstart) | [`~openai/gpt-latest`](https://openrouter.ai/docs/quickstart) |
| SiliconFlow (CN) | chat_completions | bearer | [API 文档](https://docs.siliconflow.cn/docs/userguide/quickstart) | [`deepseek-ai/DeepSeek-V4-Flash`](https://docs.siliconflow.cn/docs/api/models-get) |
| SiliconFlow (Global) | chat_completions | bearer | [API 文档](https://docs.siliconflow.com/en/userguide/quickstart) | [`deepseek-ai/DeepSeek-V4-Flash`](https://docs.siliconflow.com/en/api-reference/chat-completions/chat-completions) |
| NVIDIA API Catalog | chat_completions | bearer | [API 文档](https://docs.api.nvidia.com/nim/reference/llm-apis) | [`deepseek-ai/deepseek-v4-flash`](https://docs.api.nvidia.com/nim/reference/llm-apis) |
| ModelScope API Inference | chat_completions | bearer | [API 文档](https://modelscope.cn/docs/model-service/API-Inference/intro) | [`Qwen/Qwen3.5-35B-A3B`](https://modelscope.cn/docs/model-service/API-Inference/intro) |
| PPIO | chat_completions | bearer | [API 文档](https://ppio.com/docs/models/reference-llm-create-chat-completion) | [`deepseek/deepseek-r1`](https://ppio.com/docs/model/llm) |
| Qiniu AI | chat_completions | bearer | [API 文档](https://developer.qiniu.com/aitokenapi/13379/real-time-ai-interface-api) | [`deepseek-v3`](https://developer.qiniu.com/aitokenapi/13379/real-time-ai-interface-api) |
| Novita AI | chat_completions | bearer | [API 文档](https://docs.novita.ai/api-reference/model-apis-llm-create-chat-completion) | [`meta-llama/llama-3.1-8b-instruct`](https://docs.novita.ai/guides/llm-recommended) |
| Compshare ModelVerse | chat_completions | bearer | [API 文档](https://compshare.cn/docs/modelverse/models/text_api/openai_compatible) | [`deepseek-ai/DeepSeek-R1`](https://www.compshare.cn/docs/modelverse/models/quick-start) |
| Compshare Coding Plan | chat_completions | bearer | [API 文档](https://compshare.cn/docs/modelverse/codingfaq) | [`deepseek-v4-pro`](https://compshare.cn/docs/modelverse/codingfaq) |
| AtlasCloud Coding Plan | chat_completions | bearer | [API 文档](https://www.atlascloud.ai/docs/coding-plan/api) | [`zai-org/glm-5.1`](https://www.atlascloud.ai/docs/coding-plan/api) |

KAT-Coder 的完整 Chat 地址与 Bearer 鉴权，根据官方 OpenAI 兼容客户端配置推导：Base URL 加标准 `/chat/completions` 后缀。百灵采用当前官方 `api.ant-ling.com` 域名。

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
