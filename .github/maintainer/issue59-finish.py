from pathlib import Path

def append(path, text):
    p = Path(path)
    assert p.exists(), path
    p.write_text(p.read_text() + '\n' + text)

append('docs/user/provider-presets.md', '''## Official API balances and reference prices

DeepSeek API and Zhipu GLM API presets now expose a separate financial reference panel when their saved preset, API offering, authentication, and official destination still match. Inference remains on Configurable HTTP. This is not available for arbitrary Custom API endpoints, edited proxy destinations, or Coding Plan presets.

On **Accounts**, DeepSeek's **Refresh balance** explicitly reads the selected Key's official `/user/balance` endpoint. Total, granted and topped-up balances are shown separately, with their original CNY/USD currency and observation time. The total already contains its components; it is not added to them. Balance refresh never changes enablement, authentication state, cooldown or routing, and errors retain the last successful observation. A replaced Key or edited endpoint cannot inherit an old balance. Zhipu shows that no public balance API is integrated; it does not invent a wallet value or request undocumented console endpoints.

On **Providers → Pricing**, these presets show official reference rates per million tokens and an explicit **Refresh price table** action. A dated, checked-in reference is available initially. Price references expire after 30 days; new requests then remain unpriced until a successful refresh. Unsupported source layouts retain the previous reference without extending its validity. Loading the page and sending inference never fetch a price document or balance.

DeepSeek's reference is in USD and distinguishes cache hits, misses, and weekday peak/off-peak periods. A CNY balance is not converted or subtracted from USD estimates. Zhipu's reference is in CNY and is stored as native-currency cost, never as USD. Account panels aggregate this month's successful locally logged requests by original currency using UTC month boundaries and show the number with unknown cost. This is an estimate, not an invoice; it excludes traffic outside OCG and does not reprice historical requests. The price revision and period are captured per attempt, including streaming finalization.

Only supported exact upstream model rates apply. Unknown models, unsupported tier/storage billing, missing usage, invalid token counts, cache writes, paid hosted tools and other variable-cost requests stay unknown. Editing a model's route to another destination cannot inherit official prices. Existing saved presets gain this capability without a migration; generic providers remain unpriced.

Sources: [DeepSeek balance](https://api-docs.deepseek.com/api/get-user-balance/), [DeepSeek pricing](https://api-docs.deepseek.com/quick_start/pricing/), and [Zhipu pricing](https://docs.bigmodel.cn/cn/guide/start/pricing.md). The initial reference was checked on 2026-09-17. Neither the source checks nor automated tests used a real account Key or a paid inference request.
''')
append('docs/user/provider-presets.zh-CN.md', '''## 官网 API 余额与价格参考

DeepSeek API 与智谱 GLM API 预设现在提供独立账务参考面板，但保存的预设、API 类型、鉴权和官网目的地必须仍然匹配。推理继续使用 Configurable HTTP；任意 Custom API、改成中转地址的预设以及 Coding Plan 不会继承这项能力。

在 **账号** 页，DeepSeek 的 **刷新余额** 仅在点击时使用所选 Key 读取官网 `/user/balance`。总余额、赠送余额、充值余额分别展示，保留原币 CNY/USD 和查询时间；总余额已经包含两个分项，不会重复相加。刷新失败保留上次成功结果，不改变启停、认证状态、冷却或路由。换 Key 或修改地址后不会沿用旧余额。智谱明确显示未接入公开余额 API，不生成假余额，也不调用未经证实的控制台接口。

在 **供应商 → 价格** 页，这两种预设展示每百万 Token 的官网参考单价，并提供手工 **刷新价格表**。初始使用附日期的内置参考；参考有效期为 30 天，过期后的新请求保持未定价，直到刷新成功。官网结构不支持时保留旧参考但不延长有效期。打开页面和执行推理都不会自动请求官网价格或余额。

DeepSeek 使用官网 USD 价格，区分缓存命中、未命中以及工作日峰谷时段；CNY 余额不会换算或扣除 USD 估算。智谱使用官网 CNY 价格，写入原币费用，不会冒充美元。账号面板按 UTC 自然月统计 OCG 本地成功请求，按原币分别汇总，并显示费用未知的请求数。这是参考估算，不是实际账单；不包含 OCG 外的调用，也不回算历史请求。每次尝试固定价格版本与时段，流式完成时沿用同一份价格。

仅对有完整单价的准确上游模型计价。未知模型、不支持的分档或存储计费、缺少用量、无效 Token 数、缓存写入、收费托管工具及其他额外计费请求保持未知。把模型路由改到非官网地址后不会继承官网价格。已有匹配预设无需数据迁移；普通用户定义供应商仍未定价。

来源：[DeepSeek 余额](https://api-docs.deepseek.com/api/get-user-balance/)、[DeepSeek 价格](https://api-docs.deepseek.com/quick_start/pricing/)、[智谱价格](https://docs.bigmodel.cn/cn/guide/start/pricing.md)。初始参考核验于 2026-09-17。来源检查和自动测试均未使用真实账号 Key，也未发送收费推理请求。
''')
# Qualify older statements without replacing newer architecture or unrelated providers.
for name in ['docs/user/provider-presets.md', 'docs/user/provider-presets.zh-CN.md', 'docs/maintainer/runtime-invariants.md', 'docs/maintainer/runtime-invariants.zh-CN.md']:
    p = Path(name); s = p.read_text()
    s = s.replace('User-defined Providers are unpriced/unknown: no official usage, quota estimate, or pricing rows.', 'Generic user-defined Providers are unpriced/unknown. The matching DeepSeek/Zhipu official API preset exception is described under Official API Financial Evidence below; it does not add quota authority.')
    s = s.replace('用户定义供应商未定价/未知：没有官方用量、额度估算或价格行。', '普通用户定义供应商未定价/未知；匹配的 DeepSeek/智谱官网 API 预设例外见下文「官网 API 账务依据」，不增加额度裁决能力。')
    s = s.replace('新增供应商仍为未定价：不同步官方额度、余额和价格。', '除下文说明的匹配 DeepSeek/智谱官网 API 预设外，新增供应商仍未定价，不同步官方额度、余额和价格。')
    s = s.replace('New Providers remain unpriced: no official usage, balance, or pricing sync.', 'Except for the matching DeepSeek/Zhipu official API presets described below, new Providers remain unpriced with no official usage, balance, or pricing sync.')
    p.write_text(s)
