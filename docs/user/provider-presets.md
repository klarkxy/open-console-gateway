[简体中文](provider-presets.zh-CN.md)

# Plan and API presets

Browse Plan/API presets from Accounts → Add account → Add new service, or Providers → Add Provider. Existing connections list only built-in families that still have an account plus saved user-defined Providers; unused built-in templates stay with these creation templates. Presets group by vendor, with a compact selector for regional or plan variants. Search includes vendor and variant names, preset IDs and endpoint hosts. Accounts add shows a read-only connection summary before the Key field. Providers add collects an optional Key on keyed auth. Fixed presets supply the address, protocol, authentication and an editable default model; Azure and Bedrock still require resource/regional addresses and deployment/model information. Saving from Accounts requires a Key and creates the Provider and its first account together; saving from Providers writes the definition and, if a Key is filled, its first account. Custom API and manual configuration retain their full settings.

Presets create ordinary user-defined Providers through the existing atomic Dashboard V3 workflow. The saved Provider participates in account ordering, fallback, model routing and request logs. Its configuration stays editable. It does not add a separate adapter or automatically create an account before saving.

Models imported while a preset is selected receive a public name such as `openrouter/vendor/model`, while the exact upstream ID remains `vendor/model`. Both fields can be edited. Manual configuration imports retain the original public-name behavior.

Switching presets clears the previous channel's Key and model mappings, then fills the selected channel's default model. Operator links contain no referral parameters. Discovery only changes the draft; model tests remain explicit and may be billable. Use models supporting the fixed upstream protocol. Image, audio, video and embedding-only models are outside this chat gateway's preset workflow.

Default models come from operator model documentation or request examples. They are editable starting points and do not certify account entitlement. Saving makes no upstream calls, imports no full model catalog and performs no paid test. Template updates do not rewrite saved connections or mappings.

## Discover, test and edit

Defaults and endpoints were reviewed on **2026-09-09**: xAI uses Responses, and MiniMax CN/Global uses Messages with Bearer auth, following the current operator recommendation and REST reference. OpenAI/Azure/Bedrock retain Responses, Anthropic retains Messages, and the other presets retain their documented Chat-compatible defaults. Gemini's native Google API is outside these three upstream formats, so its preset deliberately uses the documented OpenAI compatibility surface. A documented compatibility option is not necessarily the operator's exclusive recommendation. Templates only initialize new drafts; saved choices are preserved.

