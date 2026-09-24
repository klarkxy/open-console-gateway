//! Pure per-credential credit meter. Persistence, HTTP, and settlement live elsewhere.

use crate::billing_types::{
    CreditBalanceCorrection, CreditBucket, CreditBucketKind, CreditConfiguration, CreditMeterView,
    CreditPreset, CreditRate, MonthlyCredits,
};
use anyhow::{Result, anyhow, bail, ensure};
use chrono::{DateTime, Datelike, FixedOffset, NaiveDate, TimeZone, Utc};
use ocg_domain::billing::{BillingModel, BillingTokens, TokenRates, convert_charge, token_charge};
use ocg_domain::destination::AdapterKind;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::HashSet;

const MAX_AMOUNT: f64 = 1e15;
const MAX_BUCKETS: usize = 64;
const MAX_EXPIRED_BUCKETS: usize = 32;
const MAX_RATES: usize = 64;
const MAX_CORRECTIONS: usize = 64;
const MAX_ID_LEN: usize = 128;
const MAX_LABEL_LEN: usize = 128;
const MAX_URL_LEN: usize = 2048;
const CHINA_OFFSET_MINUTES: i32 = 480;

/// Persisted local estimate for one credential. One Key is one account.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreditMeterState {
    pub meter_id: String,
    pub credential_id: String,
    pub destination_id: String,
    pub endpoint: String,
    pub configuration: CreditConfiguration,
    pub buckets: Vec<CreditBucket>,
    pub spent_since_calibration: f64,
    pub overdrawn: f64,
    pub unpriced_requests: u64,
    pub last_calibration_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    /// Last monthly cycle start that already received a lazy grant.
    #[serde(default)]
    pub monthly_cursor: Option<DateTime<Utc>>,
}

/// Frozen attempt snapshot. Root stores this exact type; it carries no balances.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreditAttempt {
    pub credential_id: String,
    pub meter_id: String,
    pub destination_id: String,
    pub endpoint: String,
    pub account_id: String,
    pub model: String,
    pub currency: String,
    pub credits_per_currency: f64,
    pub rate: Option<CreditRate>,
    pub at: DateTime<Utc>,
}

pub(crate) fn billing_model_for_adapter(kind: AdapterKind) -> BillingModel {
    match kind {
        AdapterKind::OpencodeGo
        | AdapterKind::Goat
        | AdapterKind::Minimax
        | AdapterKind::Kimi
        | AdapterKind::Zen => BillingModel::Quota,
        AdapterKind::Ollama => BillingModel::Credits,
        AdapterKind::Http | AdapterKind::Cpa => BillingModel::Cash,
    }
}

/// Exact HTTPS `:443` `api.stepfun.com/step_plan` (and `/step_plan/…`) only.
/// Ordinary `/v1` API paths, `/step_planet`, and `/step_planning` are false.
pub(crate) fn is_stepfun_plan_endpoint(endpoint: &str) -> bool {
    crate::official_service::identify(endpoint)
        == Some(crate::official_service::OfficialService::StepFunPlan)
}

pub(crate) fn billing_model_for_destination(kind: AdapterKind, endpoint: &str) -> BillingModel {
    if kind == AdapterKind::Http {
        crate::official_service::identify(endpoint)
            .map_or(BillingModel::Cash, |service| service.billing_model())
    } else {
        billing_model_for_adapter(kind)
    }
}

pub(crate) fn stepfun_plan_credits(endpoint: &str, at: DateTime<Utc>) -> Option<Vec<CreditPreset>> {
    is_stepfun_plan_endpoint(endpoint).then(|| stepfun_credit_presets(at))
}

pub(crate) fn stepfun_credit_presets(at: DateTime<Utc>) -> Vec<CreditPreset> {
    let next_reset = match next_month_start(at, CHINA_OFFSET_MINUTES) {
        Ok(reset) => reset,
        Err(_) => at,
    };
    let preset = &*STEPFUN_PRESETS;
    preset
        .tiers
        .iter()
        .map(|tier| CreditPreset {
            id: tier.id.clone(),
            configuration: CreditConfiguration {
                name: tier.name.clone(),
                currency: preset.currency.clone(),
                credits_per_currency: preset.credits_per_currency,
                rates: preset.rates.clone(),
                monthly: Some(MonthlyCredits {
                    amount: tier.amount,
                    next_reset_at: next_reset,
                    timezone_offset_minutes: CHINA_OFFSET_MINUTES,
                    renewal_ends_at: None,
                }),
                source_url: Some(preset.source_url.clone()),
            },
            initial_grant: tier.amount,
        })
        .collect()
}

