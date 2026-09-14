//! Operator publication of public model names to downstream clients.
//!
//! Presence in the unpublished set hides a name from `GET /v1/models` only.
//! Routing and `GET /dashboard/api/v3/application-models` stay unchanged.
//! Missing names default to published.

use std::collections::HashSet;

pub const MAX_PUBLIC_MODEL_CHARS: usize = 200;

/// Case-folded public-name key used for persistence and list filtering.
pub fn normalize_public_model_key(value: &str) -> Result<String, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err("publicModel is required".into());
    }
    if trimmed.chars().count() > MAX_PUBLIC_MODEL_CHARS {
        return Err("publicModel must be at most 200 characters".into());
    }
    if trimmed.chars().any(|c| c.is_control()) {
        return Err("publicModel cannot contain control characters".into());
    }
    Ok(trimmed.to_ascii_lowercase())
}

/// Whether `GET /v1/models` may advertise this public name.
pub fn is_downstream_visible(id: &str, unpublished: &HashSet<String>) -> bool {
    !unpublished.contains(&id.trim().to_ascii_lowercase())
}

#[cfg(test)]
mod tests;
