use std::fmt;

use chrono::{DateTime, Datelike, Local, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::provider::{ConnectionVerificationStatus, OPENCODE_GO_BASE_URL, UpstreamProtocolKind};

pub use crate::kernel::ids::DEFAULT_ACCOUNT_TEST_MODEL;
pub use ocg_domain::account::{
    Account, AccountSetupStep, AccountType, DEFAULT_INFERENCE_BINDING_ENABLED,
    NEW_READY_KEY_ACCOUNT_ENABLED, UpstreamChannel,
};

/// Maximum persisted freeform account note length, counted in Unicode scalars.
pub const MAX_ACCOUNT_NOTES_CHARS: usize = 4000;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AccountUpdate {
    pub name: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub key: Option<String>,
    pub enabled: Option<bool>,
    pub referral_code: Option<String>,
    #[serde(alias = "recharge_date")]
    pub purchase_date: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PurchaseDateError;

impl fmt::Display for PurchaseDateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("purchase date must use the YYYY-MM-DD format")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountNotesError;

impl fmt::Display for AccountNotesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "notes must be at most {MAX_ACCOUNT_NOTES_CHARS} characters"
        )
    }
}

impl std::error::Error for AccountNotesError {}

/// Trims a freeform account note. Empty input becomes `None`; overlong input is rejected.
pub fn normalize_account_notes(value: &str) -> Result<Option<String>, AccountNotesError> {
    let trimmed = value.trim();
    if trimmed.chars().count() > MAX_ACCOUNT_NOTES_CHARS {
        return Err(AccountNotesError);
    }
    if trimmed.is_empty() {
        Ok(None)
    } else {
        Ok(Some(trimmed.to_string()))
    }
}

