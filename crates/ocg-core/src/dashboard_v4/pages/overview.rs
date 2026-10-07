//! Dashboard business facts from local state; this read never contacts an upstream.
use super::super::types::{CredentialCooldownsDto, PlanWindowKindDto};
use super::*;
use chrono::NaiveDate;
use ocg_domain::credential::AuthState;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const CHART_DAYS: i64 = 30;
const ATTENTION_LIMIT: usize = 50;

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DashboardPageQuery {
    /// Browser calendar offset east of UTC. Token buckets remain UTC.
    pub utc_offset_minutes: Option<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum DashboardAttentionReason {
    AuthError,
    Expired,
    Cooling,
    SetupIncomplete,
}
impl DashboardAttentionReason {
    fn priority(self) -> u8 {
        match self {
            Self::AuthError => 0,
            Self::Expired => 1,
            Self::Cooling => 2,
            Self::SetupIncomplete => 3,
        }
    }
}

macro_rules! dto {
    ($(pub struct $name:ident { $($body:tt)* })*) => {$(
        #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
        #[serde(rename_all = "camelCase")]
        #[schemars(rename_all = "camelCase", deny_unknown_fields)]
        pub struct $name { $($body)* }
    )*};
}
dto! {
pub struct DashboardPageSummary {
    pub total_accounts: u64, pub available_accounts: u64, pub gateway_running: bool,
    pub today_cost: Option<f64>, pub week_cost: Option<f64>, pub month_cost: Option<f64>
}
pub struct DashboardAttentionItem {
    pub account_id: String, pub account_name: String, pub reason: DashboardAttentionReason,
    pub expired_days: Option<i64>
}
pub struct DashboardTokenRow { pub date: String, pub model: String, pub tokens: i64 }
pub struct DashboardModelTotal { pub model: String, pub tokens: i64 }
pub struct DashboardChartDay { pub date: String, pub total_tokens: i64, pub models: Vec<DashboardModelTotal> }
pub struct DashboardPage {
    pub revision: ControlRevision, pub read_version: String, pub as_of: String, pub valid_until: String,
    pub summary: DashboardPageSummary, pub attention_items: Vec<DashboardAttentionItem>,
    pub attention_total: u32, pub attention_limit: u32,
    pub model_totals: Vec<DashboardModelTotal>, pub total_tokens: i64, pub daily_average_tokens: i64,
    pub chart_days: u32, pub chart_series: Vec<DashboardChartDay>, pub errors: Vec<PageReadIssue>
}
}

struct AttentionFacts<'a> {
    ready: bool,
    enabled: bool,
    auth_error: bool,
    has_expiry: bool,
    expires_on: &'a str,
    free_only: bool,
    cooldowns: &'a CredentialCooldownsDto,
}

fn cooldown_deadline(facts: &AttentionFacts<'_>, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let c = facts.cooldowns;
    let windows = if facts.free_only {
        vec![&c.free_until]
    } else {
        vec![
            &c.generic_until,
            &c.five_hour_until,
            &c.week_until,
            &c.month_until,
            &c.free_until,
        ]
    };
    windows
        .into_iter()
        .filter_map(|at| at.as_deref())
        .filter_map(|at| DateTime::parse_from_rfc3339(at).ok())
        .map(|at| at.with_timezone(&Utc))
        .filter(|at| *at > now)
        .max()
}

fn attention_reason(
    facts: &AttentionFacts<'_>,
    now: DateTime<Utc>,
    offset: i32,
) -> Option<(DashboardAttentionReason, Option<i64>)> {
    // Onboarding needs action even when its not-yet-ready account is disabled.
    if !facts.ready {
        return Some((DashboardAttentionReason::SetupIncomplete, None));
    }
    if !facts.enabled {
        return None;
    }
    if facts.auth_error {
        return Some((DashboardAttentionReason::AuthError, None));
    }
    if facts.has_expiry
        && let Ok(date) = NaiveDate::parse_from_str(facts.expires_on, "%Y-%m-%d")
    {
        let today = (now + Duration::minutes(i64::from(offset))).date_naive();
        let days = today.signed_duration_since(date).num_days();
        if date.to_string() == facts.expires_on && days > 0 {
            return Some((DashboardAttentionReason::Expired, Some(days)));
        }
    }
    cooldown_deadline(facts, now).map(|_| (DashboardAttentionReason::Cooling, None))
}

