//! Fixed BYOK format adapters. No registry.
mod codex;
mod kimi;
mod minimax;
mod zcode;

#[cfg(test)]
mod tests;

use super::receipt::{ApplyPlan, FileRole, ManagedSnapshot, PlannedFile, Receipt};
use super::{ByokError, ByokResult};
use crate::byok_application::{ByokClient, ByokModel};
use crate::model_metadata::ModelMetadata;

pub use codex::CodexAdapter;
pub use kimi::KimiAdapter;
pub use minimax::MinimaxAdapter;
pub use zcode::ZcodeAdapter;

pub const PROVIDER_ID: &str = "ocg";
pub const PROVIDER_NAME: &str = "Open Console Gateway";

pub struct ConfigureInput<'a> {
    pub gateway_v1_url: &'a str,
    pub secret: &'a str,
    pub models: &'a [ByokModel],
    pub default_model_id: Option<&'a str>,
}

pub struct ParsedStatus {
    pub incompatible: Option<String>,
    pub collision: bool,
    pub configured_model_ids: Vec<String>,
    pub current_default: Option<String>,
    pub user_changed_owned: bool,
}

pub trait FormatAdapter {
    fn inspect_bytes(
        &self,
        target_bytes: Option<&[u8]>,
        catalog_bytes: Option<&[u8]>,
        receipt: Option<&Receipt>,
    ) -> ParsedStatus;

    fn configure(
        &self,
        target_path: &std::path::Path,
        catalog_path: Option<&std::path::Path>,
        target_bytes: Option<&[u8]>,
        catalog_bytes: Option<&[u8]>,
        receipt: Option<&Receipt>,
        input: ConfigureInput<'_>,
    ) -> ByokResult<ApplyPlan>;

    fn remove(
        &self,
        target_path: &std::path::Path,
        catalog_path: Option<&std::path::Path>,
        target_bytes: Option<&[u8]>,
        catalog_bytes: Option<&[u8]>,
        receipt: &Receipt,
    ) -> ByokResult<ApplyPlan>;
}

pub fn adapter(client: ByokClient) -> &'static dyn FormatAdapter {
    match client {
        ByokClient::Codex => &CodexAdapter,
        ByokClient::Kimi => &KimiAdapter,
        ByokClient::Minimax => &MinimaxAdapter,
        ByokClient::Zcode => &ZcodeAdapter,
    }
}

pub fn validate_models(_client: ByokClient, models: &[ByokModel]) -> ByokResult<()> {
    let mut seen = std::collections::BTreeSet::new();
    for model in models {
        if model.id.trim().is_empty() {
            return Err(ByokError::invalid("Model id is invalid"));
        }
        if !seen.insert(model.id.as_str()) {
            return Err(ByokError::invalid("Model ids must be unique"));
        }
    }
    Ok(())
}

pub fn display_name(model: &ByokModel) -> &str {
    model
        .metadata
        .name
        .as_deref()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(model.id.as_str())
}

pub fn owned_changed(receipt: Option<&Receipt>, current: Option<&serde_json::Value>) -> bool {
    match (receipt, current) {
        (Some(receipt), Some(current)) => {
            // Secret-bearing fields are cleared before a snapshot is persisted,
            // so both sides are compared without them. A receipt written by an
            // older version still holds the raw values; stripping them here
            // keeps that receipt comparable instead of flagging every one as
            // an external edit.
            super::receipt::without_secrets(&receipt.last_managed.owned)
                != super::receipt::without_secrets(current)
        }
        (Some(_), None) => true,
        (None, _) => false,
    }
}

/// A receipt means OCG already owns a snapshot. A present target is compared in
/// full, including when the provider entry itself is gone. A missing target is
/// not a conflict: the file was removed, so configure may recreate it. Callers
/// report that case through the absent-bytes branch instead.
pub fn ownership_conflict(
    receipt: Option<&Receipt>,
    target_present: bool,
    current: &serde_json::Value,
) -> bool {
    if !target_present {
        return false;
    }
    owned_changed(receipt, Some(current))
}

pub fn restore_default(receipt: &Receipt, current_default: Option<&str>) -> Option<Option<String>> {
    match &receipt.last_applied_default {
        Some(applied) if current_default == Some(applied.as_str()) => {
            Some(receipt.baseline_default.clone())
        }
        Some(_) => None,
        None => None,
    }
}

/// Default that remove would leave behind. `Some(None)` from [`restore_default`]
/// means the applied default is cleared back to an absent baseline.
pub fn default_after_restore(receipt: &Receipt, current: Option<&str>) -> Option<String> {
    match restore_default(receipt, current) {
        Some(baseline) => baseline,
        None => current.map(str::to_string),
    }
}