impl CreditMeterState {
    pub(crate) fn new(
        meter_id: String,
        credential_id: String,
        destination_id: String,
        endpoint: String,
        configuration: CreditConfiguration,
        initial_buckets: Vec<CreditBucket>,
        created_at: DateTime<Utc>,
    ) -> Result<Self> {
        let mut state = Self {
            meter_id,
            credential_id,
            destination_id,
            endpoint,
            configuration,
            buckets: initial_buckets,
            spent_since_calibration: 0.0,
            overdrawn: 0.0,
            unpriced_requests: 0,
            last_calibration_at: Some(created_at),
            created_at,
            monthly_cursor: None,
        };
        if let Some(monthly) = state.configuration.monthly.as_ref() {
            let next = match current_cycle_index(monthly, created_at)? {
                Some(index) => monthly_occurrence(monthly, index + 1)?,
                None => monthly.next_reset_at,
            };
            for bucket in &mut state.buckets {
                if bucket.kind == CreditBucketKind::Monthly && bucket_active(bucket, created_at) {
                    bucket.expires_at =
                        Some(bucket.expires_at.map_or(next, |expiry| expiry.min(next)));
                }
            }
        }
        state.validate_monthly_capacity(created_at)?;
        state.validate()?;
        Ok(state)
    }

    pub(crate) fn validate(&self) -> Result<()> {
        validate_id(&self.meter_id, "meter id")?;
        validate_id(&self.credential_id, "credential id")?;
        validate_id(&self.destination_id, "destination id")?;
        validate_endpoint(&self.endpoint)?;
        validate_configuration(&self.configuration)?;
        ensure!(self.buckets.len() <= MAX_BUCKETS, "too many credit buckets");
        let mut seen = HashSet::new();
        for bucket in &self.buckets {
            validate_bucket(bucket)?;
            ensure!(seen.insert(bucket.id.as_str()), "duplicate bucket id");
        }
        ensure!(
            self.spent_since_calibration.is_finite() && self.spent_since_calibration >= 0.0,
            "invalid spent since calibration"
        );
        ensure!(
            self.overdrawn.is_finite() && self.overdrawn >= 0.0,
            "invalid overdrawn"
        );
        Ok(())
    }

    pub(crate) fn project(&self, now: DateTime<Utc>, pending_requests: u64) -> CreditMeterView {
        let buckets: Vec<CreditBucket> = self
            .buckets
            .iter()
            .filter(|bucket| bucket_active(bucket, now))
            .cloned()
            .collect();
        let remaining = buckets.iter().map(|bucket| bucket.remaining).sum();
        let active_granted = buckets.iter().map(|bucket| bucket.granted).sum();
        CreditMeterView {
            credential_id: self.credential_id.clone(),
            meter_id: self.meter_id.clone(),
            configuration: self.configuration.clone(),
            buckets,
            remaining,
            active_granted,
            spent_since_calibration: self.spent_since_calibration,
            overdrawn: self.overdrawn,
            unpriced_requests: self.unpriced_requests,
            pending_requests,
            last_calibration_at: self.last_calibration_at,
            estimated_at: now,
            next_reset_at: self.configuration.monthly.as_ref().and_then(|monthly| {
                let next = match current_cycle_index(monthly, now).ok()? {
                    Some(index) => monthly_occurrence(monthly, index + 1).ok()?,
                    None => monthly.next_reset_at,
                };
                monthly
                    .renewal_ends_at
                    .is_none_or(|end| next < end)
                    .then_some(next)
            }),
        }
    }

