from pathlib import Path
def write(path, text):
    p=Path(path); assert not p.exists(),path; p.parent.mkdir(parents=True,exist_ok=True); p.write_text(text)
write('crates/ocg-core/src/official_api/balance.rs', '''//! Selected-Key GET to a fixed first-party URL, bounded and without redirects.
use super::*;
use anyhow::{Result, anyhow, ensure};
use futures_util::StreamExt;
use serde_json::Value;
use std::collections::BTreeSet;

pub(crate) fn parse(body: &[u8], now: DateTime<Utc>) -> Result<Vec<OfficialBalance>> {
    let value: Value = serde_json::from_slice(body)?;
    ensure!(value.get("is_available").is_some_and(Value::is_boolean), "missing balance availability");
    let entries = value.get("balance_infos").and_then(Value::as_array).ok_or_else(||anyhow!("missing balance entries"))?;
    ensure!(!entries.is_empty() && entries.len() <= 2, "invalid balance count");
    let mut seen = BTreeSet::new();
    entries.iter().map(|entry| {
        let currency = entry.get("currency").and_then(Value::as_str).ok_or_else(||anyhow!("missing balance currency"))?;
        ensure!(matches!(currency,"USD"|"CNY") && seen.insert(currency), "invalid or duplicate balance currency");
        let amount = |field:&str| -> Result<f64> {
            let s=entry.get(field).and_then(Value::as_str).ok_or_else(||anyhow!("missing balance amount"))?;
            ensure!(s.len()<=40 && !s.is_empty() && s.bytes().enumerate().all(|(i,b)| b.is_ascii_digit() || b==b'.' || (i==0 && b==b'-')), "invalid balance amount");
            let value:f64=s.parse()?;
            ensure!(value.is_finite() && value.abs()<=1e15, "invalid balance amount");
            Ok(value)
        };
        Ok(OfficialBalance {currency:currency.into(),total:amount("total_balance")?,granted:amount("granted_balance")?,topped_up:amount("topped_up_balance")?,observed_at:now})
    }).collect()
}

pub(crate) async fn fetch(config: &crate::models::AppConfig, key:&str, generation:u64, now:impl FnOnce()->DateTime<Utc>) -> Result<Vec<OfficialBalance>> {
    let body=fetch_bytes(config,BALANCE_URL,Some(key),64*1024,generation).await?;
    parse(&body,now()).map_err(|_|anyhow!("official balance response has an unsupported schema"))
}

pub(crate) async fn fetch_bytes(config:&crate::models::AppConfig, url:&str, key:Option<&str>, limit:usize, generation:u64)->Result<Vec<u8>> {
    let endpoint = test_endpoint(generation,url);
    let client=crate::http_client::configured_builder(config).map_err(|_|anyhow!("official API client unavailable"))?
        .redirect(crate::http_client::no_redirect_policy())
        .connect_timeout(std::time::Duration::from_secs(config.connect_timeout_secs.clamp(5,30)))
        .timeout(std::time::Duration::from_secs(30)).build().map_err(|_|anyhow!("official API client unavailable"))?;
    let mut request=client.get(&endpoint).header(reqwest::header::ACCEPT,"application/json,text/plain,text/html");
    if let Some(key)=key { request=request.bearer_auth(key); }
    let response=request.send().await.map_err(|_|anyhow!("official API request failed"))?;
    ensure!(response.status()==reqwest::StatusCode::OK,"official API returned HTTP {}",response.status().as_u16());
    ensure!(response.content_length().is_none_or(|n|n<=limit as u64),"official API response exceeds size limit");
    let mut body=Vec::new(); let mut stream=response.bytes_stream();
    while let Some(chunk)=stream.next().await {
        let chunk=chunk.map_err(|_|anyhow!("official API body read failed"))?;
        ensure!(body.len().saturating_add(chunk.len())<=limit,"official API response exceeds size limit");
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(debug_assertions)]
static TEST_URLS: std::sync::LazyLock<parking_lot::Mutex<std::collections::HashMap<(u64,String),String>>> = std::sync::LazyLock::new(Default::default);

fn test_endpoint(generation:u64,url:&str)->String {
    #[cfg(debug_assertions)]
    if let Some(value)=TEST_URLS.lock().get(&(generation,url.into())).cloned() { return value; }
    let _=generation; url.into()
}

#[cfg(debug_assertions)]
#[doc(hidden)]
pub struct OfficialApiTestGuard { generation:u64, source:String }
#[cfg(debug_assertions)]
impl Drop for OfficialApiTestGuard { fn drop(&mut self) { TEST_URLS.lock().remove(&(self.generation,self.source.clone())); } }

#[cfg(debug_assertions)]
#[doc(hidden)]
pub fn install_official_api_endpoint_for_test(generation:u64,source:&str,target:&str)->Result<OfficialApiTestGuard> {
    ensure!(matches!(source,BALANCE_URL|DEEPSEEK_PRICING_URL|ZHIPU_PRICING_URL),"unknown test source");
    let url=reqwest::Url::parse(target)?;
    ensure!(url.scheme()=="http" && matches!(url.host_str(),Some("127.0.0.1"|"[::1]"|"::1")) && url.username().is_empty() && url.password().is_none() && url.query().is_none() && url.fragment().is_none(),"test endpoint must be literal loopback HTTP");
    let mut urls=TEST_URLS.lock();
    ensure!(!urls.contains_key(&(generation,source.into())),"test override already installed");
    urls.insert((generation,source.into()),target.into());
    Ok(OfficialApiTestGuard {generation,source:source.into()})
}
''')
write('crates/ocg-core/src/official_api/pricing.rs', '''//! Bounded parsers for first-party public price evidence. No inference Key is used.
use super::*;
use anyhow::{Result, anyhow, ensure};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn seed(kind: OfficialApiKind) -> OfficialPriceSheet {
    let at = DateTime::parse_from_rfc3339("2026-09-17T00:00:00Z").expect("constant date").with_timezone(&Utc);
    let mut rows = Vec::new();
    match kind {
        OfficialApiKind::Deepseek => {
            for (model, input, output, cache) in [
                ("deepseek-flash", 0.3, 1.2, 0.006),
                ("deepseek-v4-flash", 0.3, 1.2, 0.006),
                ("deepseek-v4-flash-vision-exp", 0.3, 1.2, 0.006),
                ("deepseek-v4-pro", 1.32, 3.96, 0.044),
            ] {
                for (period, factor) in [("peak", 1.0), ("off_peak", 0.5)] {
                    rows.push(row(model, "USD", period, input*factor, output*factor, Some(cache*factor)));
                }
            }
        }
        OfficialApiKind::Zhipu => {
            for (model, input, output, cache) in [
                ("glm-5.3", 8.0, 28.0, 2.0), ("glm-5.3-flash", 0.8, 2.8, 0.23),
                ("glm-5.2", 8.0, 28.0, 2.0), ("glm-4.7-flashx", 0.5, 3.0, 0.1),
                ("glm-4.7-flash", 0.0, 0.0, 0.0),
            ] { rows.push(row(model, "CNY", "all", input, output, Some(cache))); }
        }
    }
    finish(kind, rows, at).expect("reviewed seed is valid")
}

fn row(model: &str, currency: &str, period: &str, input: f64, output: f64, cache: Option<f64>) -> OfficialPriceRow {
    OfficialPriceRow { model:model.into(), currency:currency.into(), period:period.into(), input_per_million:input, output_per_million:output, cache_read_per_million:cache }
}

fn finish(kind: OfficialApiKind, rows: Vec<OfficialPriceRow>, now: DateTime<Utc>) -> Result<OfficialPriceSheet> {
    ensure!(!rows.is_empty() && rows.len() <= 256, "official price rows are missing or too numerous");
    let encoded = serde_json::to_vec(&(now, &rows))?;
    let revision = format!("official-api:{}:{}", kind.id(), hex::encode(Sha256::digest(encoded)));
    let sheet = OfficialPriceSheet { kind, revision, source_url:kind.pricing_url().into(), observed_at:now, valid_until: now + chrono::Duration::days(PRICE_MAX_AGE_DAYS), rows };
    validate(&sheet)?;
    Ok(sheet)
}

pub(crate) fn validate(sheet: &OfficialPriceSheet) -> Result<()> {
    let revision = format!("official-api:{}:{}", sheet.kind.id(), hex::encode(Sha256::digest(serde_json::to_vec(&(sheet.observed_at, &sheet.rows))?)));
    ensure!(sheet.source_url == sheet.kind.pricing_url() && sheet.revision == revision, "invalid price source or revision");
    ensure!(sheet.valid_until > sheet.observed_at && sheet.valid_until - sheet.observed_at <= chrono::Duration::days(PRICE_MAX_AGE_DAYS), "invalid price lifetime");
    ensure!(!sheet.rows.is_empty() && sheet.rows.len() <= 256, "invalid price count");
    let mut identities = BTreeSet::new();
    for r in &sheet.rows {
        ensure!(r.model.len() <= 128 && !r.model.is_empty() && r.model.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.')), "invalid price model");
        ensure!(r.currency == sheet.kind.currency(), "price currency mismatch");
        ensure!(match sheet.kind { OfficialApiKind::Deepseek => matches!(r.period.as_str(), "peak" | "off_peak"), OfficialApiKind::Zhipu => r.period == "all" }, "invalid price period");
        ensure!(identities.insert((&r.model, &r.period)), "duplicate price row");
        for v in [Some(r.input_per_million), Some(r.output_per_million), r.cache_read_per_million].into_iter().flatten() {
            ensure!(v.is_finite() && (0.0..=100_000.0).contains(&v), "invalid price amount");
        }
    }
    if sheet.kind == OfficialApiKind::Deepseek {
        for r in &sheet.rows { ensure!(sheet.rows.iter().filter(|other| other.model == r.model).count() == 2, "incomplete peak/off-peak pair"); }
    }
    Ok(())
}

pub(crate) fn parse(kind: OfficialApiKind, document: &str, now: DateTime<Utc>) -> Result<OfficialPriceSheet> {
    let rows = match kind { OfficialApiKind::Deepseek => parse_deepseek(document)?, OfficialApiKind::Zhipu => parse_zhipu(document)? };
    finish(kind, rows, now)
}

fn number(text: &str) -> Result<f64> {
    if text.trim() == "免费" { return Ok(0.0); }
    let text = text.trim().trim_start_matches('$');
    ensure!(!text.is_empty() && text.bytes().all(|b| b.is_ascii_digit() || b == b'.'), "invalid price number");
    let value: f64 = text.parse()?;
    ensure!(value.is_finite() && (0.0..=100_000.0).contains(&value), "invalid price number");
    Ok(value)
}

fn parse_deepseek(document: &str) -> Result<Vec<OfficialPriceRow>> {
    let plain = crate::pricing::strip_tags(document);
    for marker in ["01:00 - 04:00", "06:00 - 10:00", "Monday through Friday"] { ensure!(plain.contains(marker), "DeepSeek schedule changed"); }
    let tables = crate::pricing::extract_tables(document)?;
    let mut candidates = tables.iter().filter(|t| t.first().and_then(|r| r.first()).is_some_and(|s| s.trim() == "MODEL"));
    let table = candidates.next().ok_or_else(|| anyhow!("missing DeepSeek price table"))?;
    ensure!(candidates.next().is_none(), "ambiguous DeepSeek price table");
    let names: Vec<_> = table[0].iter().skip(1).map(|s| s.split('(').next().unwrap_or(s).trim().to_string()).collect();
    ensure!(!names.is_empty() && names.len() <= 16 && names.iter().all(|s| s.starts_with("deepseek-")), "invalid DeepSeek models");
    let mut values: BTreeMap<(usize,String,String),f64> = BTreeMap::new();
    let mut metric = "";
    for cells in table.iter().skip(1) {
        if cells.iter().any(|s| s.contains("CACHE HIT")) { metric="cache"; }
        else if cells.iter().any(|s| s.contains("CACHE MISS")) { metric="input"; }
        else if cells.iter().any(|s| s.contains("1M OUTPUT TOKENS")) { metric="output"; }
        let Some(i) = cells.iter().position(|s| matches!(s.as_str(), "PEAK" | "OFF-PEAK")) else { continue; };
        ensure!(!metric.is_empty() && cells.len() == i + 1 + names.len(), "invalid DeepSeek price columns");
        let period = if cells[i] == "PEAK" { "peak" } else { "off_peak" };
        for (j, amount) in cells.iter().skip(i+1).enumerate() {
            ensure!(values.insert((j,period.into(),metric.into()), number(amount)?).is_none(), "duplicate DeepSeek rate");
        }
    }
    let mut rows = Vec::new();
    for (j, model) in names.iter().enumerate() { for period in ["peak", "off_peak"] {
        let get = |metric:&str| values.get(&(j,period.into(),metric.into())).copied().ok_or_else(||anyhow!("incomplete DeepSeek price"));
        rows.push(row(model,"USD",period,get("input")?,get("output")?,Some(get("cache")?)));
    }}
    // These documented temporary aliases are copied only while the source
    // still explicitly states that both are billed at the Flash price.
    if plain.contains("billed at the Flash price") && plain.contains("deepseek-v4-flash-vision-exp") && plain.contains("deepseek-v4-flash") {
        let flash: Vec<_> = rows.iter().filter(|r| r.model == "deepseek-flash").cloned().collect();
        for alias in ["deepseek-v4-flash", "deepseek-v4-flash-vision-exp"] {
            if !names.iter().any(|s| s == alias) { for r in &flash { let mut r=r.clone(); r.model=alias.into(); rows.push(r); } }
        }
    }
    Ok(rows)
}

fn parse_zhipu(document: &str) -> Result<Vec<OfficialPriceRow>> {
    ensure!(document.contains("元/百万 Tokens") && document.contains("缓存命中"), "Zhipu price units changed");
    let mut rows = Vec::new();
    let mut excluded = BTreeSet::new();
    let mut header_valid = false;
    for line in document.lines() {
        let line = line.trim();
        if line.starts_with("### 视觉理解") || line.starts_with("### 多模态生成") { break; }
        if !line.starts_with('|') { continue; }
        let cells: Vec<_> = line.trim_matches('|').split('|').map(str::trim).collect();
        if cells.first().is_some_and(|s| *s == "模型名称") {
            header_valid = cells.get(2).is_some_and(|s| s.contains("输入单价")) && cells.get(3).is_some_and(|s| s.contains("输出单价")) && cells.get(5).is_some_and(|s| s.contains("缓存命中"));
            continue;
        }
        if !header_valid || cells.len() < 6 || !cells[0].starts_with("GLM-") { continue; }
        let model = cells[0].to_ascii_lowercase();
        // Prompt/output-tier and charged storage rules need additional billing
        // evidence. Never take one tier and silently apply it to every request.
        if cells[1].contains("输入") || cells[1].contains("输出") || !matches!(cells[4], "免费" | "限时免费") { excluded.insert(model); continue; }
        let cache = if cells[5] == "不支持" { None } else { Some(number(cells[5])?) };
        rows.push(row(&model,"CNY","all",number(cells[2])?,number(cells[3])?,cache));
    }
    rows.retain(|r| !excluded.contains(&r.model));
    Ok(rows)
}

pub(crate) async fn fetch(config: &crate::models::AppConfig, kind: OfficialApiKind, generation: u64, now: impl FnOnce()->DateTime<Utc>) -> Result<OfficialPriceSheet> {
    let body = super::balance::fetch_bytes(config, kind.pricing_url(), None, 2*1024*1024, generation).await?;
    let text = std::str::from_utf8(&body).map_err(|_|anyhow!("official pricing is not UTF-8"))?;
    parse(kind, text, now()).map_err(|_|anyhow!("official pricing schema is unsupported; previous evidence was retained"))
}
''')
