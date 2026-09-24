[简体中文](provider-presets.zh-CN.md)

# Plan and API presets

Browse Plan/API presets from **Accounts → Add account** or **Providers → Add Provider**. Both buttons open the same Accounts chooser. Existing connections list only built-in families that still have an account plus saved user-defined Providers; unused built-in templates stay with these creation templates. Presets group by vendor, with a compact selector for regional or plan variants. Search includes vendor and variant names, preset IDs and endpoint hosts. The chooser shows a read-only connection summary before the Key field. Fixed presets supply the address, protocol, authentication and an editable default model; Azure and Bedrock still require resource/regional addresses and deployment/model information. Completing a preset requires a Key and creates the Provider and its first account together; **Save draft** may omit the Key. Custom API and manual configuration retain their full settings.

Presets create ordinary user-defined Providers through the atomic Dashboard V4 onboarding commit. The saved Provider participates in account ordering, fallback, model routing and request logs. Its configuration stays editable. It does not add a separate adapter or automatically create an account before saving.

Models imported while a preset is selected receive a public name such as `openrouter/vendor/model`, while the exact upstream ID remains `vendor/model`. Both fields can be edited. Manual configuration imports add no preset prefix.

Switching presets clears the previous channel's Key and model mappings, then fills the selected channel's default model. Operator links contain no referral parameters. Discovery only changes the draft; model tests remain explicit and may be billable. Use models supporting the fixed upstream protocol. Image, audio, video and embedding-only models are outside this chat gateway's preset workflow.

Default models come from operator model documentation or request examples. They are editable starting points and do not certify account entitlement. Saving makes no upstream calls, imports no full model catalog and performs no paid test. Template updates do not rewrite saved connections or mappings.

## Discover, test and edit

Presets save the documented route set when you create or explicitly reapply one. Fixed presets never derive replacement paths from a runtime hostname, and updating OCG never rewrites an already saved connection. Azure and Bedrock require your resource or regional Responses URL. If that URL matches the documented Azure OpenAI or Bedrock Runtime host and path, OCG fills their other documented routes for the same resource. Review the routes before saving; a custom hostname or path stays as one manually editable route. Azure static API Keys use the `api-key` header; Microsoft Entra tokens use Bearer. Bedrock has no compatible `GET /models`, so enter its model ID yourself. Gemini's native Google API is outside these three upstream formats, so its preset uses the documented OpenAI-compatible surface.

To update a connection created from an older preset, open **Providers → Edit connection**, choose **Adopt preset protocols**, review the affected Keys and addresses, then save. Existing routes and grants are not changed just by updating OCG.