    pub(crate) fn advance(&mut self, now: DateTime<Utc>) -> Result<()> {
        if let Some(monthly) = self.configuration.monthly.clone()
            && let Some(index) = current_cycle_index(&monthly, now)?
        {
            let start = monthly_occurrence(&monthly, index)?;
            if self.monthly_cursor != Some(start) {
                for bucket in &mut self.buckets {
                    if bucket.kind == CreditBucketKind::Monthly
                        && bucket.starts_at < start
                        && !bucket_expired(bucket, now)
                    {
                        bucket.expires_at = Some(start);
                    }
                }
                let covers_cycle = self.buckets.iter().any(|bucket| {
                    bucket.kind == CreditBucketKind::Monthly
                        && bucket.starts_at >= start
                        && bucket_active(bucket, now)
                });
                let allowed = monthly.renewal_ends_at.is_none_or(|ends| start < ends);
                if allowed && !covers_cycle && monthly.amount > 0.0 {
                    let expiry = monthly_occurrence(&monthly, index + 1)?;
                    self.reserve_bucket_slot(now)?;
                    self.buckets.push(CreditBucket {
                        id: monthly_bucket_id(start),
                        kind: CreditBucketKind::Monthly,
                        label: self.configuration.name.clone(),
                        granted: monthly.amount,
                        remaining: monthly.amount,
                        starts_at: start,
                        expires_at: Some(expiry),
                    });
                }
                self.monthly_cursor = Some(start);
            }
        }
        self.prune_history(now);
        self.validate()
    }

    pub(crate) fn deduct(&mut self, amount: f64, now: DateTime<Utc>) -> Result<()> {
        finite_amount(amount, "debit")?;
        self.advance(now)?;
        let mut leftover = amount;
        for index in self.active_indices_by_expiry(now) {
            if leftover <= 0.0 {
                break;
            }
            let take = self.buckets[index].remaining.min(leftover);
            self.buckets[index].remaining -= take;
            leftover -= take;
        }
        let spent = self.spent_since_calibration + amount;
        ensure!(spent.is_finite(), "spent overflow");
        self.spent_since_calibration = spent;
        if leftover > 0.0 {
            let overdrawn = self.overdrawn + leftover;
            ensure!(overdrawn.is_finite(), "overdrawn overflow");
            self.overdrawn = overdrawn;
        }
        self.validate()
    }

    pub(crate) fn calibrate(
        &mut self,
        corrections: &[CreditBalanceCorrection],
        now: DateTime<Utc>,
    ) -> Result<()> {
        ensure!(
            corrections.len() <= MAX_CORRECTIONS,
            "too many credit corrections"
        );
        let mut seen = HashSet::new();
        for correction in corrections {
            validate_id(&correction.bucket_id, "bucket id")?;
            ensure!(
                seen.insert(correction.bucket_id.as_str()),
                "duplicate bucket id"
            );
            let bucket = self
                .buckets
                .iter_mut()
                .find(|bucket| bucket.id == correction.bucket_id)
                .ok_or_else(|| anyhow!("unknown bucket id"))?;
            finite_amount(correction.remaining, "calibrated remaining")?;
            ensure!(
                correction.remaining <= bucket.granted,
                "calibrated remaining exceeds granted"
            );
            bucket.remaining = correction.remaining;
        }
        self.overdrawn = 0.0;
        self.unpriced_requests = 0;
        self.spent_since_calibration = 0.0;
        self.last_calibration_at = Some(now);
        self.validate_monthly_capacity(now)?;
        self.validate()
    }