fn attention(
    s: &PageSnapshot,
    now: DateTime<Utc>,
    offset: i32,
) -> (Vec<DashboardAttentionItem>, u32, DateTime<Utc>) {
    let mut valid_until = observation_deadline(now, offset);
    let mut items = Vec::new();
    for account in &s.accounts {
        let credential = s
            .credentials
            .iter()
            .find(|c| c.legacy_account_id == account.id);
        let destination =
            credential.and_then(|c| s.destinations.iter().find(|d| d.id == c.destination_id));
        let fallback = CredentialCooldownsDto {
            generic_until: account
                .cooldown_generic_until
                .clone()
                .or_else(|| account.cooldown_until.clone()),
            five_hour_until: account.cooldown_5h_until.clone(),
            week_until: account.cooldown_week_until.clone(),
            month_until: account.cooldown_month_until.clone(),
            free_until: account.cooldown_free_until.clone(),
        };
        let plan = destination.and_then(|d| d.plan.as_ref());
        let facts = AttentionFacts {
            ready: account.setup_step == dashboard_v3::AccountSetupStep::Ready,
            enabled: credential.map_or(account.enabled, |c| c.enabled),
            auth_error: account
                .auth_error
                .as_deref()
                .is_some_and(|error| !error.is_empty())
                || credential.is_some_and(|c| c.auth_state == AuthState::Invalid),
            has_expiry: plan.is_some_and(|p| p.expiry_cadence.is_some()),
            expires_on: &account.expires_on,
            free_only: plan.is_some_and(|p| {
                !p.windows.is_empty() && p.windows.iter().all(|w| w.kind == PlanWindowKindDto::Free)
            }),
            cooldowns: credential.map_or(&fallback, |c| &c.cooldowns),
        };
        if let Some(deadline) = cooldown_deadline(&facts, now) {
            valid_until = valid_until.min(deadline);
        }
        if let Some((reason, expired_days)) = attention_reason(&facts, now, offset) {
            items.push(DashboardAttentionItem {
                account_id: account.id.clone(),
                account_name: account.name.clone(),
                reason,
                expired_days,
            });
        }
    }
    let (items, total) = bound_attention(items);
    (items, total, valid_until)
}

fn observation_deadline(now: DateTime<Utc>, offset: i32) -> DateTime<Utc> {
    let local_midnight = (now + Duration::minutes(i64::from(offset)))
        .date_naive()
        .succ_opt()
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        - Duration::minutes(i64::from(offset));
    let utc_midnight = now
        .date_naive()
        .succ_opt()
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc();
    (now + Duration::seconds(15))
        .min(local_midnight)
        .min(utc_midnight)
}

fn bound_attention(mut items: Vec<DashboardAttentionItem>) -> (Vec<DashboardAttentionItem>, u32) {
    items.sort_by(|a, b| {
        a.reason
            .priority()
            .cmp(&b.reason.priority())
            .then_with(|| {
                a.account_name
                    .to_lowercase()
                    .cmp(&b.account_name.to_lowercase())
            })
            .then_with(|| a.account_id.cmp(&b.account_id))
    });
    let total = count(items.len());
    items.truncate(ATTENTION_LIMIT);
    (items, total)
}

fn token_totals(rows: &[DashboardTokenRow]) -> (Vec<DashboardModelTotal>, i64, i64) {
    let mut totals: BTreeMap<&str, i64> = BTreeMap::new();
    let mut total: i64 = 0;
    for row in rows {
        total = total.saturating_add(row.tokens);
        let sum = totals.entry(&row.model).or_default();
        *sum = sum.saturating_add(row.tokens);
    }
    let mut models: Vec<_> = totals
        .into_iter()
        .map(|(model, tokens)| DashboardModelTotal {
            model: model.into(),
            tokens,
        })
        .collect();
    models.sort_by(|a, b| b.tokens.cmp(&a.tokens).then_with(|| a.model.cmp(&b.model)));
    (
        models,
        total,
        (total as f64 / CHART_DAYS as f64).round() as i64,
    )
}