MiniMax CN/API and Global presets save distinct Chat Completions, Responses, and Messages routes. Chat and Responses use Bearer authentication; Messages uses `x-api-key`. Their full endpoints are stored so that the dashboard does not guess alternative paths. MiMo API and MiMo Token Plan (CN) similarly save Bearer-authenticated Chat `/v1/chat/completions`, Responses `/v1/responses`, and Messages `/anthropic/v1/messages` routes. See [protocol defaults](providers.md#protocol-defaults-and-connection-tests).

Fetch models only updates the draft candidate list. Empty and truncated results are shown explicitly; import only the models you select, then save. Discovery does not prove that every model accepts the selected protocol.

Choose a model and use **Test model**. The test uses its explicit protocol/endpoint override, or the supplier defaults when it inherits. A success requires a response matching the selected protocol, not just HTTP 200. It does not prove streaming, tools or other advanced features for that supplier.

Testing never changes routing or preference. **Save Provider** commits the configuration you edited. Configurable HTTP tests use the selected connection route; they do not read back or replace a saved Key. Linked platform Keys keep their managed endpoint constraints. Model protocol/endpoint overrides are connection-owned and available to both ordinary dynamic and migrated Custom HTTP connections.

The selected preset is retained when saving and reopening, including resource-specific addresses such as Azure. It preserves template hints, discovery restrictions and import naming; it does not certify an edited endpoint as official. Entries without preset provenance use only unambiguous existing configuration evidence, and manually named models are not rewritten. Changing the endpoint, Key, model or protocol clears stale test results.

## Coverage and sources

Compared on **2026-09-08** against CC-Switch's Claude, Codex, Gemini, OpenCode, OpenClaw and Hermes [preset sources at `f3b18df`](https://github.com/farion1231/cc-switch/tree/f3b18df12007d0fd79fd8ad8d310880664015197/src/config). CC-Switch categories are discovery hints, not a trust decision: for example, Azure and xAI appear under third-party categories. The endpoint and authentication choices below were checked against operator documentation.

Each row is a ready configuration template, not a claim of authenticated live testing or account entitlement. Except for the matching DeepSeek/Zhipu official API presets described below, new Providers have no automatic official quota, balance or pricing synchronization; per-account credit configuration enables local estimates. Coding/Token Plan Keys and regional API Keys must match the selected endpoint; the upstream's supported-use restrictions still apply.

Protocol routes were checked against each operator's documentation on **2026-09-24**. They describe the offering's available formats, not a guarantee that every model or Key accepts each format. Tencent TokenHub, Alibaba Responses, AtlasCloud, PPIO and Novita have model-specific availability. Where an operator publishes only a base URL, the listed full Messages URL adds the standard `/v1/messages` suffix. StreamLake's Messages proxy is pinned to the default `kat-coder-pro-v2.5` model; review the URL when changing models. Anthropic's Chat compatibility is intended for evaluation; use native Messages for full Claude features. The table shows the routes seeded by OCG after you supply any customer-specific address.

The rows below are grouped by vendor family — one row per preset variant, with multi-variant families (Tencent, Zhipu, Alibaba Cloud, Volcengine/BytePlus, Baidu Qianfan, StepFun, Xiaomi, MiniMax, StreamLake, SiliconFlow, Compshare) listed together under their shared brand mark. Vendor families that map to a CC0 brand asset under `src/assets/provider-logos/` (Anthropic, Google, DeepSeek, Ollama, NVIDIA, OpenRouter, Alibaba Cloud, ByteDance, Baidu) render that logo in the chooser; the rest use a tinted monogram block carrying the vendor's initial. The built-in plans Kimi Code CN, MiniMax CN, and Ollama Cloud also show their vendor brand marks even though they live outside the chooser.

| Preset | Upstream | Authentication | Operator documentation | Default model |
| --- | --- | --- | --- | --- |
| OpenAI API | Responses + Chat Completions | bearer | [API docs](https://developers.openai.com/api/reference/overview) | [`gpt-5.6-luna`](https://developers.openai.com/api/docs/models/gpt-5.6-luna) |
| Anthropic API | Messages + Chat Completions | x-api-key (Messages); bearer (Chat) | [API docs](https://platform.claude.com/docs/en/cli-sdks-libraries/libraries/openai-sdk) | [`claude-haiku-4-5-20251001`](https://platform.claude.com/docs/en/models/overview) |
| Google Gemini API | Chat Completions | bearer | [API docs](https://ai.google.dev/gemini-api/docs/openai) | [`gemini-3.8-flash`](https://ai.google.dev/gemini-api/docs/openai) |
| xAI (Grok) API | Responses + Chat Completions | bearer | [API docs](https://docs.x.ai/developers/quickstart) | [`grok-4.6`](https://docs.x.ai/developers/models) |
| Azure OpenAI v1 | Responses + Chat Completions (resource URL required) | api-key for static Key | [API docs](https://learn.microsoft.com/en-us/azure/foundry/openai/api-version-lifecycle) | Customer model/deployment |
| AWS Bedrock (API Key) | Responses + Chat Completions + Messages (regional URL required) | bearer (Responses/Chat); x-api-key (Messages) | [API docs](https://docs.aws.amazon.com/bedrock/latest/userguide/models-api-compatibility.html) | Customer model/deployment |
| DeepSeek API | Chat Completions + Responses + Messages | bearer (Chat/Responses); x-api-key (Messages) | [API docs](https://api-docs.deepseek.com/) | [`deepseek-flash`](https://api-docs.deepseek.com/) |
| Kimi / Moonshot API | Chat Completions + Responses + Messages | bearer | [API docs](https://platform.kimi.com/docs/api/overview) | [`kimi-k2.6`](https://platform.kimi.com/docs/api/models) |
| Zhipu GLM API | Chat Completions + Responses + Messages | bearer (Chat/Responses); x-api-key (Messages) | [API docs](https://docs.bigmodel.cn/cn/guide/develop/http/introduction) | [`glm-5.3`](https://docs.bigmodel.cn/cn/guide/develop/http/introduction) |
| Zhipu GLM Coding Plan | Chat Completions + Responses + Messages | bearer (Chat/Responses); x-api-key (Messages) | [API docs](https://docs.bigmodel.cn/cn/guide/develop/cursor) | [`glm-5.3`](https://docs.bigmodel.cn/cn/guide/develop/cursor) |
| Z.AI GLM API | Chat Completions | bearer | [API docs](https://docs.z.ai/api-reference/introduction) | [`glm-5.3`](https://docs.z.ai/api-reference/introduction) |
| Z.AI GLM Coding Plan | Chat Completions + Responses + Messages | bearer (Chat/Responses); x-api-key (Messages) | [API docs](https://docs.z.ai/devpack/tool/others) | [`glm-5.3`](https://docs.z.ai/devpack/tool/others) |
| MiniMax API (CN) | Messages + Chat Completions + Responses | x-api-key (Messages); bearer (Chat/Responses) | [API docs](https://platform.minimax.cn/docs/api-reference/responses-create) | [`MiniMax-M3`](https://platform.minimax.cn/docs/api-reference/responses-create) |
| MiniMax API / Token Plan (Global) | Messages + Chat Completions + Responses | x-api-key (Messages); bearer (Chat/Responses) | [API docs](https://platform.minimax.io/docs/api-reference/responses-create) | [`MiniMax-M3`](https://platform.minimax.io/docs/api-reference/responses-create) |
| LongCat API | Chat Completions + Messages | bearer | [API docs](https://longcat.chat/platform/docs/api/chat.html) | [`LongCat-2.0`](https://longcat.chat/platform/docs/api/chat) |
| Tencent Hunyuan / TokenHub API | Chat Completions + Responses + Messages | bearer (Chat/Responses); x-api-key (Messages) | [API docs](https://cloud.tencent.com/document/product/1823/130078) | [`hy3`](https://cloud.tencent.com/document/product/1823/132252) |
| Tencent Token Plan (CN) | Chat Completions + Messages | bearer (Chat); x-api-key (Messages) | [API docs](https://cloud.tencent.com/document/product/1823/130119) | [`tc-code-latest`](https://cloud.tencent.com/document/product/1823/130119) |
| Tencent Token Plan (Global) | Chat Completions + Messages | bearer (Chat); x-api-key (Messages) | [API docs](https://www.tencentcloud.com/document/product/1300/81037) | [`auto`](https://www.tencentcloud.com/document/product/1300/81037) |
| Tencent Token Plan Enterprise Pro (CN) | Chat Completions + Responses + Messages | bearer (Chat/Responses); x-api-key (Messages) | [API docs](https://cloud.tencent.com/document/product/1823/130659) | [`auto`](https://cloud.tencent.com/document/product/1823/130659) |
| Tencent Token Plan Enterprise Pro (Global) | Chat Completions + Responses + Messages | bearer (Chat/Responses); x-api-key (Messages) | [API docs](https://www.tencentcloud.com/document/product/1300/81489) | [`auto`](https://www.tencentcloud.com/document/product/1300/81489) |
| Tencent Token Plan Enterprise Lite (CN) | Chat Completions + Responses + Messages | bearer (Chat/Responses); x-api-key (Messages) | [API docs](https://cloud.tencent.com/document/product/1823/131173) | [`auto`](https://cloud.tencent.com/document/product/1823/131173) |
| Tencent Token Plan Enterprise Lite (Global) | Chat Completions + Responses + Messages | bearer (Chat/Responses); x-api-key (Messages) | [API docs](https://www.tencentcloud.com/document/product/1300/81490) | [`auto`](https://www.tencentcloud.com/document/product/1300/81490) |
| Alibaba Bailian API (CN) | Chat Completions + Messages + Responses | bearer (Chat/Responses); x-api-key (Messages) | [API docs](https://help.aliyun.com/zh/model-studio/base-url) | [`qwen3.8-flash`](https://help.aliyun.com/zh/model-studio/text-generation-model) |
| Alibaba Bailian Coding Plan (CN) | Chat Completions + Messages | bearer (Chat); x-api-key (Messages) | [API docs](https://help.aliyun.com/zh/model-studio/coding-plan) | [`qwen3.7-plus`](https://help.aliyun.com/zh/model-studio/coding-plan) |
| QwenCloud API (Global) | Chat Completions + Messages + Responses | bearer (Chat/Responses); x-api-key (Messages) | [API docs](https://www.alibabacloud.com/help/en/model-studio/base-url) | [`qwen3.8-flash`](https://www.alibabacloud.com/help/en/model-studio/qwen3-8-flash) |
| QwenCloud Coding Plan (Global) | Chat Completions + Messages | bearer (Chat); x-api-key (Messages) | [API docs](https://www.alibabacloud.com/help/en/model-studio/coding-plan) | [`qwen3.7-plus`](https://www.alibabacloud.com/help/en/model-studio/coding-plan) |
| QwenCloud Token Plan (Global) | Chat Completions + Messages + Responses | bearer (Chat/Responses); x-api-key (Messages) | [API docs](https://www.alibabacloud.com/help/en/model-studio/base-url) | [`qwen3.6-flash`](https://www.alibabacloud.com/help/en/model-studio/token-plan-team-overview) |
| Volcengine Ark / Doubao API | Chat Completions + Responses + Messages | bearer | [API docs](https://www.volcengine.com/docs/ark/chat-api) | [`doubao-seed-2-1-pro-260628`](https://www.volcengine.com/docs/ark/chat-api) |
| Volcengine Agent Plan | Chat Completions + Responses + Messages | bearer | [API docs](https://www.volcengine.com/docs/82379/2160841) | [`doubao-seed-1.6`](https://www.volcengine.com/docs/82379/2160841) |
| Volcengine Coding Plan | Chat Completions + Responses + Messages | bearer | [API docs](https://www.volcengine.com/docs/82379/1928261) | [`ark-code-latest`](https://www.volcengine.com/docs/82379/1928261) |
| BytePlus Coding Plan | Chat Completions + Responses + Messages | bearer | [API docs](https://docs.byteplus.com/en/docs/ModelArk/1928261) | [`ark-code-latest`](https://docs.byteplus.com/en/docs/ModelArk/1928261) |
| Baidu Qianfan API | Chat Completions + Responses | bearer | [API docs](https://cloud.baidu.com/doc/qianfan/s/Hmh4suq26) | [`ernie-5.0`](https://cloud.baidu.com/doc/qianfan/s/Hmh4suq26) |
| Baidu Qianfan Coding Plan | Chat Completions + Messages | bearer | [API docs](https://cloud.baidu.com/doc/qianfan/s/imlg0beiu) | [`qianfan-code-latest`](https://cloud.baidu.com/doc/qianfan/s/imlg0beiu) |
| Baidu Qianfan Token Plan (Team) | Chat Completions + Messages | bearer | [API docs](https://cloud.baidu.com/doc/qianfan/s/smqeup7hm) | [`glm-5.2`](https://cloud.baidu.com/doc/qianfan/s/smqeup7hm) |
| StepFun API (CN) | Chat Completions + Responses + Messages | bearer | [Messages API](https://platform.stepfun.com/docs/zh/api-reference/chat/messages-create) | [`step-3.5-flash`](https://platform.stepfun.com/docs/zh/api-reference/chat/chat-completion-create) |
| StepFun Step Plan (CN) | Chat Completions + Messages | bearer | [API docs](https://platform.stepfun.com/docs/zh/step-plan/integrations/reasoning-api) | [`step-router-v1`](https://platform.stepfun.com/docs/zh/step-plan/integrations/reasoning-api) |
| StepFun API (Global) | Chat Completions + Responses + Messages | bearer | [Messages API](https://platform.stepfun.ai/docs/en/api-reference/chat/messages-create) | [`step-3.5-flash`](https://platform.stepfun.ai/docs/en/api-reference/chat/chat-completion-create) |
| StepFun Step Plan (Global) | Chat Completions + Messages | bearer | [API docs](https://platform.stepfun.ai/docs/en/step-plan/integrations/reasoning-api) | [`step-3.5-flash`](https://platform.stepfun.ai/docs/en/step-plan/integrations/reasoning-api) |
| Xiaomi MiMo API | Chat Completions + Messages + Responses | bearer | [API docs](https://mimo.mi.com/docs/en-US/tokenplan/integration/codex-configuration) | [`mimo-v2.6-flash`](https://mimo.mi.com/docs/en-US/tokenplan/integration/codex-configuration) |
| Xiaomi MiMo Token Plan (CN) | Chat Completions + Messages + Responses | bearer | [API docs](https://mimo.mi.com/docs/en-US/tokenplan/integration/claudecode) | [`mimo-v2.6-flash`](https://mimo.mi.com/docs/en-US/tokenplan/integration/codex-configuration) |
| BaiLing / Ant Ling API | Chat Completions + Messages | bearer | [API docs](https://developer.ant-ling.com/zh-CN/docs/api-reference/openai/) | [`Ling-3.0-flash`](https://developer.ant-ling.com/zh-CN/docs/api-reference/openai/) |
| KAT-Coder / StreamLake API | Chat Completions + Messages | bearer | [API docs](https://www.streamlake.ai/document/DOC/mg6k6nlp8j6qxicx4c9) | [`kat-coder-pro-v2.5`](https://www.streamlake.ai/document/DOC/mg6k6nlp8j6qxicx4c9) |
| KAT-Coder / StreamLake Coding Plan | Chat Completions + Messages | bearer | [API docs](https://www.streamlake.ai/document/DOC/mg6k6nlp8j6qxicx4c9) | [`kat-coder-pro-v2.5`](https://www.streamlake.ai/document/DOC/mg6k6nlp8j6qxicx4c9) |
| OpenRouter | Chat Completions + Responses + Messages | bearer | [API docs](https://openrouter.ai/docs/quickstart) | [`~openai/gpt-latest`](https://openrouter.ai/docs/quickstart) |
| OpenRouter Free | Chat Completions + Responses + Messages | bearer | [Model routes](https://openrouter.ai/openrouter/free) | [`openrouter/free`](https://openrouter.ai/openrouter/free) |
| SiliconFlow (CN) | Chat Completions + Messages | bearer | [Messages API](https://api-docs.siliconflow.cn/docs/api/messages-post) | [`deepseek-ai/DeepSeek-V4-Flash`](https://docs.siliconflow.cn/docs/api/models-get) |
| SiliconFlow (Global) | Chat Completions + Messages | bearer | [API docs](https://docs.siliconflow.cn/docs/usercases/use-siliconcloud-in-ccswitch) | [`deepseek-ai/DeepSeek-V4-Flash`](https://docs.siliconflow.com/en/api-reference/chat-completions/chat-completions) |
| NVIDIA API Catalog | Chat Completions | bearer | [API docs](https://docs.api.nvidia.com/nim/reference/llm-apis) | [`deepseek-ai/deepseek-v4-flash`](https://docs.api.nvidia.com/nim/reference/llm-apis) |
| ModelScope API Inference | Chat Completions | bearer | [API docs](https://modelscope.cn/docs/model-service/API-Inference/intro) | [`Qwen/Qwen3.5-35B-A3B`](https://modelscope.cn/docs/model-service/API-Inference/intro) |
| PPIO | Chat Completions + Messages | bearer | [Messages guide](https://ppio.com/docs/model/llm-anthropic-compatibility) | [`deepseek/deepseek-r1`](https://ppio.com/docs/model/llm) |
| Qiniu AI | Chat Completions + Messages | bearer | [API docs](https://developer.qiniu.com/aitokenapi/13379/real-time-ai-interface-api) | [`deepseek-v3`](https://developer.qiniu.com/aitokenapi/13379/real-time-ai-interface-api) |
| Novita AI | Chat Completions + Messages | bearer | [Messages guide](https://blogs.novita.ai/access-glm4-6-in-claude-code/) | [`meta-llama/llama-3.1-8b-instruct`](https://docs.novita.ai/guides/llm-recommended) |
| Compshare ModelVerse | Chat Completions + Responses + Messages | bearer | [API docs](https://compshare.cn/docs/modelverse/models/text_api/openai_compatible) | [`deepseek-ai/DeepSeek-R1`](https://www.compshare.cn/docs/modelverse/models/quick-start) |
| Compshare Coding Plan | Chat Completions + Messages | bearer | [API docs](https://compshare.cn/docs/modelverse/codingfaq) | [`deepseek-v4-pro`](https://compshare.cn/docs/modelverse/codingfaq) |
| AtlasCloud Coding Plan | Chat Completions + Responses + Messages | bearer (Chat/Responses); x-api-key (Messages) | [API docs](https://www.atlascloud.ai/docs/coding-plan/api) | [`zai-org/glm-5.1`](https://www.atlascloud.ai/docs/coding-plan/api) |

KAT-Coder's full Chat URL and Bearer auth are derived from its official OpenAI-compatible client configuration (base URL plus the standard `/chat/completions` suffix). Its Messages URL adds `/v1/messages` to the documented Claude proxy base. Coding Plan use is subject to [StreamLake's subscription terms](https://www.streamlake.ai/document/DOC/mjzrrkirccgntfkz46). Ant Ling uses the current official `api.ant-ling.com` domain.

**Unverified gap:** CC-Switch's Baidu personal Token Plan `/v2/tokenplan/personal` route could not be corroborated in the fetched official docs. It is not offered as a preset. The documented Qianfan general API, legacy Coding Plan and team Token Plan are included separately; do not use a personal Key on the team endpoint.

## Existing integrations and exclusions

- **OpenCode Go**, **Kimi Code CN** and **MiniMax CN Token Plan** retain their existing built-in routing and usage behavior. Select those existing Providers for the subscription workflow; the Moonshot and MiniMax API presets cover the distinct API/region use cases.
- Azure uses the current v1 API with a resource URL and deployment name. It does not provision resources or refresh Entra ID tokens. Bedrock uses its official OpenAI-compatible API with an API Key; AWS AK/SK signing is not implemented.
- New API / One API / Sub2API distributors, subscription reverse proxies, referral-only relay listings and providers whose operator/API provenance could not be established are not included. A CC-Switch “aggregator” label alone is insufficient.
- The included aggregation/inference platforms have their own documented service: OpenRouter, SiliconFlow, NVIDIA, ModelScope, PPIO, Qiniu, Novita, Compshare and AtlasCloud. This is a selected set, not a blanket import of CC-Switch's relay catalog.
- Browser subscription login, GitHub Copilot OAuth, Codex OAuth and Grok OAuth are not API Key presets. Existing CPA integration is separate.

Initial model IDs are reviewed against operator documentation rather than copied from CC-Switch. You can replace or extend them using the current catalog or console, retaining exact upstream spelling. A missing model-list interface does not prevent saving an explicit mapping.

---

[Add a Provider](add-provider.md) · [Providers](providers.md) · [简体中文](provider-presets.zh-CN.md)

## Official API balances and reference prices

DeepSeek API and Zhipu GLM API presets now expose a separate financial reference panel when their saved preset, API offering, authentication, and official destination still match. Inference remains on Configurable HTTP. This is not available for arbitrary Custom API endpoints, edited proxy destinations, or Coding Plan presets.

On **Accounts**, these presets share the pay-as-you-go meter: **Balance**, **This month**, and **Lifetime**. Remaining is the official wallet; this month and lifetime are local estimates from priced OCG logs (UTC month for this month). DeepSeek's **Refresh balance** reads the selected Key's official `/user/balance`. The remaining figure is the official total; a gift caption appears only when granted credit is above zero (it is already inside the total). Observation time is a shared caption. Balance refresh never changes enablement, authentication, cooldown, or routing, and errors keep the last successful observation. A replaced Key or edited endpoint cannot inherit an old balance. Zhipu shows that no public balance API is integrated; it does not invent a wallet value or request undocumented console endpoints.

On **Providers → Pricing**, these presets show official reference rates per million tokens and an explicit **Refresh price table** action. A dated, checked-in reference is available initially. Price references expire after 30 days; new requests then remain unpriced until a successful refresh. Unsupported source layouts retain the previous reference without extending its validity. Loading the page and sending inference never fetch a price document or balance.

DeepSeek's reference is in USD and distinguishes cache hits, misses, and weekday peak/off-peak periods. A CNY balance is not converted or subtracted from USD estimates. Zhipu's reference is in CNY and is stored as native-currency cost, never as USD. Account panels aggregate successful locally logged requests by original currency for the current UTC month and for all time, and show the number with unknown cost this month. This is an estimate, not an invoice; it excludes traffic outside OCG and does not reprice historical requests. The price revision and period are captured per attempt, including streaming finalization.

Only supported exact upstream model rates apply. Unknown models, unsupported tier/storage billing, missing usage, invalid token counts, cache writes, paid hosted tools and other variable-cost requests stay unknown. Editing a model's route to another destination cannot inherit official prices. Existing saved presets gain this capability without a migration; generic providers use local estimates only after explicit credit setup.

Sources: [DeepSeek balance](https://api-docs.deepseek.com/api/get-user-balance/), [DeepSeek pricing](https://api-docs.deepseek.com/quick_start/pricing/), and [Zhipu pricing](https://docs.bigmodel.cn/cn/guide/start/pricing.md). The initial reference was checked on 2026-09-17. Neither the source checks nor automated tests used a real account Key or a paid inference request.

## StepFun API (CN) balance and Step Plan (CN) credits

StepFun API (CN) ordinary endpoints on `api.stepfun.com` (not the `/step_plan` path) can refresh a Key-authenticated current balance with the same Accounts cash meter used for DeepSeek and Moonshot. That official wallet is unchanged.

Step Plan (CN) has no official usage API in Open Console Gateway. The account card uses a **local estimate** plus manual calibration. In **Add Key** or the Key's **Edit** form, pick Mini / Plus / Pro / Max (400M / 1600M / 8000M / 40000M credits, 1M credits = 1 CNY), enter the current remaining balance and the next reset (default: next calendar month 00:00 China time, UTC+8). Rates come from the server preset. Later balance corrections use **Calibrate usage** in the Key's action row. No console cookie or login is required.

Cost is actual normalized tokens × those rates × the 1M/CNY factor, not the subscription price. Only OCG traffic is estimated; correct external usage with calibration. Wait until pending charges complete before calibrating. Monthly grants, 400M / 1600M top-ups (optional 30-day expiry), and other expiry dates are separate buckets. Remaining excludes expired top-ups. Each expiry is listed on its own; the card does not invent a shared reset. Unknown pricing stays unknown, not free. The estimate does not block routing.

Configurable HTTP destinations can use the same credit model with an explicit name, currency, factor, rates, and buckets. The source URL is a read-only link; Open Console Gateway does not fetch it.
