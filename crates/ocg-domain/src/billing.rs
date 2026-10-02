//! Billing model labels and token counts carried by stored credit attempts.
//!
//! I/O-free. Request cost is not calculated here.

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

#[cfg(test)]
mod tests;
