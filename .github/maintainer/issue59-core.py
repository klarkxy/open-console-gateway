from pathlib import Path
R = Path('.')
def edit(path, old, new):
    p=R/path; s=p.read_text(); assert s.count(old)==1,(path,s.count(old),old[:100]); p.write_text(s.replace(old,new))
def write(path, text):
    p=R/path; assert not p.exists(),path; p.parent.mkdir(parents=True,exist_ok=True); p.write_text(text)

edit('crates/ocg-core/src/lib.rs','pub(crate) mod official_protocols;','pub mod official_api;\npub(crate) mod official_protocols;')
edit('crates/ocg-core/src/db.rs','mod platform;','mod platform;\nmod official_api;')
edit('crates/ocg-core/src/db.rs','        tx.execute("DELETE FROM providers WHERE id = ?1", [&existing.id])?;','        tx.execute("DELETE FROM provider_pricing_snapshots WHERE provider_id = ?1", [&existing.id])?;\n        tx.execute("DELETE FROM providers WHERE id = ?1", [&existing.id])?;')
edit('crates/ocg-core/src/dashboard_v3/mod.rs','    fn outbound_failed(state:', '    pub(crate) fn outbound_failed(state:')
edit('crates/ocg-core/src/dashboard_v3/mod.rs','    pub(crate) fn conflict_at(state:', '''    pub(crate) fn throttled_at(state: &CoreState, message: impl Into<String>) -> Self {
        Self { status: StatusCode::TOO_MANY_REQUESTS, body: V3Error {
            code: ERROR_THROTTLED.into(), message: message.into(),
            current_revision: Some(state.settings_revision()), process_generation: Some(state.process_generation()),
        }}
    }

    pub(crate) fn conflict_at(state:''')