/// `(baseline, last_applied, effective)`.
///
/// The baseline is captured on the first request that changes the default.
/// An earlier `None` does not freeze the value then in the file: the store
/// persists `baseline_default` on every successful configure.
pub fn default_plan_defaults(
    receipt: Option<&Receipt>,
    requested: Option<&str>,
    current: Option<&str>,
) -> (Option<String>, Option<String>, Option<String>) {
    let applied_before = receipt.and_then(|receipt| receipt.last_applied_default.clone());
    let baseline = if applied_before.is_some() {
        receipt.and_then(|receipt| receipt.baseline_default.clone())
    } else if requested.is_some() {
        current.map(str::to_string)
    } else {
        None
    };
    let last_applied = match requested {
        Some(value) => Some(value.to_string()),
        None => applied_before,
    };
    let effective = match requested {
        Some(value) => Some(value.to_string()),
        None => current.map(str::to_string),
    };
    (baseline, last_applied, effective)
}

pub fn ensure_default_selected(
    default_model_id: Option<&str>,
    models: &[ByokModel],
) -> ByokResult<()> {
    if let Some(id) = default_model_id {
        if !models.iter().any(|model| model.id == id) {
            return Err(ByokError::invalid(
                "defaultModelId must be one of the selected models",
            ));
        }
    }
    Ok(())
}

pub fn ensure_retained_ocg_default(
    ocg_model_id: Option<&str>,
    retained: &[String],
) -> ByokResult<()> {
    let Some(id) = ocg_model_id else {
        return Ok(());
    };
    if !id.is_empty() && retained.iter().any(|kept| kept == id) {
        return Ok(());
    }
    Err(ByokError::conflict(
        "The current default still points at an OCG model that would be removed",
    ))
}

pub(super) fn toml_item_json(item: &toml_edit::Item) -> serde_json::Value {
    if item.is_none() {
        return serde_json::Value::Null;
    }
    if let Some(table) = item.as_table_like() {
        return toml_table_like_json(table);
    }
    if let Some(tables) = item.as_array_of_tables() {
        return serde_json::Value::Array(
            tables
                .iter()
                .map(|table| toml_table_like_json(table))
                .collect(),
        );
    }
    if let Some(value) = item.as_value() {
        return toml_value_json(value);
    }
    serde_json::Value::Null
}

fn toml_table_like_json(table: &dyn toml_edit::TableLike) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (key, item) in table.iter() {
        if item.is_none() {
            continue;
        }
        map.insert(key.to_string(), toml_item_json(item));
    }
    serde_json::Value::Object(map)
}

fn toml_value_json(value: &toml_edit::Value) -> serde_json::Value {
    if let Some(text) = value.as_str() {
        return serde_json::Value::String(text.to_string());
    }
    if let Some(number) = value.as_integer() {
        return serde_json::Value::Number(number.into());
    }
    if let Some(number) = value.as_float() {
        return serde_json::Number::from_f64(number)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null);
    }
    if let Some(flag) = value.as_bool() {
        return serde_json::Value::Bool(flag);
    }
    if let Some(array) = value.as_array() {
        return serde_json::Value::Array(array.iter().map(toml_value_json).collect());
    }
    if let Some(table) = value.as_inline_table() {
        return toml_table_like_json(table);
    }
    if let Some(datetime) = value.as_datetime() {
        return serde_json::Value::String(datetime.to_string());
    }
    serde_json::Value::Null
}

pub fn first_owned(receipt: Option<&Receipt>, owned: &serde_json::Value) -> serde_json::Value {
    match receipt {
        Some(receipt) if !receipt.first_owned.is_null() => receipt.first_owned.clone(),
        _ => owned.clone(),
    }
}

pub fn preserve_created(
    receipt: Option<&Receipt>,
    created_target: bool,
    created_catalog: bool,
) -> (bool, bool) {
    match receipt {
        Some(receipt) => (receipt.created_target, receipt.created_catalog),
        None => (created_target, created_catalog),
    }
}

pub fn has_image(metadata: &ModelMetadata) -> bool {
    metadata
        .input_modalities
        .as_ref()
        .is_some_and(|modalities| modalities.iter().any(|item| item == "image"))
}

pub fn planned_target(path: std::path::PathBuf, bytes: Option<Vec<u8>>) -> PlannedFile {
    PlannedFile {
        role: FileRole::Target,
        path,
        new_bytes: bytes,
    }
}

pub fn planned_catalog(path: std::path::PathBuf, bytes: Option<Vec<u8>>) -> PlannedFile {
    PlannedFile {
        role: FileRole::Catalog,
        path,
        new_bytes: bytes,
    }
}

pub fn snapshot(
    model_ids: Vec<String>,
    owned: serde_json::Value,
    applied_default: Option<String>,
) -> ManagedSnapshot {
    ManagedSnapshot {
        provider_id: PROVIDER_ID.into(),
        model_ids,
        owned,
        applied_default,
    }
}