fn chart_series(rows: &[DashboardTokenRow], now: DateTime<Utc>) -> Vec<DashboardChartDay> {
    let today = now.date_naive();
    let mut by_date: BTreeMap<&str, Vec<DashboardTokenRow>> = BTreeMap::new();
    for row in rows {
        by_date.entry(&row.date).or_default().push(row.clone());
    }
    (0..CHART_DAYS)
        .rev()
        .map(|age| {
            let date = (today - Duration::days(age)).to_string();
            let (models, total_tokens, _) =
                token_totals(by_date.get(date.as_str()).map_or(&[], Vec::as_slice));
            DashboardChartDay {
                date,
                total_tokens,
                models,
            }
        })
        .collect()
}

pub(crate) async fn get(
    State(state): State<CoreState>,
    Query(query): Query<DashboardPageQuery>,
) -> Result<Json<DashboardPage>, V3ApiError> {
    let offset = query.utc_offset_minutes.unwrap_or(0);
    if !(-840..=840).contains(&offset) {
        return Err(V3ApiError::invalid_request_at(
            &state,
            "utcOffsetMinutes must be between -840 and 840",
        ));
    }
    tokio::task::spawn_blocking(move || project(&state, offset))
        .await
        .map_err(V3ApiError::internal)?
}

fn project(state: &CoreState, offset: i32) -> Result<Json<DashboardPage>, V3ApiError> {
    for _ in 0..3 {
        let s = snapshot(state)?;
        let _settings = state.settings_update.lock();
        let before = cache::ReadVersion::capture(state)?;
        if before.token() != s.read_version {
            continue;
        }
        let now = state.sample_gateway_clock().0;
        let (summary, daily_tokens) = {
            let db = state.db.lock();
            let summary = crate::control::observability::dashboard_summary(
                &db,
                state.gateway.lock().is_some(),
                |cipher| state.decrypt_key(cipher).ok(),
            )
            .map_err(V3ApiError::internal)?;
            let rows = crate::control::observability::daily_tokens_by_model(&db, Some(CHART_DAYS))
                .map_err(V3ApiError::internal)?;
            (
                summary,
                rows.into_iter()
                    .map(|r| DashboardTokenRow {
                        date: r.date,
                        model: r.model,
                        tokens: r.tokens,
                    })
                    .collect::<Vec<_>>(),
            )
        };
        if before != cache::ReadVersion::capture(state)? {
            continue;
        }
        let (attention_items, attention_total, valid_until) = attention(&s, now, offset);
        let (model_totals, total_tokens, daily_average_tokens) = token_totals(&daily_tokens);
        let chart_series = chart_series(&daily_tokens, now);
        return Ok(Json(DashboardPage {
            revision: s.revision.clone(),
            read_version: s.read_version.clone(),
            as_of: now.to_rfc3339(),
            valid_until: valid_until.to_rfc3339(),
            summary: DashboardPageSummary {
                total_accounts: summary.total_accounts as u64,
                available_accounts: summary.available_accounts as u64,
                gateway_running: summary.gateway_running,
                today_cost: summary.today_cost,
                week_cost: summary.week_cost,
                month_cost: summary.month_cost,
            },
            attention_items,
            attention_total,
            attention_limit: ATTENTION_LIMIT as u32,
            model_totals,
            total_tokens,
            daily_average_tokens,
            chart_days: CHART_DAYS as u32,
            chart_series,
            errors: s.errors.clone(),
        }));
    }
    Err(V3ApiError::conflict_at(
        state,
        "dashboard snapshot changed during read",
    ))
}

#[cfg(test)]
mod tests;