    pub(crate) fn add_grant(
        &mut self,
        label: String,
        amount: f64,
        expires_at: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> Result<String> {
        finite_amount(amount, "grant")?;
        ensure!(amount > 0.0, "grant amount must be positive");
        validate_label(&label, "grant label")?;
        if let Some(expires_at) = expires_at {
            ensure!(expires_at > now, "grant expiry must be after now");
        }
        self.advance(now)?;
        self.reserve_bucket_slot(now)?;
        if self.configuration.monthly.is_some() {
            ensure!(
                self.funded_nonmonthly_count(now) < MAX_BUCKETS - 1,
                "monthly renewal requires one reserved credit bucket"
            );
        }
        let id = unique_grant_id(&self.buckets, now);
        let kind = if expires_at.is_some() {
            CreditBucketKind::TopUp
        } else {
            CreditBucketKind::Manual
        };
        self.buckets.push(CreditBucket {
            id: id.clone(),
            kind,
            label,
            granted: amount,
            remaining: amount,
            starts_at: now,
            expires_at,
        });
        self.validate()?;
        Ok(id)
    }

    /// Rate and monthly-policy edits keep current remaining. `now` pins the
    /// monthly cursor so a rewritten past/now anchor cannot refill this cycle.
    /// Next grant is the following real boundary of the new calendar.
    pub(crate) fn configure(
        &mut self,
        configuration: CreditConfiguration,
        now: DateTime<Utc>,
    ) -> Result<()> {
        validate_configuration(&configuration)?;
        if configuration.monthly.is_some() {
            ensure!(
                self.funded_nonmonthly_count(now) < MAX_BUCKETS,
                "monthly renewal requires one reserved credit bucket"
            );
        }
        if monthly_policy_changed(&self.configuration.monthly, &configuration.monthly) {
            self.hold_current_monthly_grant(&configuration.monthly, now)?;
        }
        self.configuration = configuration;
        self.validate()
    }

    pub(crate) fn capture_attempt(
        &self,
        account_id: String,
        model: &str,
        at: DateTime<Utc>,
    ) -> CreditAttempt {
        CreditAttempt {
            credential_id: self.credential_id.clone(),
            meter_id: self.meter_id.clone(),
            destination_id: self.destination_id.clone(),
            endpoint: self.endpoint.clone(),
            account_id,
            model: model.to_string(),
            currency: self.configuration.currency.clone(),
            credits_per_currency: self.configuration.credits_per_currency,
            rate: credit_rate_for_model(&self.configuration.rates, model).cloned(),
            at,
        }
    }

    fn active_indices_by_expiry(&self, now: DateTime<Utc>) -> Vec<usize> {
        let mut indices: Vec<usize> = self
            .buckets
            .iter()
            .enumerate()
            .filter(|(_, bucket)| bucket_active(bucket, now))
            .map(|(index, _)| index)
            .collect();
        indices.sort_by(|&left, &right| {
            match (
                self.buckets[left].expires_at,
                self.buckets[right].expires_at,
            ) {
                (Some(a), Some(b)) => a
                    .cmp(&b)
                    .then_with(|| self.buckets[left].id.cmp(&self.buckets[right].id)),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => self.buckets[left].id.cmp(&self.buckets[right].id),
            }
        });
        indices
    }

    fn prune_history(&mut self, now: DateTime<Utc>) {
        let mut keep = Vec::new();
        let mut expired = Vec::new();
        for bucket in self.buckets.drain(..) {
            if bucket_expired(&bucket, now) {
                expired.push(bucket);
            } else {
                keep.push(bucket);
            }
        }
        expired.sort_by_key(|bucket| std::cmp::Reverse(bucket.expires_at));
        expired.truncate(MAX_EXPIRED_BUCKETS);
        let room = MAX_BUCKETS.saturating_sub(keep.len());
        expired.truncate(room);
        keep.append(&mut expired);
        self.buckets = keep;
    }

    fn funded_nonmonthly_count(&self, now: DateTime<Utc>) -> usize {
        self.buckets
            .iter()
            .filter(|bucket| {
                bucket.kind != CreditBucketKind::Monthly
                    && bucket.remaining > 0.0
                    && !bucket_expired(bucket, now)
            })
            .count()
    }

    fn validate_monthly_capacity(&self, now: DateTime<Utc>) -> Result<()> {
        if self.configuration.monthly.is_some() {
            ensure!(
                self.funded_nonmonthly_count(now) < MAX_BUCKETS,
                "monthly renewal requires one reserved credit bucket"
            );
        }
        Ok(())
    }

    fn reserve_bucket_slot(&mut self, now: DateTime<Utc>) -> Result<()> {
        self.prune_history(now);
        if self.buckets.len() >= MAX_BUCKETS {
            self.buckets.retain(|bucket| !bucket_expired(bucket, now));
        }
        if self.buckets.len() >= MAX_BUCKETS {
            // Keep zero active grants correctable until a new grant needs room.
            if let Some(index) = self.buckets.iter().position(|bucket| {
                bucket.remaining == 0.0 && bucket.kind != CreditBucketKind::Monthly
            }) {
                self.buckets.remove(index);
            }
        }
        ensure!(
            self.buckets.len() < MAX_BUCKETS,
            "too many funded credit buckets"
        );
        Ok(())
    }

    fn hold_current_monthly_grant(
        &mut self,
        new_monthly: &Option<MonthlyCredits>,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let Some(monthly) = new_monthly else {
            return Ok(());
        };
        let Some(index) = current_cycle_index(monthly, now)? else {
            return Ok(());
        };
        let start = monthly_occurrence(monthly, index)?;
        self.monthly_cursor = Some(start);
        let next = monthly_occurrence(monthly, index + 1)?;
        for bucket in &mut self.buckets {
            if bucket.kind != CreditBucketKind::Monthly || !bucket_active(bucket, now) {
                continue;
            }
            let expiry = match bucket.expires_at {
                Some(exp) if exp >= next => exp,
                _ => next,
            };
            if expiry > bucket.starts_at {
                bucket.expires_at = Some(expiry);
            }
        }
        Ok(())
    }
}

impl CreditAttempt {
    /// Token total includes cache groups. Missing cache-write prices stay unknown.
    pub(crate) fn charge(&self, tokens: BillingTokens) -> Option<f64> {
        let rate = self.rate.as_ref()?;
        let currency = token_charge(
            tokens,
            TokenRates::per_million(
                rate.input_per_million,
                rate.output_per_million,
                rate.cache_read_per_million,
                rate.cache_write_per_million,
            ),
        )?;
        convert_charge(currency, self.credits_per_currency)
    }
}

pub(crate) fn credit_rate_for_model<'a>(
    rates: &'a [CreditRate],
    model: &str,
) -> Option<&'a CreditRate> {
    rates.iter().find(|rate| rate.model == model)
}

