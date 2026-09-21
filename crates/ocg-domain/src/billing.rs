//! Shared billing arithmetic for timed quota, cash, and credits.
//!
//! I/O-free: callers select rates, time tiers, and currency. Token groups and
//! unit conversion live here. `input` is always the total prompt, including
//! both cache groups. `output` includes provider reasoning when the upstream
//! already included it; callers never add reasoning a second time.

use serde::{Deserialize, Serialize};

#[cfg(feature = "schemars")]
use schemars::JsonSchema;

/// Token counts for one priced attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BillingTokens {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
}

impl BillingTokens {
    pub fn new(input: i64, output: i64, cache_read: i64, cache_write: i64) -> Self {
        Self {
            input,
            output,
            cache_read,
            cache_write,
        }
    }

    /// Legacy compatibility: negatives become 0 and cache groups stay inside total input.
    pub fn clamped(input: i64, output: i64, cache_read: i64, cache_write: i64) -> Self {
        let input = input.max(0);
        let output = output.max(0);
        let cache_read = cache_read.clamp(0, input);
        let cache_write = cache_write.clamp(0, input - cache_read);
        Self {
            input,
            output,
            cache_read,
            cache_write,
        }
    }

    /// Nonnegative counts whose cache groups fit in `input` without overflowing the sum.
    pub fn valid(&self) -> bool {
        if self.input < 0 || self.output < 0 || self.cache_read < 0 || self.cache_write < 0 {
            return false;
        }
        match self.cache_read.checked_add(self.cache_write) {
            Some(cached) => cached <= self.input,
            None => false,
        }
    }
}

/// Per-token rates. `per_tokens` is the scale those rates are quoted against.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TokenRates {
    pub input: f64,
    pub output: f64,
    pub cache_read: Option<f64>,
    pub cache_write: Option<f64>,
    pub per_tokens: f64,
}

impl TokenRates {
    pub fn per_million(
        input: f64,
        output: f64,
        cache_read: Option<f64>,
        cache_write: Option<f64>,
    ) -> Self {
        Self {
            input,
            output,
            cache_read,
            cache_write,
            per_tokens: 1_000_000.0,
        }
    }
}

/// Four disjoint groups (uncached input, output, cache read, cache write)
/// weighted by `rates` and divided by `rates.per_tokens`.
pub fn token_charge(tokens: BillingTokens, rates: TokenRates) -> Option<f64> {
    if !tokens.valid() {
        return None;
    }
    if !finite_non_negative(rates.input) || !finite_non_negative(rates.output) {
        return None;
    }
    if !rates.per_tokens.is_finite() || rates.per_tokens <= 0.0 {
        return None;
    }
    let cache_read_rate = optional_group_rate(rates.cache_read, tokens.cache_read)?;
    let cache_write_rate = optional_group_rate(rates.cache_write, tokens.cache_write)?;
    let uncached = tokens
        .input
        .checked_sub(tokens.cache_read)
        .and_then(|rest| rest.checked_sub(tokens.cache_write))?;
    let weighted = group_weight(uncached, rates.input)?
        + group_weight(tokens.output, rates.output)?
        + group_weight(tokens.cache_read, cache_read_rate)?
        + group_weight(tokens.cache_write, cache_write_rate)?;
    if !weighted.is_finite() {
        return None;
    }
    let charge = weighted / rates.per_tokens;
    (charge.is_finite() && charge >= 0.0).then_some(charge)
}

/// Scale a finite nonnegative charge by a positive units-per-currency factor.
pub fn convert_charge(amount: f64, units_per_currency: f64) -> Option<f64> {
    if !finite_non_negative(amount) {
        return None;
    }
    if !units_per_currency.is_finite() || units_per_currency <= 0.0 {
        return None;
    }
    let converted = amount * units_per_currency;
    converted.is_finite().then_some(converted)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum BillingModel {
    Quota,
    Cash,
    Credits,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum BillingSource {
    Official,
    LocalEstimate,
    Unavailable,
}

fn finite_non_negative(value: f64) -> bool {
    value.is_finite() && value >= 0.0
}

fn optional_group_rate(rate: Option<f64>, tokens: i64) -> Option<f64> {
    match rate {
        Some(rate) => finite_non_negative(rate).then_some(rate),
        None if tokens == 0 => Some(0.0),
        None => None,
    }
}

fn group_weight(tokens: i64, rate: f64) -> Option<f64> {
    let value = (tokens as f64) * rate;
    value.is_finite().then_some(value)
}

#[cfg(test)]
mod tests;