append('docs/maintainer/runtime-invariants.md', '''## Official API Financial Evidence

- Matching saved `deepseek` and `zhipu` API presets can expose `model_source=official_api_preset` and financial reference pricing while inference stays Configurable HTTP. Both preset provenance and fixed destination/authentication are required; model-route overrides are checked again per attempt. Generic dynamic, Custom, Coding Plan and edited destinations do not inherit this exception.
- Additive authenticated V4 GETs `/accounts/{id}/official-api` and `/providers/{id}/official-api/pricing` are local projections. CAS-protected POSTs `/accounts/{id}/official-api/balance` and `/providers/{id}/official-api/pricing` are the only network paths. Balance requires the selected saved credential's endpoint and Origin grants, a ready Key, bounded no-redirect first-party I/O, a 15-second throttle and a post-I/O identity/CAS recheck. Public pricing fetches never carry a Key. Failure keeps prior evidence and never writes inference state.
- Reuse existing price-snapshot and credit-balance tables; no schema migration. Bind balance observations to credential ciphertext and saved destination. Delete provider-scoped reference snapshots with their dynamic Provider. GET never reparses a live web page. Prices expire after 30 days. Freeze revision/model/time per attempt; never reinterpret CNY as USD, sum currencies, debit a wallet from estimates, or reprice history. UTC-month projections count successful locally logged requests and separately expose unknown costs.
- Source parsing validates identity, units, duplicate rows, complete DeepSeek peak/off-peak pairs and supported Zhipu non-tiered token rates. Unsupported models, stale or corrupt prices, missing usage and variable-charge requests stay unknown. Zhipu balance is explicitly unavailable, not zero. Sources and initial date are documented in the paired provider preset guide.
''')
append('docs/maintainer/runtime-invariants.zh-CN.md', '''## 官网 API 账务依据

- 保存的 `deepseek` / `zhipu` API 预设只有在来源、官网目的地和鉴权同时匹配时，才公开 `model_source=official_api_preset` 与账务参考价格；推理仍走 Configurable HTTP。逐次尝试再次检查模型级路由覆盖。普通动态供应商、Custom、Coding Plan 与改过的非官网目的地不继承此能力。
- 受认证保护的新增 V4 GET `/accounts/{id}/official-api`、`/providers/{id}/official-api/pricing` 只投影本地数据。带 CAS 的 POST `/accounts/{id}/official-api/balance`、`/providers/{id}/official-api/pricing` 是唯一网络入口。余额要求所选就绪 Key 的已存端点和 Origin 授权，固定官网、有界响应、不跟随重定向、15 秒节流，并在网络后复查身份与 CAS。公共价格请求不携带 Key。失败保留已有依据，不写推理状态。
- 复用既有价格快照和余额表，不迁移数据库。余额绑定密文凭据与保存的目的地；删除动态供应商时清理其价格参考快照。GET 不读在线网页。价格有效期 30 天，每次尝试固定版本、模型和时间。CNY 不转写成 USD，不混币种求和，不从余额中扣除估算，不回算历史。按 UTC 月汇总本地成功调用，并单独记录未知费用数量。
- 来源解析校验身份、单位、重复行、DeepSeek 完整峰谷对和智谱可支持的非分档 Token 单价。不支持的模型、过期或损坏价格、缺失用量和额外收费请求保持未知。智谱余额明确为不可用，不是零。来源与初始日期见中英文预设使用指南。
''')
append('DESIGN.md', '''## Official API financial reference

Matching DeepSeek/Zhipu API presets expose a compact financial panel on Accounts and a reference-rate table on Providers → Pricing. Keep original currency, observation time, source and expiration visible. Balance and estimated spend are different facts and must never be combined into one progress meter. A provider without an integrated public balance API shows unavailable, never zero. Use existing typography, color and spacing tokens; do not add a navigation destination or automatic network refresh. Tables remain horizontally scrollable on narrow screens; action labels describe the explicit balance or price refresh.
''')
# A live opt-in check ties every shipped seed row to today's actual public document.
append('crates/ocg-core/src/official_api/tests.rs', '''#[tokio::test]
#[ignore = "explicit public seed evidence check; no Key or inference"]
async fn live_official_api_seed_rates_match_public_documents() {
    let config = crate::models::AppConfig { proxy_mode: crate::models::ProxyMode::Direct, ..Default::default() };
    for kind in [OfficialApiKind::Deepseek, OfficialApiKind::Zhipu] {
        let bytes = balance::fetch_bytes(&config, kind.pricing_url(), None, 2 * 1024 * 1024, 0).await.unwrap();
        let sheet = pricing::parse(kind, std::str::from_utf8(&bytes).unwrap(), Utc::now()).unwrap();
        for seed in pricing::seed(kind).rows {
            assert!(sheet.rows.contains(&seed), "seed has no matching public price evidence: {seed:?}");
        }
    }
}
''')