write('crates/ocg-core/src/official_api.rs', '''//! Official API financial evidence for matching saved presets, never arbitrary URLs.
//!
//! Inference stays on Configurable HTTP. Prices are reference estimates, not a
//! bill, wallet, quota authority, or exchange-rate conversion. Explicit refresh
//! is the only network path. Account balance failure never changes routing.
pub(crate) mod balance;
pub(crate) mod pricing;
#[cfg(debug_assertions)]
#[doc(hidden)]
pub use balance::{OfficialApiTestGuard, install_official_api_endpoint_for_test};

use chrono::{DateTime, Datelike, Timelike, Utc, Weekday};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use crate::dynamic::DynamicProviderRuntime;
use crate::models::Account;
use crate::provider::UpstreamProtocolKind;
use ocg_domain::dynamic::DynamicAuthKind;

pub const PRICE_MAX_AGE_DAYS: i64 = 30;
pub const BALANCE_URL: &str = "https://api.deepseek.com/user/balance";
pub const DEEPSEEK_PRICING_URL: &str = "https://api-docs.deepseek.com/quick_start/pricing/";
pub const ZHIPU_PRICING_URL: &str = "https://docs.bigmodel.cn/cn/guide/start/pricing.md";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OfficialApiKind { Deepseek, Zhipu }

impl OfficialApiKind {
    pub fn pricing_url(self) -> &'static str { match self { Self::Deepseek => DEEPSEEK_PRICING_URL, Self::Zhipu => ZHIPU_PRICING_URL } }
    pub fn currency(self) -> &'static str { match self { Self::Deepseek => "USD", Self::Zhipu => "CNY" } }
    pub fn balance_available(self) -> bool { self == Self::Deepseek }
    pub fn id(self) -> &'static str { match self { Self::Deepseek => "deepseek", Self::Zhipu => "zhipu" } }
}

/// Preset provenance is necessary, not sufficient: edited destinations and
/// Coding Plan paths never inherit official API financial evidence.
pub fn kind_for_runtime(runtime: &DynamicProviderRuntime) -> Option<OfficialApiKind> {
    if runtime.auth_kind != DynamicAuthKind::Bearer || runtime.offering != "api" { return None; }
    let kind = match runtime.preset_id.as_deref()? { "deepseek" => OfficialApiKind::Deepseek, "zhipu" => OfficialApiKind::Zhipu, _ => return None };
    route_is_official(kind, &runtime.endpoint_url, runtime.upstream_protocol).then_some(kind)
}

pub fn route_is_official(kind: OfficialApiKind, endpoint: &str, protocol: UpstreamProtocolKind) -> bool {
    let Ok(url) = reqwest::Url::parse(endpoint) else { return false; };
    if url.scheme() != "https" || url.port_or_known_default() != Some(443)
        || !url.username().is_empty() || url.password().is_some() || url.query().is_some() || url.fragment().is_some() { return false; }
    let path = url.path().trim_end_matches('/');
    match kind {
        OfficialApiKind::Deepseek if url.host_str() == Some("api.deepseek.com") => match protocol {
            UpstreamProtocolKind::ChatCompletions => matches!(path, "" | "/v1" | "/chat/completions" | "/v1/chat/completions"),
            UpstreamProtocolKind::Responses => matches!(path, "" | "/v1" | "/responses" | "/v1/responses"),
            UpstreamProtocolKind::Messages => matches!(path, "/anthropic" | "/anthropic/v1" | "/anthropic/v1/messages"),
        },
        OfficialApiKind::Zhipu if url.host_str() == Some("open.bigmodel.cn") => protocol == UpstreamProtocolKind::ChatCompletions && matches!(path, "/api/paas/v4" | "/api/paas/v4/chat/completions"),
        _ => false,
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OfficialPriceRow {
    pub model: String,
    pub currency: String,
    /// `all`, or DeepSeek `peak` / `off_peak` at the frozen attempt time.
    pub period: String,
    pub input_per_million: f64,
    pub output_per_million: f64,
    pub cache_read_per_million: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OfficialPriceSheet {
    pub kind: OfficialApiKind,
    pub revision: String,
    pub source_url: String,
    pub observed_at: DateTime<Utc>,
    pub valid_until: DateTime<Utc>,
    pub rows: Vec<OfficialPriceRow>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OfficialBalance {
    pub currency: String,
    pub total: f64,
    pub granted: f64,
    pub topped_up: f64,
    pub observed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OfficialSpend {
    pub currency: String,
    pub amount: f64,
    pub priced_requests: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OfficialApiStatus {
    pub account_id: String,
    pub provider_id: String,
    pub kind: OfficialApiKind,
    pub balance_available: bool,
    pub balances: Vec<OfficialBalance>,
    pub prices: OfficialPriceSheet,
    pub month_started_at: DateTime<Utc>,
    pub month_spend: Vec<OfficialSpend>,
    pub unpriced_requests: u64,
    pub revision: u64,
    pub process_generation: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OfficialApiPrices {
    pub provider_id: String,
    pub prices: OfficialPriceSheet,
    pub revision: u64,
    pub process_generation: u64,
}

/// The hash binds balance evidence to ciphertext + provider configuration;
/// neither old-Key results nor results for a changed endpoint are displayed.
pub(crate) fn balance_source(account: &Account, runtime: &DynamicProviderRuntime) -> String {
    let mut hash = Sha256::new();
    for value in [&account.id, &account.key_cipher, &runtime.id, &runtime.endpoint_url] {
        hash.update(value.len().to_le_bytes()); hash.update(value.as_bytes());
    }
    format!("official-api-balance:{}", hex::encode(hash.finalize()))
}

pub(crate) fn peak_at(now: DateTime<Utc>) -> bool {
    !matches!(now.weekday(), Weekday::Sat | Weekday::Sun)
        && ((1..4).contains(&now.hour()) || (6..10).contains(&now.hour()))
}

/// Frozen per-attempt reference; a refresh cannot reprice a stream in flight.
#[derive(Clone)]
pub(crate) struct OfficialAttemptPrice {
    pub provider_id: String,
    pub sheet: OfficialPriceSheet,
    pub model: String,
    pub at: DateTime<Utc>,
}

impl OfficialAttemptPrice {
    pub fn amount(&self, prompt: i64, output: i64, cached: i64, created: i64) -> Option<f64> {
        if self.at < self.sheet.observed_at || self.at >= self.sheet.valid_until || created != 0
            || prompt < 0 || output < 0 || cached < 0 || cached > prompt { return None; }
        let period = if peak_at(self.at) { "peak" } else { "off_peak" };
        let mut rows = self.sheet.rows.iter().filter(|row| row.model == self.model && (row.period == "all" || row.period == period));
        let row = rows.next()?;
        if rows.next().is_some() || row.currency != self.sheet.kind.currency() { return None; }
        let cache_rate = if cached == 0 { 0.0 } else { row.cache_read_per_million? };
        let value = ((prompt - cached) as f64 * row.input_per_million + output as f64 * row.output_per_million + cached as f64 * cache_rate) / 1_000_000.0;
        (value.is_finite() && value >= 0.0).then_some(value)
    }
}

#[cfg(test)]
pub(crate) mod tests;
''')
write('crates/ocg-core/src/db/official_api.rs', '''//! Typed financial evidence in the existing price and balance tables.
use super::*;
use crate::official_api::{self, OfficialApiKind, OfficialPriceSheet, OfficialBalance, OfficialSpend};

impl Database {
    pub(crate) fn official_api_prices(&self, provider: &str, kind: OfficialApiKind) -> Result<OfficialPriceSheet> {
        match self.latest_provider_pricing_snapshot(provider)? {
            None => Ok(official_api::pricing::seed(kind)),
            Some(row) => {
                let sheet: OfficialPriceSheet = serde_json::from_str(&row.snapshot_json)?;
                anyhow::ensure!(sheet.kind == kind && row.source_url == sheet.source_url && row.revision == sheet.revision, "official price identity mismatch");
                official_api::pricing::validate(&sheet)?;
                Ok(sheet)
            }
        }
    }

    pub(crate) fn store_official_api_prices(&self, runtime: &DynamicProviderRuntime, sheet: &OfficialPriceSheet) -> Result<()> {
        anyhow::ensure!(official_api::kind_for_runtime(runtime) == Some(sheet.kind), "official price provider mismatch");
        official_api::pricing::validate(sheet)?;
        let tx = self.conn.unchecked_transaction()?;
        let current = get_dynamic_provider_on(&tx, &runtime.id)?.ok_or_else(|| anyhow::anyhow!("provider removed"))?;
        anyhow::ensure!(current == *runtime, "provider changed");
        tx.execute("INSERT OR IGNORE INTO provider_pricing_snapshots
            (provider_id,revision,activated_at,document_updated_at,source_url,content_hash,snapshot_json)
            VALUES (?1,?2,?3,?3,?4,?5,?6)", params![runtime.id, sheet.revision, sheet.observed_at.to_rfc3339(), sheet.source_url,
                sheet.revision.rsplit(':').next().unwrap_or_default(), serde_json::to_string(sheet)?])?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn official_api_balances(&self, account: &Account, runtime: &DynamicProviderRuntime) -> Result<Vec<OfficialBalance>> {
        let source = official_api::balance_source(account, runtime);
        let rows = self.list_credit_balances(&account.id)?;
        let mut result = Vec::new();
        for currency in ["CNY", "USD"] {
            let part = |kind: &str| rows.iter().find(|row| row.balance_kind == format!("official_{currency}_{kind}") && row.source == source && row.unit == currency);
            if let (Some(total), Some(granted), Some(topped_up)) = (part("total"), part("granted"), part("topped_up"))
                && let Some(observed_at) = total.observed_at
                && granted.observed_at == Some(observed_at) && topped_up.observed_at == Some(observed_at)
                && [total.amount, granted.amount, topped_up.amount].iter().all(|v| v.is_finite()) {
                result.push(OfficialBalance { currency:currency.into(),total:total.amount,granted:granted.amount,topped_up:topped_up.amount,observed_at });
            }
        }
        Ok(result)
    }

    pub(crate) fn store_official_api_balances(&self, account: &Account, runtime: &DynamicProviderRuntime, balances: &[OfficialBalance], now: DateTime<Utc>) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        let cipher: String = tx.query_row("SELECT key_cipher FROM accounts WHERE id=?1 AND provider_id=?2", params![account.id,runtime.id], |row|row.get(0))?;
        anyhow::ensure!(cipher == account.key_cipher, "credential changed");
        let source = official_api::balance_source(account, runtime);
        tx.execute("DELETE FROM credit_balances WHERE account_id=?1 AND balance_kind LIKE 'official_%'", [&account.id])?;
        for balance in balances {
            anyhow::ensure!(matches!(balance.currency.as_str(),"CNY"|"USD"),"unsupported balance currency");
            for (kind, amount) in [("total",balance.total),("granted",balance.granted),("topped_up",balance.topped_up)] {
                anyhow::ensure!(amount.is_finite(), "invalid balance amount");
                tx.execute("INSERT INTO credit_balances(account_id,balance_kind,amount,unit,source,observed_at,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                    params![account.id,format!("official_{}_{kind}",balance.currency),amount,balance.currency,source,balance.observed_at.to_rfc3339(),now.to_rfc3339()])?;
            }
        }
        tx.execute("UPDATE provider_usage_sync_state SET last_attempt_at=?1,last_success_at=?1 WHERE account_id=?2",params![now.to_rfc3339(),account.id])?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn official_api_spend(&self, account: &Account, since: DateTime<Utc>, until: DateTime<Utc>) -> Result<(Vec<OfficialSpend>,u64)> {
        let mut stmt=self.conn.prepare("SELECT native_cost_currency,SUM(native_cost_value),COUNT(*) FROM forward_logs
            WHERE account_id=?1 AND provider_id=?2 AND julianday(timestamp)>=julianday(?3) AND julianday(timestamp)<=julianday(?4)
              AND status IN ('success','success_unpriced','success_no_usage') AND pricing_revision_id LIKE 'official-api:%'
              AND native_cost_value IS NOT NULL AND native_cost_currency IN ('CNY','USD')
            GROUP BY native_cost_currency ORDER BY native_cost_currency")?;
        let rows=stmt.query_map(params![account.id,account.provider_id,since.to_rfc3339(),until.to_rfc3339()],|r|Ok(OfficialSpend{currency:r.get(0)?,amount:r.get(1)?,priced_requests:r.get(2)?}))?;
        let spend=rows.collect::<rusqlite::Result<Vec<_>>>()?;
        let unknown:u64=self.conn.query_row("SELECT COUNT(*) FROM forward_logs WHERE account_id=?1 AND provider_id=?2
            AND julianday(timestamp)>=julianday(?3) AND julianday(timestamp)<=julianday(?4)
            AND status IN ('success','success_unpriced','success_no_usage')
            AND (pricing_revision_id IS NULL OR pricing_revision_id NOT LIKE 'official-api:%' OR native_cost_value IS NULL)",params![account.id,account.provider_id,since.to_rfc3339(),until.to_rfc3339()],|r|r.get(0))?;
        Ok((spend,unknown))
    }
}
''')
write('crates/ocg-core/src/dashboard_v4/official_api.rs', '''//! Explicit first-party financial refresh; GETs and inference never fetch.
use axum::{Json,body::Bytes,extract::{Path,State}};
use chrono::{Datelike,TimeZone,Utc};
use crate::dashboard_v3::{V3ApiError,MutationExpectation,check_expectation,parse_mutation_json};
use crate::db::Database;
use crate::dynamic::DynamicProviderRuntime;
use crate::models::{Account,AccountSetupStep};
use crate::official_api::{self,OfficialApiKind,OfficialApiStatus,OfficialApiPrices};
use crate::state::CoreState;
use ocg_domain::connection::{LegacyConnectionKind,EndpointOperation,connection_id_for_legacy,endpoint_id_for};

fn runtime(db:&Database, id:&str, state:&CoreState)->Result<(DynamicProviderRuntime,OfficialApiKind),V3ApiError> {
    let runtime=db.list_dynamic_providers().map_err(V3ApiError::internal)?.into_iter().find(|r|r.id==id)
        .ok_or_else(||V3ApiError::not_found_at(state,"configured provider not found"))?;
    let kind=official_api::kind_for_runtime(&runtime).ok_or_else(||V3ApiError::invalid_request_at(state,"official financial evidence is unavailable for this preset or destination"))?;
    Ok((runtime,kind))
}
fn account(db:&Database,id:&str,state:&CoreState)->Result<Account,V3ApiError>{
    db.get_account(id).map_err(V3ApiError::internal)?.ok_or_else(||V3ApiError::not_found_at(state,"account not found"))
}
fn status(state:&CoreState,id:&str)->Result<OfficialApiStatus,V3ApiError>{
    let _settings=state.settings_update.lock(); let db=state.db.lock();
    let account=account(&db,id,state)?; let (runtime,kind)=runtime(&db,&account.provider_id,state)?;
    let now=state.usage_sync.now();
    let since=Utc.with_ymd_and_hms(now.year(),now.month(),1,0,0,0).single().expect("valid UTC month");
    let (spend,unpriced)=db.official_api_spend(&account,since,now).map_err(V3ApiError::internal)?;
    Ok(OfficialApiStatus{account_id:id.into(),provider_id:runtime.id.clone(),kind,balance_available:kind.balance_available(),
        balances:db.official_api_balances(&account,&runtime).map_err(V3ApiError::internal)?,
        prices:db.official_api_prices(&runtime.id,kind).map_err(V3ApiError::internal)?,
        month_started_at:since,month_spend:spend,unpriced_requests:unpriced,revision:state.settings_revision(),process_generation:state.process_generation()})
}
fn prices(state:&CoreState,id:&str)->Result<OfficialApiPrices,V3ApiError>{
    let _settings=state.settings_update.lock(); let db=state.db.lock(); let (_,kind)=runtime(&db,id,state)?;
    Ok(OfficialApiPrices{provider_id:id.into(),prices:db.official_api_prices(id,kind).map_err(V3ApiError::internal)?,revision:state.settings_revision(),process_generation:state.process_generation()})
}
pub(super) async fn get_status(State(state):State<CoreState>,Path(id):Path<String>)->Result<Json<OfficialApiStatus>,V3ApiError>{status(&state,&id).map(Json)}
pub(super) async fn get_prices(State(state):State<CoreState>,Path(id):Path<String>)->Result<Json<OfficialApiPrices>,V3ApiError>{prices(&state,&id).map(Json)}

/// A billing read uses only a Key whose saved default endpoint and Origin are
/// both still granted. It does not grant a new billing destination on its own.
fn require_grant(db:&Database,account:&Account,runtime:&DynamicProviderRuntime,state:&CoreState)->Result<(),V3ApiError>{
    let binding=db.list_inference_bindings().map_err(V3ApiError::internal)?.into_iter().find(|b|b.account_id==account.id)
        .ok_or_else(||V3ApiError::invalid_request_at(state,"selected credential binding is unavailable"))?;
    let endpoint=endpoint_id_for(&connection_id_for_legacy(LegacyConnectionKind::DynamicProvider,&runtime.id),EndpointOperation::from(runtime.upstream_protocol)).to_string();
    if !binding.enabled || !binding.allowed_endpoint_ids.contains(&endpoint)
        || crate::custom_http::ensure_secret_origin_granted(official_api::BALANCE_URL,&binding.allowed_origins).is_err() {
        return Err(V3ApiError::invalid_request_at(state,"the official destination is not authorized for this Key"));
    }
    Ok(())
}

pub(super) async fn refresh_balance(State(state):State<CoreState>,Path(id):Path<String>,body:Bytes)->Result<Json<OfficialApiStatus>,V3ApiError>{
    let expectation=parse_mutation_json::<MutationExpectation>(&body)?;
    let _refresh=state.provider_usage_refresh.try_lock().map_err(|_|V3ApiError::conflict_at(&state,"provider usage refresh is already running"))?;
    let (snapshot,provider,config,key)={
        let _settings=state.settings_update.lock();check_expectation(&state,&expectation)?;let db=state.db.lock();
        let account=account(&db,&id,&state)?;let (runtime,kind)=runtime(&db,&account.provider_id,&state)?;
        if !kind.balance_available(){return Err(V3ApiError::invalid_request_at(&state,"no supported public balance API is configured for this provider"));}
        if account.setup_step!=AccountSetupStep::Ready || account.key_cipher.is_empty(){return Err(V3ApiError::invalid_request_at(&state,"a ready account with a stored Key is required"));}
        require_grant(&db,&account,&runtime,&state)?;
        if crate::usage_sync::manual_next_allowed_at(db.account_usage_sync_state(&id).map_err(V3ApiError::internal)?.and_then(|s|s.last_attempt_at),state.usage_sync.now()).is_some(){
            return Err(V3ApiError::throttled_at(&state,"official balance refresh is limited to once per 15 seconds"));
        }
        let key=state.decrypt_key(&account.key_cipher).map_err(V3ApiError::internal)?;
        (account,runtime,state.config(),key)
    };
    let fetched=official_api::balance::fetch(&config,&key,state.process_generation(),||state.usage_sync.now()).await;drop(key);
    {
        let _settings=state.settings_update.lock();check_expectation(&state,&expectation)?;let db=state.db.lock();
        let current=account(&db,&id,&state)?;let (current_provider,_)=runtime(&db,&provider.id,&state)?;
        if current.key_cipher!=snapshot.key_cipher || current.updated_at!=snapshot.updated_at || current.provider_id!=snapshot.provider_id || current_provider!=provider {
            return Err(V3ApiError::conflict_at(&state,"account or provider changed during official balance refresh"));
        }
        require_grant(&db,&current,&current_provider,&state)?;
        let now=state.usage_sync.now();
        match fetched {
            Ok(balances)=>db.store_official_api_balances(&current,&provider,&balances,now).map_err(V3ApiError::internal)?,
            Err(_)=>{
                db.touch_account_usage_sync_attempt(&id,now).map_err(V3ApiError::internal)?;
                return Err(V3ApiError::outbound_failed(&state,"official balance refresh failed; previous evidence retained"));
            }
        }
    }
    status(&state,&id).map(Json)
}

pub(super) async fn refresh_prices(State(state):State<CoreState>,Path(id):Path<String>,body:Bytes)->Result<Json<OfficialApiPrices>,V3ApiError>{
    let expectation=parse_mutation_json::<MutationExpectation>(&body)?;
    let _refresh=state.pricing_refresh.try_lock().map_err(|_|V3ApiError::conflict_at(&state,"pricing refresh is already running"))?;
    let (provider,kind,config)={
        let _settings=state.settings_update.lock();check_expectation(&state,&expectation)?;let db=state.db.lock();
        let (provider,kind)=runtime(&db,&id,&state)?;(provider,kind,state.config())
    };
    let fetched=official_api::pricing::fetch(&config,kind,state.process_generation(),||state.usage_sync.now()).await;
    {
        let _settings=state.settings_update.lock();check_expectation(&state,&expectation)?;let db=state.db.lock();
        let (current,_)=runtime(&db,&id,&state)?;
        if current!=provider {return Err(V3ApiError::conflict_at(&state,"provider changed during official pricing refresh"));}
        let sheet=fetched.map_err(|_|V3ApiError::outbound_failed(&state,"official pricing refresh failed; previous evidence retained"))?;
        db.store_official_api_prices(&current,&sheet).map_err(V3ApiError::internal)?;
    }
    prices(&state,&id).map(Json)
}
''')
edit('crates/ocg-core/src/dashboard_v4/mod.rs','mod onboarding;', 'mod onboarding;\nmod official_api;')
edit('crates/ocg-core/src/dashboard_v4/mod.rs','//! outbound network requests.', '//! outbound network requests except the explicit official-API balance/price refreshes.\n//! Their GET projections and inference stay local-only.')
edit('crates/ocg-core/src/dashboard_v4/mod.rs','        .route("/accounts", get(identities::list_accounts))', '''        .route("/accounts", get(identities::list_accounts))
        .route("/accounts/{id}/official-api", get(official_api::get_status))
        .route("/accounts/{id}/official-api/balance", post(official_api::refresh_balance))
        .route("/providers/{id}/official-api/pricing", get(official_api::get_prices).post(official_api::refresh_prices))''')