MiniMax's official Chat and Messages URLs have different prefixes (`/v1` and `/anthropic/v1`). Configure the exact endpoint when overriding a model's protocol; the dashboard does not guess alternate paths. [Current MiniMax Messages authentication](https://platform.minimax.io/docs/api-reference/text-chat-anthropic) is Bearer, even though the wire format is Anthropic-compatible. See [protocol defaults](providers.md#protocol-defaults-and-connection-tests).

Fetch models only updates the draft candidate list. Empty and truncated results are shown explicitly; import only the models you select, then save. Discovery does not prove that every model accepts the selected protocol.

Choose a model and use **Test model**. The test uses its explicit protocol/endpoint override, or the supplier defaults when it inherits. A success requires a response matching the selected protocol, not just HTTP 200. It does not prove streaming, tools or other advanced features for that supplier.

Testing never changes routing or preference. **Save Provider** commits the configuration you edited. Custom API accounts test their configured account-owned protocol. Supply a temporary test Key when editing; this action does not read back or replace a saved Key. Linked platform Keys keep their managed endpoint constraints. Model overrides are available for user-defined Providers, not account-owned Custom endpoints.

The selected preset is retained when saving and reopening, including resource-specific addresses such as Azure. It preserves template hints, discovery restrictions and import naming; it does not certify an edited endpoint as official. Older entries without preset provenance use only unambiguous existing configuration evidence, and manually named models are not rewritten. Changing the endpoint, Key, model or protocol clears stale test results.

## Coverage and sources

Compared on **2026-09-08** against CC-Switch's Claude, Codex, Gemini, OpenCode, OpenClaw and Hermes [preset sources at `f3b18df`](https://github.com/farion1231/cc-switch/tree/f3b18df12007d0fd79fd8ad8d310880664015197/src/config). CC-Switch categories are discovery hints, not a trust decision: for example, Azure and xAI appear under third-party categories. The endpoint and authentication choices below were checked against operator documentation.

Each row is a ready configuration template, not a claim of authenticated live testing or account entitlement. New Providers remain unpriced: official quota, balance and pricing are not synchronized. Coding/Token Plan Keys and regional API Keys must match the selected endpoint; the upstream's supported-use restrictions still apply.

The rows below are grouped by vendor family — one row per preset variant, with multi-variant families (Tencent, Zhipu, Alibaba Cloud, Volcengine/BytePlus, Baidu Qianfan, StepFun, Xiaomi, MiniMax, StreamLake, SiliconFlow, Compshare) listed together under their shared brand mark. Vendor families that map to a CC0 brand asset under `src/assets/provider-logos/` (Anthropic, Google, DeepSeek, Ollama, NVIDIA, OpenRouter, Alibaba Cloud, ByteDance, Baidu) render that logo in the chooser; the rest use a tinted monogram block carrying the vendor's initial. The built-in plans Kimi Code CN, MiniMax CN, and Ollama Cloud also show their vendor brand marks even though they live outside the chooser.

| Preset | Upstream | Authentication | Operator documentation | Default model |
| --- | --- | --- | --- | --- |
| OpenAI API | responses | bearer | [API docs](https://developers.openai.com/api/reference/overview) | [`gpt-5.6-luna`](https://developers.openai.com/api/docs/models/gpt-5.6-luna) |
| Anthropic API | messages | x-api-key | [API docs](https://platform.claude.com/docs/en/api/overview) | [`claude-haiku-4-5-20251001`](https://platform.claude.com/docs/en/models/overview) |
| Google Gemini API | chat_completions | bearer | [API docs](https://ai.google.dev/gemini-api/docs/openai) | [`gemini-3.8-flash`](https://ai.google.dev/gemini-api/docs/openai) |
| xAI (Grok) API | responses | bearer | [API docs](https://docs.x.ai/developers/quickstart) | [`grok-4.6`](https://docs.x.ai/developers/models) |
| Azure OpenAI v1 | responses | bearer | [API docs](https://learn.microsoft.com/en-us/azure/foundry/openai/api-version-lifecycle) | Customer model/deployment |
| AWS Bedrock (API Key) | responses | bearer | [API docs](https://docs.aws.amazon.com/bedrock/latest/userguide/bedrock-mantle.html) | Customer model/deployment |
| DeepSeek API | chat_completions | bearer | [API docs](https://api-docs.deepseek.com/) | [`deepseek-v4-flash`](https://api-docs.deepseek.com/) |
| Kimi / Moonshot API | chat_completions | bearer | [API docs](https://platform.kimi.com/docs/api/overview) | [`kimi-k2.6`](https://platform.kimi.com/docs/api/models) |
| Zhipu GLM API | chat_completions | bearer | [API docs](https://docs.bigmodel.cn/cn/guide/develop/http/introduction) | [`glm-5.3`](https://docs.bigmodel.cn/cn/guide/develop/http/introduction) |
| Zhipu GLM Coding Plan | chat_completions | bearer | [API docs](https://docs.bigmodel.cn/cn/guide/develop/cursor) | [`glm-5.3`](https://docs.bigmodel.cn/cn/guide/develop/cursor) |
| Z.AI GLM API | chat_completions | bearer | [API docs](https://docs.z.ai/api-reference/introduction) | [`glm-5.3`](https://docs.z.ai/api-reference/introduction) |
| Z.AI GLM Coding Plan | chat_completions | bearer | [API docs](https://docs.z.ai/devpack/tool/others) | [`glm-5.3`](https://docs.z.ai/devpack/tool/others) |
| MiniMax API (CN) | messages | bearer | [API docs](https://platform.minimaxi.com/docs/api-reference/text-anthropic-api) | [`MiniMax-M2.5`](https://platform.minimaxi.com/docs/api-reference/text-anthropic-api) |
| MiniMax API / Token Plan (Global) | messages | bearer | [API docs](https://platform.minimax.io/docs/api-reference/text-anthropic-api) | [`MiniMax-M3`](https://platform.minimax.io/docs/api-reference/text-chat-anthropic) |
| LongCat API | chat_completions | bearer | [API docs](https://longcat.chat/platform/docs/api/chat.html) | [`LongCat-2.0`](https://longcat.chat/platform/docs/api/chat) |
| Tencent Hunyuan / TokenHub API | chat_completions | bearer | [API docs](https://cloud.tencent.com/document/product/1823/130078) | [`hy3`](https://cloud.tencent.com/document/product/1823/132252) |
| Tencent Token Plan (CN) | chat_completions | bearer | [API docs](https://cloud.tencent.com/document/product/1823/130119) | [`tc-code-latest`](https://cloud.tencent.com/document/product/1823/130119) |
| Tencent Token Plan (Global) | chat_completions | bearer | [API docs](https://www.tencentcloud.com/document/product/1300/81037) | [`auto`](https://www.tencentcloud.com/document/product/1300/81037) |
| Tencent Token Plan Enterprise Pro (CN) | chat_completions | bearer | [API docs](https://cloud.tencent.com/document/product/1823/130659) | [`auto`](https://cloud.tencent.com/document/product/1823/130659) |
| Tencent Token Plan Enterprise Pro (Global) | chat_completions | bearer | [API docs](https://www.tencentcloud.com/document/product/1300/81489) | [`auto`](https://www.tencentcloud.com/document/product/1300/81489) |
| Tencent Token Plan Enterprise Lite (CN) | chat_completions | bearer | [API docs](https://cloud.tencent.com/document/product/1823/131173) | [`auto`](https://cloud.tencent.com/document/product/1823/131173) |
| Tencent Token Plan Enterprise Lite (Global) | chat_completions | bearer | [API docs](https://www.tencentcloud.com/document/product/1300/81490) | [`auto`](https://www.tencentcloud.com/document/product/1300/81490) |
| Alibaba Bailian API (CN) | chat_completions | bearer | [API docs](https://help.aliyun.com/zh/model-studio/base-url) | [`qwen3.8-flash`](https://help.aliyun.com/zh/model-studio/text-generation-model) |
| Alibaba Bailian Coding Plan (CN) | chat_completions | bearer | [API docs](https://help.aliyun.com/zh/model-studio/coding-plan) | [`qwen3.7-plus`](https://help.aliyun.com/zh/model-studio/coding-plan) |
| QwenCloud API (Global) | chat_completions | bearer | [API docs](https://www.alibabacloud.com/help/en/model-studio/base-url) | [`qwen3.8-flash`](https://www.alibabacloud.com/help/en/model-studio/qwen3-8-flash) |
| QwenCloud Coding Plan (Global) | chat_completions | bearer | [API docs](https://www.alibabacloud.com/help/en/model-studio/coding-plan) | [`qwen3.7-plus`](https://www.alibabacloud.com/help/en/model-studio/coding-plan) |
| QwenCloud Token Plan (Global) | chat_completions | bearer | [API docs](https://www.alibabacloud.com/help/en/model-studio/base-url) | [`qwen3.6-flash`](https://www.alibabacloud.com/help/en/model-studio/token-plan-team-overview) |
| Volcengine Ark / Doubao API | chat_completions | bearer | [API docs](https://www.volcengine.com/docs/ark/chat-api) | [`doubao-seed-2-1-pro-260628`](https://www.volcengine.com/docs/ark/chat-api) |
| Volcengine Agent Plan | chat_completions | bearer | [API docs](https://www.volcengine.com/docs/82379/2160841) | [`doubao-seed-1.6`](https://www.volcengine.com/docs/82379/2160841) |
| Volcengine Coding Plan | chat_completions | bearer | [API docs](https://www.volcengine.com/docs/82379/1928261) | [`ark-code-latest`](https://www.volcengine.com/docs/82379/1928261) |
| BytePlus Coding Plan | chat_completions | bearer | [API docs](https://docs.byteplus.com/en/docs/ModelArk/1928261) | [`ark-code-latest`](https://docs.byteplus.com/en/docs/ModelArk/1928261) |
| Baidu Qianfan API | chat_completions | bearer | [API docs](https://cloud.baidu.com/doc/qianfan/s/Hmh4suq26) | [`ernie-5.0`](https://cloud.baidu.com/doc/qianfan/s/Hmh4suq26) |
| Baidu Qianfan Coding Plan | chat_completions | bearer | [API docs](https://cloud.baidu.com/doc/qianfan/s/imlg0beiu) | [`qianfan-code-latest`](https://cloud.baidu.com/doc/qianfan/s/imlg0beiu) |
| Baidu Qianfan Token Plan (Team) | chat_completions | bearer | [API docs](https://cloud.baidu.com/doc/qianfan/s/smqeup7hm) | [`glm-5.2`](https://cloud.baidu.com/doc/qianfan/s/smqeup7hm) |
| StepFun API (CN) | chat_completions | bearer | [API docs](https://platform.stepfun.com/docs/zh/api-reference/chat/chat-completion-create) | [`step-3.5-flash`](https://platform.stepfun.com/docs/zh/api-reference/chat/chat-completion-create) |
| StepFun Step Plan (CN) | chat_completions | bearer | [API docs](https://platform.stepfun.com/docs/zh/step-plan/integrations/reasoning-api) | [`step-router-v1`](https://platform.stepfun.com/docs/zh/step-plan/integrations/reasoning-api) |
| StepFun API (Global) | chat_completions | bearer | [API docs](https://platform.stepfun.ai/docs/en/api-reference/chat/chat-completion-create) | [`step-3.5-flash`](https://platform.stepfun.ai/docs/en/api-reference/chat/chat-completion-create) |
| StepFun Step Plan (Global) | chat_completions | bearer | [API docs](https://platform.stepfun.ai/docs/en/step-plan/integrations/reasoning-api) | [`step-3.5-flash`](https://platform.stepfun.ai/docs/en/step-plan/integrations/reasoning-api) |
| Xiaomi MiMo API | chat_completions | bearer | [API docs](https://mimo.mi.com/docs/zh-CN/quick-start/faq/api-integration) | [`mimo-v2.5`](https://mimo.mi.com/docs/zh-CN/quick-start/faq/api-integration) |
| Xiaomi MiMo Token Plan (CN) | chat_completions | bearer | [API docs](https://mimo.mi.com/docs/zh-CN/tokenplan/integration/tools-overview) | [`mimo-v2.5`](https://mimo.mi.com/docs/zh-CN/tokenplan/integration/tools-overview) |
| BaiLing / Ant Ling API | chat_completions | bearer | [API docs](https://developer.ant-ling.com/zh-CN/docs/api-reference/openai/) | [`Ling-3.0-flash`](https://developer.ant-ling.com/zh-CN/docs/api-reference/openai/) |
| KAT-Coder / StreamLake API | chat_completions | bearer | [API docs](https://www.streamlake.ai/document/DOC/mg6k6nlp8j6qxicx4c9) | [`kat-coder-pro-v2.5`](https://www.streamlake.ai/document/DOC/mg6k6nlp8j6qxicx4c9) |
| KAT-Coder / StreamLake Coding Plan | chat_completions | bearer | [API docs](https://www.streamlake.ai/document/DOC/mg6k6nlp8j6qxicx4c9) | [`kat-coder-pro-v2.5`](https://www.streamlake.ai/document/DOC/mg6k6nlp8j6qxicx4c9) |
| OpenRouter | chat_completions | bearer | [API docs](https://openrouter.ai/docs/quickstart) | [`~openai/gpt-latest`](https://openrouter.ai/docs/quickstart) |
| SiliconFlow (CN) | chat_completions | bearer | [API docs](https://docs.siliconflow.cn/docs/userguide/quickstart) | [`deepseek-ai/DeepSeek-V4-Flash`](https://docs.siliconflow.cn/docs/api/models-get) |
| SiliconFlow (Global) | chat_completions | bearer | [API docs](https://docs.siliconflow.com/en/userguide/quickstart) | [`deepseek-ai/DeepSeek-V4-Flash`](https://docs.siliconflow.com/en/api-reference/chat-completions/chat-completions) |
| NVIDIA API Catalog | chat_completions | bearer | [API docs](https://docs.api.nvidia.com/nim/reference/llm-apis) | [`deepseek-ai/deepseek-v4-flash`](https://docs.api.nvidia.com/nim/reference/llm-apis) |
| ModelScope API Inference | chat_completions | bearer | [API docs](https://modelscope.cn/docs/model-service/API-Inference/intro) | [`Qwen/Qwen3.5-35B-A3B`](https://modelscope.cn/docs/model-service/API-Inference/intro) |
| PPIO | chat_completions | bearer | [API docs](https://ppio.com/docs/models/reference-llm-create-chat-completion) | [`deepseek/deepseek-r1`](https://ppio.com/docs/model/llm) |
| Qiniu AI | chat_completions | bearer | [API docs](https://developer.qiniu.com/aitokenapi/13379/real-time-ai-interface-api) | [`deepseek-v3`](https://developer.qiniu.com/aitokenapi/13379/real-time-ai-interface-api) |
| Novita AI | chat_completions | bearer | [API docs](https://docs.novita.ai/api-reference/model-apis-llm-create-chat-completion) | [`meta-llama/llama-3.1-8b-instruct`](https://docs.novita.ai/guides/llm-recommended) |
| Compshare ModelVerse | chat_completions | bearer | [API docs](https://compshare.cn/docs/modelverse/models/text_api/openai_compatible) | [`deepseek-ai/DeepSeek-R1`](https://www.compshare.cn/docs/modelverse/models/quick-start) |
| Compshare Coding Plan | chat_completions | bearer | [API docs](https://compshare.cn/docs/modelverse/codingfaq) | [`deepseek-v4-pro`](https://compshare.cn/docs/modelverse/codingfaq) |
| AtlasCloud Coding Plan | chat_completions | bearer | [API docs](https://www.atlascloud.ai/docs/coding-plan/api) | [`zai-org/glm-5.1`](https://www.atlascloud.ai/docs/coding-plan/api) |

KAT-Coder's full Chat URL and Bearer auth are derived from its official OpenAI-compatible client configuration (base URL plus the standard `/chat/completions` suffix). Ant Ling uses the current `api.ant-ling.com` official domain rather than CC-Switch's older `api.tbox.cn` entries.

**Unverified gap:** CC-Switch's Baidu personal Token Plan `/v2/tokenplan/personal` route could not be corroborated in the fetched official docs. It is not offered as a preset. The documented Qianfan general API, legacy Coding Plan and team Token Plan are included separately; do not use a personal Key on the team endpoint.

## Existing integrations and exclusions

- **OpenCode Go**, **Kimi Code CN** and **MiniMax CN Token Plan** retain their existing built-in routing and usage behavior. Select those existing Providers for the subscription workflow; the new Moonshot and MiniMax API presets cover the distinct API/region use cases.
- Azure uses the current v1 API with a resource URL and deployment name. It does not provision resources or refresh Entra ID tokens. Bedrock uses its official OpenAI-compatible API with an API Key; AWS AK/SK signing is not implemented.
- New API / One API / Sub2API distributors, subscription reverse proxies, referral-only relay listings and providers whose operator/API provenance could not be established are not included. A CC-Switch “aggregator” label alone is insufficient.
- The included aggregation/inference platforms have their own documented service: OpenRouter, SiliconFlow, NVIDIA, ModelScope, PPIO, Qiniu, Novita, Compshare and AtlasCloud. This is a selected set, not a blanket import of CC-Switch's relay catalog.
- Browser subscription login, GitHub Copilot OAuth, Codex OAuth and Grok OAuth are not API Key presets. Existing CPA integration is separate.

Initial model IDs are reviewed against operator documentation rather than copied from CC-Switch. You can replace or extend them using the current catalog or console, retaining exact upstream spelling. A missing model-list interface does not prevent saving an explicit mapping.

---

[Add a Provider](add-provider.md) · [Providers](providers.md) · [简体中文](provider-presets.zh-CN.md)

## Official API balances and reference prices

DeepSeek API and Zhipu GLM API presets now expose a separate financial reference panel when their saved preset, API offering, authentication, and official destination still match. Inference remains on Configurable HTTP. This is not available for arbitrary Custom API endpoints, edited proxy destinations, or Coding Plan presets.

On **Accounts**, DeepSeek's **Refresh balance** explicitly reads the selected Key's official `/user/balance` endpoint. Total, granted and topped-up balances are shown separately, with their original CNY/USD currency and observation time. The total already contains its components; it is not added to them. Balance refresh never changes enablement, authentication state, cooldown or routing, and errors retain the last successful observation. A replaced Key or edited endpoint cannot inherit an old balance. Zhipu shows that no public balance API is integrated; it does not invent a wallet value or request undocumented console endpoints.

On **Providers → Pricing**, these presets show official reference rates per million tokens and an explicit **Refresh price table** action. A dated, checked-in reference is available initially. Price references expire after 30 days; new requests then remain unpriced until a successful refresh. Unsupported source layouts retain the previous reference without extending its validity. Loading the page and sending inference never fetch a price document or balance.

DeepSeek's reference is in USD and distinguishes cache hits, misses, and weekday peak/off-peak periods. A CNY balance is not converted or subtracted from USD estimates. Zhipu's reference is in CNY and is stored as native-currency cost, never as USD. Account panels aggregate this month's successful locally logged requests by original currency using UTC month boundaries and show the number with unknown cost. This is an estimate, not an invoice; it excludes traffic outside OCG and does not reprice historical requests. The price revision and period are captured per attempt, including streaming finalization.

Only supported exact upstream model rates apply. Unknown models, unsupported tier/storage billing, missing usage, invalid token counts, cache writes, paid hosted tools and other variable-cost requests stay unknown. Editing a model's route to another destination cannot inherit official prices. Existing saved presets gain this capability without a migration; generic providers remain unpriced.

Sources: [DeepSeek balance](https://api-docs.deepseek.com/api/get-user-balance/), [DeepSeek pricing](https://api-docs.deepseek.com/quick_start/pricing/), and [Zhipu pricing](https://docs.bigmodel.cn/cn/guide/start/pricing.md). The initial reference was checked on 2026-09-17. Neither the source checks nor automated tests used a real account Key or a paid inference request.