impl std::error::Error for PurchaseDateError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountCustomConfig {
    pub account_id: String,
    pub endpoint_url: String,
    pub upstream_protocol: UpstreamProtocolKind,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountCustomConfigInput {
    pub endpoint_url: String,
    pub upstream_protocol: UpstreamProtocolKind,
}

/// One explicitly user-triggered, non-persisting Custom API model-list probe.
/// A create form supplies `api_key`; an edit form may instead identify the
/// existing Custom account so the dashboard can use its already stored key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomModelDiscoveryInput {
    pub endpoint_url: String,
    pub upstream_protocol: UpstreamProtocolKind,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub account_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomModelDiscoveryResult {
    pub models: Vec<String>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountModelCapability {
    pub account_id: String,
    /// Client-facing Custom model identity. The persisted column remains
    /// `model_id` for migration compatibility.
    pub public_model: String,
    /// Exact model identity written to the selected upstream request.
    pub upstream_model: String,
    pub protocol: UpstreamProtocolKind,
    pub verified_at: Option<DateTime<Utc>>,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountModelCapabilityInput {
    pub public_model: String,
    pub upstream_model: String,
    pub protocol: UpstreamProtocolKind,
    #[serde(default)]
    pub source: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountVerificationState {
    pub account_id: String,
    pub status: ConnectionVerificationStatus,
    pub connection_verified_at: Option<DateTime<Utc>>,
    pub verification_error: Option<String>,
}

impl Default for AccountVerificationState {
    fn default() -> Self {
        Self {
            account_id: String::new(),
            status: ConnectionVerificationStatus::NotRequired,
            connection_verified_at: None,
            verification_error: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct AccountContractState {
    pub verification: AccountVerificationState,
    pub custom_config: Option<AccountCustomConfig>,
    pub model_capabilities: Vec<AccountModelCapability>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ForwardLogNativeAttribution {
    pub requested_model: Option<String>,
    pub resolved_alias: Option<String>,
    pub upstream_model: Option<String>,
    pub native_cost_value: Option<f64>,
    pub native_cost_unit: Option<String>,
    pub native_cost_currency: Option<String>,
}

impl ForwardLogNativeAttribution {
    pub fn inferred_from_forward_log(log: &ForwardLog) -> Self {
        let (native_cost_value, native_cost_unit, native_cost_currency) =
            Self::usd_fields_from_cost(log.raw_cost_usd, log.cost, &log.cost_state);
        Self {
            requested_model: Some(log.model.clone()),
            resolved_alias: None,
            upstream_model: Some(log.model.clone()),
            native_cost_value,
            native_cost_unit,
            native_cost_currency,
        }
    }

    /// Dual-write USD native fields from the same cost/raw_cost_usd/cost_state
    /// tuple persisted on the compatibility columns. Callers that only have a
    /// priced `cost` REAL should pass `Some(cost)` when `cost_state == "priced"`.
    pub fn usd_fields_from_cost(
        raw_cost_usd: Option<f64>,
        cost: Option<f64>,
        cost_state: &str,
    ) -> (Option<f64>, Option<String>, Option<String>) {
        let usd = raw_cost_usd.or(cost);
        let has_usd = matches!(cost_state, "priced" | "legacy_estimate" | "free") && usd.is_some();
        (
            usd,
            has_usd.then_some("usd".to_string()),
            has_usd.then_some("USD".to_string()),
        )
    }
}

/// Returns the current calendar date in the process's local timezone.
pub fn local_today() -> String {
    format_date(Local::now().date_naive())
}

/// Validates a purchase date and returns its canonical `YYYY-MM-DD` representation.
pub fn normalize_purchase_date(value: &str) -> Result<String, PurchaseDateError> {
    let parsed = NaiveDate::parse_from_str(value, "%Y-%m-%d").map_err(|_| PurchaseDateError)?;
    let normalized = format_date(parsed);
    if normalized != value {
        return Err(PurchaseDateError);
    }
    Ok(normalized)
}

/// Calculates the natural-month expiry date, clamping to the target month's last day.
pub fn purchase_expires_on(value: &str) -> Result<String, PurchaseDateError> {
    let normalized = normalize_purchase_date(value)?;
    let purchase =
        NaiveDate::parse_from_str(&normalized, "%Y-%m-%d").map_err(|_| PurchaseDateError)?;
    let (target_year, target_month) = next_month(purchase.year(), purchase.month())?;
    let (following_year, following_month) = next_month(target_year, target_month)?;
    let target_last_day = NaiveDate::from_ymd_opt(following_year, following_month, 1)
        .and_then(|date| date.pred_opt())
        .ok_or(PurchaseDateError)?
        .day();
    let expires = NaiveDate::from_ymd_opt(
        target_year,
        target_month,
        purchase.day().min(target_last_day),
    )
    .ok_or(PurchaseDateError)?;
    Ok(format_date(expires))
}

fn next_month(year: i32, month: u32) -> Result<(i32, u32), PurchaseDateError> {
    if month == 12 {
        Ok((year.checked_add(1).ok_or(PurchaseDateError)?, 1))
    } else {
        Ok((year, month + 1))
    }
}

fn format_date(date: NaiveDate) -> String {
    date.format("%Y-%m-%d").to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum RoutingMode {
    #[default]
    StrictPriority,
    StickyGlobal,
    RoundRobin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ProxyMode {
    /// Use the platform/environment proxy configuration when available.
    #[default]
    Auto,
    /// Route every supported outbound HTTP(S) request through one explicit proxy.
    Manual,
    /// Ignore platform/environment proxy configuration and connect directly.
    Direct,
    /// Route per model against `proxy_list_models`: listed models use the
    /// direction's exception leg, everything else (including non-model-scoped
    /// outbound traffic) uses the direction's default leg.
    List,
}

/// Which leg the listed models take in list proxy mode. The other leg is the
/// direction's default for unlisted models and non-model-scoped traffic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ProxyListDirection {
    /// Listed models go through `proxy_url`; everything else connects directly.
    #[default]
    Whitelist,
    /// Listed models connect directly; everything else goes through `proxy_url`.
    Blacklist,
}

pub const DEFAULT_OPENCODE_INVITE_URL: &str = "https://opencode.ai/go?ref=68XPB6NP8V";

/// Shared rejection message for a blank primary gateway key; used by
/// `AppConfig::validate` and both settings-update entry points.
pub const PRIMARY_KEY_REQUIRED_MESSAGE: &str = "key is required";

/// Sentinel filter value selecting forward logs without a client key
/// (written before multi-key support or not yet backfilled).
pub const UNATTRIBUTED_KEY_FILTER: &str = "__unattributed__";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub gateway_port: u16,
    pub gateway_key: String,
    /// Persisted for compatibility. Production OpenCode Go/Zen routes use the
    /// sealed official origins; only loopback HTTP(S) values remain a test seam.
    pub upstream_base_url: String,
    pub proxy_mode: ProxyMode,
    pub proxy_url: String,
    #[serde(default)]
    pub proxy_list_direction: ProxyListDirection,
    #[serde(default)]
    pub proxy_list_models: Vec<String>,
    pub opencode_invite_url: String,
    pub client_root_url: String,
    pub auto_start: bool,
    pub show_dock_icon: bool,
    pub connect_timeout_secs: u64,
    pub non_stream_timeout_secs: u64,
    pub stream_idle_timeout_secs: u64,
    pub routing_mode: RoutingMode,
    pub conversation_sticky: bool,
    pub claude_desktop_models: ClaudeDesktopModels,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            gateway_port: 9042,
            gateway_key: String::new(),
            upstream_base_url: OPENCODE_GO_BASE_URL.to_string(),
            proxy_mode: ProxyMode::Auto,
            proxy_url: String::new(),
            proxy_list_direction: ProxyListDirection::Whitelist,
            proxy_list_models: Vec::new(),
            opencode_invite_url: DEFAULT_OPENCODE_INVITE_URL.to_string(),
            client_root_url: String::new(),
            auto_start: false,
            show_dock_icon: true,
            connect_timeout_secs: 30,
            non_stream_timeout_secs: 900,
            stream_idle_timeout_secs: 300,
            routing_mode: RoutingMode::StrictPriority,
            conversation_sticky: false,
            claude_desktop_models: ClaudeDesktopModels::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaudeDesktopModels {
    pub sonnet: String,
    pub opus: String,
    pub haiku: String,
}

impl Default for ClaudeDesktopModels {
    fn default() -> Self {
        Self {
            sonnet: "minimax-m3".to_string(),
            opus: String::new(),
            haiku: String::new(),
        }
    }
}

impl ClaudeDesktopModels {
    pub fn normalize(&mut self) {
        self.sonnet = self.sonnet.trim().to_string();
        self.opus = self.opus.trim().to_string();
        self.haiku = self.haiku.trim().to_string();
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.first_configured().is_none() {
            return Err("at least one Claude Desktop model is required".to_string());
        }
        for (role, model) in [
            ("sonnet", self.sonnet.as_str()),
            ("opus", self.opus.as_str()),
            ("haiku", self.haiku.as_str()),
        ] {
            if model.is_empty() {
                continue;
            }
            if crate::kernel::ids::is_free_model(model) {
                return Err(format!(
                    "Claude Desktop {role} model `{model}` cannot be a Zen free model"
                ));
            }
            if !crate::kernel::protocol::supported_model_ids().any(|supported| supported == model) {
                return Err(format!("unsupported Claude Desktop {role} model `{model}`"));
            }
        }
        Ok(())
    }

    pub fn resolved(&self) -> Self {
        let fallback = self.first_configured().unwrap_or_default();
        Self {
            sonnet: if self.sonnet.is_empty() {
                fallback.to_string()
            } else {
                self.sonnet.clone()
            },
            opus: if self.opus.is_empty() {
                fallback.to_string()
            } else {
                self.opus.clone()
            },
            haiku: if self.haiku.is_empty() {
                fallback.to_string()
            } else {
                self.haiku.clone()
            },
        }
    }

    pub(crate) fn model_for_alias(&self, alias: &str) -> Option<&str> {
        let configured = match alias {
            CLAUDE_DESKTOP_SONNET_ALIAS => self.sonnet.as_str(),
            CLAUDE_DESKTOP_OPUS_ALIAS => self.opus.as_str(),
            CLAUDE_DESKTOP_HAIKU_ALIAS => self.haiku.as_str(),
            _ => return None,
        };
        (!configured.is_empty())
            .then_some(configured)
            .or_else(|| self.first_configured())
    }

    fn first_configured(&self) -> Option<&str> {
        [
            self.sonnet.as_str(),
            self.opus.as_str(),
            self.haiku.as_str(),
        ]
        .into_iter()
        .find(|model| !model.is_empty())
    }
}

pub const CLAUDE_DESKTOP_SONNET_ALIAS: &str = "claude-sonnet-4-6";
pub const CLAUDE_DESKTOP_OPUS_ALIAS: &str = "claude-opus-4-6";
pub const CLAUDE_DESKTOP_HAIKU_ALIAS: &str = "claude-haiku-4-5-20251001";

/// Validates and canonicalizes the optional URL shown to downstream clients.
pub fn normalize_client_root_url(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(String::new());
    }
    let lower = value.to_ascii_lowercase();
    if !lower.starts_with("http://") && !lower.starts_with("https://") {
        return Err("client root URL must be an absolute http:// or https:// URL".to_string());
    }

    let mut url =
        reqwest::Url::parse(value).map_err(|error| format!("invalid client root URL: {error}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("client root URL must use http or https".to_string());
    }
    if url.host_str().is_none() {
        return Err("client root URL must include a host".to_string());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("client root URL must not include credentials".to_string());
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("client root URL must not include a query or fragment".to_string());
    }

    let mut path = url.path().trim_end_matches('/').to_string();
    let segments = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    if let Some(index) = segments
        .iter()
        .position(|segment| segment.eq_ignore_ascii_case("v1"))
    {
        if index + 1 != segments.len() {
            return Err("client root URL must not include an endpoint after /v1".to_string());
        }
        path.truncate(path.len() - "/v1".len());
        path.truncate(path.trim_end_matches('/').len());
    }

    url.set_path(if path.is_empty() { "/" } else { &path });
    Ok(url.as_str().trim_end_matches('/').to_string())
}

impl AppConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.gateway_key.trim().is_empty() {
            return Err(PRIMARY_KEY_REQUIRED_MESSAGE.to_string());
        }
        self.validate_timeouts()?;
        normalize_proxy_url(self.proxy_mode, &self.proxy_url)?;
        normalize_opencode_invite_url(&self.opencode_invite_url)?;
        // routing_mode is validated by serde enum decoding; unknown values never reach here.
        self.claude_desktop_models.validate()
    }

    pub fn validate_timeouts(&self) -> Result<(), String> {
        for (name, value, max) in [
            ("connect_timeout_secs", self.connect_timeout_secs, 300),
            (
                "non_stream_timeout_secs",
                self.non_stream_timeout_secs,
                3600,
            ),
            (
                "stream_idle_timeout_secs",
                self.stream_idle_timeout_secs,
                3600,
            ),
        ] {
            if !(1..=max).contains(&value) {
                return Err(format!("{name} must be between 1 and {max}"));
            }
        }
        Ok(())
    }
}

/// Validates and canonicalizes the optional global outbound HTTP proxy URL.
///
/// Manual and list modes both require a usable URL (the list legs route
/// through it); unused leftover values must not block Auto/Direct saves.
pub fn normalize_proxy_url(mode: ProxyMode, value: &str) -> Result<String, String> {
    let value = value.trim();
    let url_required = matches!(mode, ProxyMode::Manual | ProxyMode::List);
    if value.is_empty() {
        return if url_required {
            Err(match mode {
                ProxyMode::List => "list proxy mode requires a proxy URL".to_string(),
                _ => "manual proxy mode requires a proxy URL".to_string(),
            })
        } else {
            Ok(String::new())
        };
    }

    match canonicalize_proxy_url(value) {
        Ok(normalized) => Ok(normalized),
        Err(error) if url_required => Err(error),
        // Unused leftover values must not block Auto/Direct saves.
        Err(_) => Ok(value.to_string()),
    }
}

fn canonicalize_proxy_url(value: &str) -> Result<String, String> {
    if value.len() > 2048 {
        return Err("proxy URL is too long".to_string());
    }

    let parsed =
        reqwest::Url::parse(value).map_err(|error| format!("invalid proxy URL: {error}"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("proxy URL must use http or https".to_string());
    }
    if parsed.host_str().is_none() {
        return Err("proxy URL must include a host".to_string());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("proxy URL must not include credentials".to_string());
    }
    if !matches!(parsed.path(), "" | "/") || parsed.query().is_some() || parsed.fragment().is_some()
    {
        return Err("proxy URL must not include a path, query, or fragment".to_string());
    }

    Ok(parsed.as_str().trim_end_matches('/').to_string())
}

pub fn normalize_opencode_invite_url(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(String::new());
    }
    if value.len() > 2048 {
        return Err("OpenCode invite URL is too long".to_string());
    }
    let parsed = reqwest::Url::parse(value)
        .map_err(|error| format!("invalid OpenCode invite URL: {error}"))?;
    if parsed.scheme() != "https" {
        return Err("OpenCode invite URL must use https".to_string());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("OpenCode invite URL must not contain credentials".to_string());
    }
    match parsed.host_str() {
        Some("opencode.ai" | "console.opencode.ai") => {}
        _ => {
            return Err(
                "OpenCode invite URL host must be opencode.ai or console.opencode.ai".to_string(),
            );
        }
    }
    Ok(parsed.to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayLog {
    pub id: i64,
    pub level: String,
    pub category: String,
    pub message: String,
    pub created_at: DateTime<Utc>,
    pub request_id: Option<String>,
    pub attempt: Option<i64>,
    pub error_source: Option<String>,
    pub error_stage: Option<String>,
    pub duration_ms: Option<i64>,
    pub diagnostic: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForwardLog {
    pub id: i64,
    pub timestamp: DateTime<Utc>,
    pub model: String,
    pub account_id: String,
    pub account_name: String,
    #[serde(default)]
    pub route_account_id: Option<String>,
    #[serde(default)]
    pub provider_id: Option<String>,
    #[serde(default)]
    pub credential_account_id: Option<String>,
    #[serde(default)]
    pub client_key_id: Option<String>,
    #[serde(default)]
    pub client_key_name: Option<String>,
    pub status: String,
    pub http_status: Option<i32>,
    /// Route leg label for this attempt: `auto`, `proxy`, or `direct`.
    /// Empty for rows written before the column existed ("not recorded").
    #[serde(default)]
    pub route: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cached_tokens: i64,
    pub cache_creation_tokens: i64,
    pub cost: Option<f64>,
    #[serde(default)]
    pub raw_cost_usd: Option<f64>,
    #[serde(default)]
    pub quota_debit: Option<f64>,
    #[serde(default)]
    pub effective_paid_cost_usd: Option<f64>,
    pub pricing_revision_id: Option<String>,
    pub quota_multiplier: Option<f64>,
    pub local_adjustment_multiplier: Option<f64>,
    pub service_tier: Option<String>,
    pub cost_state: String,
    pub error_message: Option<String>,
    pub request_id: Option<String>,
    pub attempt: Option<i64>,
    pub error_source: Option<String>,
    pub error_stage: Option<String>,
    pub duration_ms: Option<i64>,
    pub diagnostic: Option<Value>,
}

#[derive(Debug, Clone)]
pub struct ForwardMetrics {
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cached_tokens: i64,
    pub cache_creation_tokens: i64,
    pub cost: f64,
    pub raw_cost_usd: Option<f64>,
    pub quota_debit: Option<f64>,
    pub effective_paid_cost_usd: Option<f64>,
    pub pricing_revision_id: Option<String>,
    pub quota_multiplier: Option<f64>,
    pub local_adjustment_multiplier: Option<f64>,
    /// Ephemeral source identity used to prove that token-derived pricing
    /// belongs to the selected provider before it is persisted.
    pub pricing_provider_id: Option<String>,
    pub service_tier: Option<String>,
    pub cost_state: &'static str,
}

impl Default for ForwardMetrics {
    fn default() -> Self {
        Self {
            prompt_tokens: 0,
            completion_tokens: 0,
            cached_tokens: 0,
            cache_creation_tokens: 0,
            cost: 0.0,
            raw_cost_usd: None,
            quota_debit: None,
            effective_paid_cost_usd: None,
            pricing_revision_id: None,
            quota_multiplier: None,
            local_adjustment_multiplier: None,
            pricing_provider_id: None,
            service_tier: None,
            cost_state: "not_applicable",
        }
    }
}

impl ForwardMetrics {
    /// Constrain generic token-derived metrics to the selected provider's
    /// verified pricing contract. Legacy rows without provider attribution are
    /// intentionally left alone, while an explicitly attributed provider can
    /// never inherit OpenCode Go pricing by accident.
    pub(crate) fn scope_to_provider(&mut self, provider_id: Option<&str>, successful: bool) {
        let Some(provider_id) = provider_id else {
            return;
        };
        if self.pricing_provider_id.as_deref() == Some(provider_id) {
            return;
        }

        let has_cost_outcome = matches!(self.cost_state, "priced" | "unpriced" | "free");
        self.cost = 0.0;
        self.pricing_revision_id = None;
        self.quota_multiplier = None;
        self.local_adjustment_multiplier = None;
        self.pricing_provider_id = None;

        if provider_id == crate::kernel::ids::OPENCODE_ZEN_FREE_PROVIDER_ID
            && (successful || has_cost_outcome)
        {
            self.raw_cost_usd = Some(0.0);
            self.quota_debit = Some(0.0);
            self.effective_paid_cost_usd = Some(0.0);
            self.cost_state = "free";
        } else if crate::provider::is_custom_api(provider_id) {
            self.raw_cost_usd = None;
            self.quota_debit = None;
            self.effective_paid_cost_usd = None;
            if successful || has_cost_outcome {
                self.cost_state = "unknown";
            }
        } else {
            self.raw_cost_usd = None;
            self.quota_debit = None;
            self.effective_paid_cost_usd = None;
            if has_cost_outcome {
                self.cost_state = "unpriced";
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForwardLogSummary {
    pub total_requests: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cached_tokens: i64,
    pub cost: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForwardLogPage {
    pub items: Vec<ForwardLog>,
    pub summary: ForwardLogSummary,
}

/// One distinct client key observed in forward logs (see
/// `Database::list_forward_log_keys`). Covers enabled, disabled, and
/// soft-deleted keys plus dangling ids left by a downgrade.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForwardLogClientKey {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageWindow {
    pub account_id: String,
    pub window_5h: f64,
    pub window_week: f64,
    pub window_month: f64,
    /// 当 5h 固定窗口仍有效时，表示该窗口的清零时刻；`None` 表示窗口尚未开始（无成功请求）。
    #[serde(default)]
    pub resets_in_5h: Option<DateTime<Utc>>,
    /// 当周固定窗口的清零时刻；`None` 表示窗口尚未开始。
    #[serde(default)]
    pub resets_in_week: Option<DateTime<Utc>>,
    /// 月窗口的到期时刻，固定为 `purchase_expires_on(purchase_date) 00:00`；`None` 表示账号无购买日期。
    #[serde(default)]
    pub resets_in_month: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageWindowKind {
    FiveHours,
    Week,
    Month,
    /// Zen free-model promo quota (independent of Go usage windows).
    Free,
}

/// Persistence-shaped quota window record synced from official usage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuotaWindow {
    pub account_id: String,
    pub window_kind: String,
    pub used: f64,
    pub limit_value: Option<f64>,
    pub started_at: Option<DateTime<Utc>>,
    pub resets_at: Option<DateTime<Utc>>,
    pub calibration_offset: f64,
    pub unit: String,
    pub source: String,
    pub observed_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

/// Persistence-shaped credit balance record synced from official usage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreditBalance {
    pub account_id: String,
    pub balance_kind: String,
    pub amount: f64,
    pub unit: String,
    pub source: String,
    pub observed_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

/// Persistence-shaped adaptive usage-sync state for one account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderUsageSyncState {
    pub account_id: String,
    pub last_success_at: Option<DateTime<Utc>>,
    pub last_attempt_at: Option<DateTime<Utc>>,
    pub next_eligible_at: Option<DateTime<Utc>>,
    pub failure_streak: i64,
    pub last_expedited_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayStatus {
    pub running: bool,
    pub port: u16,
    /// Primary key value; kept for legacy consumers.
    pub key: String,
    pub upstream_base_url: String,
    pub last_error: Option<String>,
}

/// One database-owned non-primary access key (schema v27 `access_keys`).
/// `key` holds the plaintext value and is cleared on soft delete so deleted
/// credentials never resurface in management APIs while the record stays
/// resolvable for log attribution. The live primary row is not represented
/// here; public `AppConfig.gateway_key` remains the API-facing primary value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubGatewayKey {
    pub id: String,
    pub name: String,
    pub key: String,
    pub enabled: bool,
    pub deleted_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl SubGatewayKey {
    pub fn is_active(&self) -> bool {
        self.deleted_at.is_none()
    }

    pub fn authenticates(&self) -> bool {
        self.enabled && self.is_active() && !self.key.is_empty()
    }
}

/// A sub key as exposed by the lightweight connection endpoint. Plaintext is
/// behind the dashboard session layer, same as the primary key value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionSubKey {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub value: String,
}

/// Aggregated connection view for the dashboard connection center: primary
/// key value, non-deleted sub keys with values, settings revision, and the
/// fields needed to derive client-facing URLs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionInfo {
    pub gateway_port: u16,
    pub client_root_url: String,
    pub upstream_base_url: String,
    pub primary_key: String,
    pub sub_keys: Vec<ConnectionSubKey>,
    pub revision: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardSummary {
    pub total_accounts: usize,
    pub available_accounts: usize,
    pub gateway_running: bool,
    pub today_cost: f64,
    pub week_cost: f64,
    pub month_cost: f64,
}

/// One row of "daily tokens per model" aggregation for the dashboard chart.
/// `date` is `YYYY-MM-DD` (UTC). `tokens` is `prompt_tokens + completion_tokens`
/// summed over the day; cached reads are already included in the input token
/// counts recorded by the gateway, so they are not added again.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyModelTokens {
    pub date: String,
    pub model: String,
    pub tokens: i64,
}

#[cfg(test)]
mod tests;