fn monthly_policy_changed(
    previous: &Option<MonthlyCredits>,
    next: &Option<MonthlyCredits>,
) -> bool {
    match (previous, next) {
        (None, None) => false,
        (Some(previous), Some(next)) => {
            previous.amount != next.amount
                || previous.next_reset_at != next.next_reset_at
                || previous.timezone_offset_minutes != next.timezone_offset_minutes
                || previous.renewal_ends_at != next.renewal_ends_at
        }
        _ => true,
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreditPresetResource {
    currency: String,
    credits_per_currency: f64,
    source_url: String,
    tiers: Vec<CreditPresetTier>,
    rates: Vec<CreditRate>,
}
#[derive(Deserialize)]
struct CreditPresetTier {
    id: String,
    name: String,
    amount: f64,
}

static STEPFUN_PRESETS: std::sync::LazyLock<CreditPresetResource> =
    std::sync::LazyLock::new(|| {
        serde_json::from_str(include_str!(
            "../../../resources/stepfun-credit-presets.json"
        ))
        .expect("bundled StepFun credit preset resource must be valid")
    });

fn bucket_active(bucket: &CreditBucket, now: DateTime<Utc>) -> bool {
    bucket.starts_at <= now && bucket.expires_at.is_none_or(|expires| now < expires)
}

fn bucket_expired(bucket: &CreditBucket, now: DateTime<Utc>) -> bool {
    bucket.expires_at.is_some_and(|expires| now >= expires)
}

fn current_cycle_index(monthly: &MonthlyCredits, now: DateTime<Utc>) -> Result<Option<i32>> {
    if now < monthly.next_reset_at {
        return Ok(None);
    }
    let tz = fixed_offset(monthly.timezone_offset_minutes)?;
    let anchor = monthly.next_reset_at.with_timezone(&tz);
    let wall = now.with_timezone(&tz);
    let mut index = (wall.year() - anchor.year())
        .checked_mul(12)
        .and_then(|months| months.checked_add(wall.month() as i32 - anchor.month() as i32))
        .ok_or_else(|| anyhow!("invalid monthly recurrence"))?;
    if index < 0 {
        return Ok(None);
    }
    while index >= 0 && monthly_occurrence(monthly, index)? > now {
        index -= 1;
    }
    if index < 0 {
        return Ok(None);
    }
    let mut guard = 0;
    while guard < 12 * 200 {
        match monthly_occurrence(monthly, index + 1) {
            Ok(next) if next <= now => index += 1,
            _ => break,
        }
        guard += 1;
    }
    Ok(Some(index))
}

fn monthly_occurrence(monthly: &MonthlyCredits, index: i32) -> Result<DateTime<Utc>> {
    if index == 0 {
        return Ok(monthly.next_reset_at);
    }
    let tz = fixed_offset(monthly.timezone_offset_minutes)?;
    let anchor = monthly.next_reset_at.with_timezone(&tz).naive_local();
    let wall = add_calendar_months(anchor, index, anchor.day())?;
    tz.from_local_datetime(&wall)
        .single()
        .map(|local| local.with_timezone(&Utc))
        .ok_or_else(|| anyhow!("invalid monthly recurrence"))
}

fn add_calendar_months(
    wall: chrono::NaiveDateTime,
    months: i32,
    original_day: u32,
) -> Result<chrono::NaiveDateTime> {
    let month0 = (wall.year() as i64) * 12 + i64::from(wall.month()) - 1 + i64::from(months);
    let year =
        i32::try_from(month0.div_euclid(12)).map_err(|_| anyhow!("invalid calendar month"))?;
    let month =
        u32::try_from(month0.rem_euclid(12) + 1).map_err(|_| anyhow!("invalid calendar month"))?;
    let last = last_day_of_month(year, month)?;
    let day = original_day.min(last);
    NaiveDate::from_ymd_opt(year, month, day)
        .map(|date| date.and_time(wall.time()))
        .ok_or_else(|| anyhow!("invalid calendar month"))
}

fn last_day_of_month(year: i32, month: u32) -> Result<u32> {
    let (next_year, next_month) = if month == 12 {
        (
            year.checked_add(1)
                .ok_or_else(|| anyhow!("invalid calendar month"))?,
            1,
        )
    } else {
        (year, month + 1)
    };
    let first_next = NaiveDate::from_ymd_opt(next_year, next_month, 1)
        .ok_or_else(|| anyhow!("invalid calendar month"))?;
    Ok(first_next
        .pred_opt()
        .ok_or_else(|| anyhow!("invalid calendar month"))?
        .day())
}

fn next_month_start(at: DateTime<Utc>, offset_minutes: i32) -> Result<DateTime<Utc>> {
    let tz = fixed_offset(offset_minutes)?;
    let wall = at.with_timezone(&tz);
    let (year, month) = if wall.month() == 12 {
        (
            wall.year()
                .checked_add(1)
                .ok_or_else(|| anyhow!("invalid calendar month"))?,
            1,
        )
    } else {
        (wall.year(), wall.month() + 1)
    };
    let naive = NaiveDate::from_ymd_opt(year, month, 1)
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .ok_or_else(|| anyhow!("invalid calendar month"))?;
    tz.from_local_datetime(&naive)
        .single()
        .map(|local| local.with_timezone(&Utc))
        .ok_or_else(|| anyhow!("invalid calendar month"))
}

fn fixed_offset(offset_minutes: i32) -> Result<FixedOffset> {
    let seconds = offset_minutes
        .checked_mul(60)
        .ok_or_else(|| anyhow!("invalid timezone offset"))?;
    FixedOffset::east_opt(seconds).ok_or_else(|| anyhow!("invalid timezone offset"))
}

fn monthly_bucket_id(start: DateTime<Utc>) -> String {
    format!("monthly-{}", start.timestamp())
}

fn unique_grant_id(buckets: &[CreditBucket], now: DateTime<Utc>) -> String {
    let base = format!("grant-{}", now.timestamp_millis());
    if buckets.iter().all(|bucket| bucket.id != base) {
        return base;
    }
    for suffix in 1..u32::MAX {
        let candidate = format!("{base}-{suffix}");
        if buckets.iter().all(|bucket| bucket.id != candidate) {
            return candidate;
        }
    }
    base
}

fn validate_configuration(configuration: &CreditConfiguration) -> Result<()> {
    validate_label(&configuration.name, "configuration name")?;
    validate_currency(&configuration.currency)?;
    ensure!(
        configuration.credits_per_currency.is_finite()
            && configuration.credits_per_currency > 0.0
            && configuration.credits_per_currency <= MAX_AMOUNT,
        "invalid credits per currency"
    );
    ensure!(
        configuration.rates.len() <= MAX_RATES,
        "too many credit rates"
    );
    let mut models = HashSet::new();
    for rate in &configuration.rates {
        validate_model(&rate.model)?;
        ensure!(models.insert(rate.model.as_str()), "duplicate rate model");
        finite_amount(rate.input_per_million, "input rate")?;
        finite_amount(rate.output_per_million, "output rate")?;
        if let Some(cache_read) = rate.cache_read_per_million {
            finite_amount(cache_read, "cache read rate")?;
        }
        if let Some(cache_write) = rate.cache_write_per_million {
            finite_amount(cache_write, "cache write rate")?;
        }
    }
    if let Some(monthly) = &configuration.monthly {
        finite_amount(monthly.amount, "monthly amount")?;
        let _ = fixed_offset(monthly.timezone_offset_minutes)?;
        if let Some(ends) = monthly.renewal_ends_at {
            ensure!(ends > monthly.next_reset_at, "invalid renewal end");
        }
    }
    if let Some(source) = &configuration.source_url {
        validate_source_url(source)?;
    }
    Ok(())
}

fn validate_bucket(bucket: &CreditBucket) -> Result<()> {
    validate_id(&bucket.id, "bucket id")?;
    validate_label(&bucket.label, "bucket label")?;
    finite_amount(bucket.granted, "granted")?;
    finite_amount(bucket.remaining, "remaining")?;
    ensure!(
        bucket.remaining <= bucket.granted,
        "remaining exceeds granted"
    );
    if let Some(expires_at) = bucket.expires_at {
        ensure!(
            expires_at > bucket.starts_at,
            "bucket expiry precedes start"
        );
    }
    Ok(())
}

fn validate_id(value: &str, what: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= MAX_ID_LEN
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')),
        "invalid {what}"
    );
    Ok(())
}

fn validate_model(value: &str) -> Result<()> {
    let trimmed = value.trim();
    ensure!(!trimmed.is_empty(), "invalid model name");
    ensure!(trimmed.chars().count() <= 200, "invalid model name");
    ensure!(
        !trimmed.contains('\0') && !trimmed.chars().any(char::is_control),
        "invalid model name"
    );
    Ok(())
}

fn validate_currency(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= 16
            && value.bytes().all(|byte| byte.is_ascii_alphanumeric()),
        "invalid currency"
    );
    Ok(())
}

