//! Cross-format usage interpretation. Token fields are read as unsigned
//! integers and missing values count as zero.

use super::shared::uint;
use serde_json::{Value, json};

pub(super) fn chat_usage_to_anthropic(usage: Option<&Value>) -> Value {
    let usage = usage.unwrap_or(&Value::Null);
    let cached = usage
        .pointer("/prompt_tokens_details/cached_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    json!({
        "input_tokens": uint(usage, "prompt_tokens").saturating_sub(cached),
        "output_tokens": uint(usage, "completion_tokens"),
        "cache_read_input_tokens": cached,
        "cache_creation_input_tokens": 0
    })
}

pub(super) fn responses_usage_to_anthropic(usage: Option<&Value>) -> Value {
    let usage = usage.unwrap_or(&Value::Null);
    let cached = usage
        .pointer("/input_tokens_details/cached_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    json!({
        "input_tokens": uint(usage, "input_tokens").saturating_sub(cached),
        "output_tokens": uint(usage, "output_tokens"),
        "cache_read_input_tokens": cached,
        "cache_creation_input_tokens": 0
    })
}

pub(super) fn anthropic_usage_to_chat(usage: Option<&Value>) -> Value {
    let usage = usage.unwrap_or(&Value::Null);
    let cached = uint(usage, "cache_read_input_tokens");
    let prompt = uint(usage, "input_tokens") + cached + uint(usage, "cache_creation_input_tokens");
    json!({
        "prompt_tokens": prompt,
        "completion_tokens": uint(usage, "output_tokens"),
        "total_tokens": prompt + uint(usage, "output_tokens"),
        "prompt_tokens_details": { "cached_tokens": cached }
    })
}

pub(super) fn anthropic_usage_to_responses(usage: Option<&Value>) -> Value {
    let usage = usage.unwrap_or(&Value::Null);
    let cached = uint(usage, "cache_read_input_tokens");
    let input = uint(usage, "input_tokens") + cached + uint(usage, "cache_creation_input_tokens");
    let output = uint(usage, "output_tokens");
    json!({
        "input_tokens": input,
        "output_tokens": output,
        "total_tokens": input + output,
        "input_tokens_details": { "cached_tokens": cached },
        "output_tokens_details": { "reasoning_tokens": 0 }
    })
}