edit('crates/ocg-core/src/dashboard_v4/types.rs','    "DshApplicationInstallRequest",\n];','''    "DshApplicationInstallRequest",
    "OfficialApiKind", "OfficialPriceRow", "OfficialPriceSheet", "OfficialBalance",
    "OfficialSpend", "OfficialApiStatus", "OfficialApiPrices",
];''')
edit('crates/ocg-core/src/dashboard_v4/types.rs','    include_type::<DshApplication>(&mut serialize);', '''    include_type::<DshApplication>(&mut serialize);
    include_type::<crate::official_api::OfficialApiStatus>(&mut serialize);
    include_type::<crate::official_api::OfficialApiPrices>(&mut serialize);''')
edit('crates/ocg-core/src/dashboard_v3/providers.rs','        model_source: "dynamic_provider".into(),', '''        model_source: if crate::official_api::kind_for_runtime(runtime).is_some() { "official_api_preset" } else { "dynamic_provider" }.into(),''')
p=R/'crates/ocg-core/src/dashboard_v3/providers.rs';s=p.read_text();a=s.index('fn dynamic_catalog_entry(');s=s[:a]+s[a:].replace('pricing_availability: "unpriced".into(),','pricing_availability: if crate::official_api::kind_for_runtime(runtime).is_some() { "available" } else { "unpriced" }.into(),',1);p.write_text(s)