fn validate_label(value: &str, what: &str) -> Result<()> {
    ensure!(
        !value.is_empty() && value.len() <= MAX_LABEL_LEN && !value.chars().any(char::is_control),
        "invalid {what}"
    );
    Ok(())
}

fn validate_endpoint(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty() && value.len() <= MAX_URL_LEN,
        "invalid endpoint URL"
    );
    let parsed = reqwest::Url::parse(value).map_err(|_| anyhow!("invalid endpoint URL"))?;
    ensure!(
        matches!(parsed.scheme(), "http" | "https")
            && parsed.host_str().is_some()
            && parsed.username().is_empty()
            && parsed.password().is_none(),
        "invalid endpoint URL"
    );
    Ok(())
}

fn validate_source_url(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty() && value.len() <= MAX_URL_LEN,
        "invalid source URL"
    );
    let parsed = reqwest::Url::parse(value).map_err(|_| anyhow!("invalid source URL"))?;
    ensure!(
        parsed.scheme() == "https"
            && parsed.host_str().is_some()
            && parsed.username().is_empty()
            && parsed.password().is_none(),
        "invalid source URL"
    );
    Ok(())
}

fn finite_amount(value: f64, what: &str) -> Result<f64> {
    if value.is_finite() && (0.0..=MAX_AMOUNT).contains(&value) {
        Ok(value)
    } else {
        bail!("invalid {what}")
    }
}

#[cfg(test)]
mod tests;
