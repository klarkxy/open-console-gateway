use super::{
    ConfigureInput, FormatAdapter, PROVIDER_ID, PROVIDER_NAME, ParsedStatus, default_after_restore,
    default_plan_defaults, display_name, ensure_default_selected, ensure_retained_ocg_default,
    first_owned, has_image, ownership_conflict, planned_target, preserve_created, restore_default,
    snapshot,
};
use crate::byok_application::{ByokError, ByokModel, ByokResult};
use crate::byok_application_host::receipt::{ApplyPlan, Receipt};
use serde_json::{Value, json};
use serde_yaml_ng::Mapping;
use std::path::Path;

pub struct MinimaxAdapter;

const CUSTOM_PROVIDER_PREFIX: &str = "custom_provider:";

impl FormatAdapter for MinimaxAdapter {
    fn inspect_bytes(
        &self,
        target_bytes: Option<&[u8]>,
        _catalog_bytes: Option<&[u8]>,
        receipt: Option<&Receipt>,
    ) -> ParsedStatus {
        let Some(bytes) = target_bytes else {
            return empty_status(receipt.is_some());
        };
        let Ok(root) = parse_yaml(bytes) else {
            return incompatible("MiniMax config.yaml is not valid YAML");
        };
        let Some(mapping) = as_mapping(&root) else {
            return incompatible("MiniMax config.yaml must be a mapping");
        };
        if unsafe_keys(mapping) {
            return incompatible("MiniMax config.yaml contains unsupported key segments");
        }
        let present = provider_present(mapping);
        let owned = owned_from_mapping(mapping);
        ParsedStatus {
            incompatible: None,
            collision: present && receipt.is_none(),
            configured_model_ids: ocg_model_ids(mapping),
            current_default: ocg_default(mapping),
            user_changed_owned: ownership_conflict(receipt, true, &owned),
        }
    }

    fn configure(
        &self,
        target_path: &Path,
        _catalog_path: Option<&Path>,
        target_bytes: Option<&[u8]>,
        _catalog_bytes: Option<&[u8]>,
        receipt: Option<&Receipt>,
        input: ConfigureInput<'_>,
    ) -> ByokResult<ApplyPlan> {
        if target_bytes.is_none() && receipt.is_some() {
            return Err(ByokError::conflict(
                "Owned MiniMax fields changed outside OCG",
            ));
        }
        let mut root = match target_bytes {
            None => serde_yaml_ng::Value::Mapping(Mapping::new()),
            Some(bytes) => parse_yaml(bytes)?,
        };
        let mapping = as_mapping_mut(&mut root).ok_or_else(|| {
            ByokError::invalid("Malformed MiniMax configuration cannot be overwritten")
        })?;
        if unsafe_keys(mapping) {
            return Err(ByokError::invalid(
                "MiniMax config.yaml contains unsupported key segments",
            ));
        }
        if provider_present(mapping) && receipt.is_none() {
            return Err(ByokError::conflict(
                "An unowned ocg MiniMax provider already exists",
            ));
        }
        if ownership_conflict(receipt, true, &owned_from_mapping(mapping)) {
            return Err(ByokError::conflict(
                "Owned MiniMax fields changed outside OCG",
            ));
        }
        ensure_default_selected(input.default_model_id, input.models)?;
        let model_ids: Vec<String> = input.models.iter().map(|model| model.id.clone()).collect();
        let current_default = string_entry(mapping, "defaultModel");
        let requested_default = input.default_model_id.map(minimax_default);
        let (baseline_default, last_applied_default, effective_default) = default_plan_defaults(
            receipt,
            requested_default.as_deref(),
            current_default.as_deref(),
        );
        let resulting = requested_default.as_deref().or(current_default.as_deref());
        ensure_retained_ocg_default(ocg_model_id(resulting), &model_ids)?;
        let preference_seed = if input.default_model_id.is_some() {
            captured_preferences(mapping)
        } else {
            Value::Null
        };
        let preferences = first_owned(receipt, &preference_seed);
        write_provider(mapping, &input)?;
        if let Some(model) = input.default_model_id {
            let next = minimax_default(model);
            if current_default.as_deref() != Some(next.as_str()) {
                mapping.remove(yaml_key("defaultModelThinking"));
                mapping.remove(yaml_key("defaultModelContextWindow"));
            }
            mapping.insert(yaml_key("defaultModel"), serde_yaml_ng::Value::String(next));
        }
        let owned = owned_from_mapping(mapping);
        let dumped = dump_yaml(&root)?;
        let (created_target, created_catalog) =
            preserve_created(receipt, target_bytes.is_none(), false);
        Ok(ApplyPlan {
            files: vec![planned_target(target_path.to_path_buf(), Some(dumped))],
            created_target,
            created_catalog,
            baseline_default,
            last_applied_default,
            managed: snapshot(model_ids, owned.clone(), effective_default),
            first_owned: preferences,
        })
    }

    fn remove(
        &self,
        target_path: &Path,
        _catalog_path: Option<&Path>,
        target_bytes: Option<&[u8]>,
        _catalog_bytes: Option<&[u8]>,
        receipt: &Receipt,
    ) -> ByokResult<ApplyPlan> {
        let Some(bytes) = target_bytes else {
            return Err(ByokError::conflict(
                "Owned MiniMax fields changed outside OCG",
            ));
        };
        let mut root = parse_yaml(bytes)?;
        let mapping = as_mapping_mut(&mut root).ok_or_else(|| {
            ByokError::invalid("Malformed MiniMax configuration cannot be overwritten")
        })?;
        if ownership_conflict(Some(receipt), true, &owned_from_mapping(mapping)) {
            return Err(ByokError::conflict(
                "Owned MiniMax fields changed outside OCG",
            ));
        }
        let current_default = string_entry(mapping, "defaultModel");
        let resulting = default_after_restore(receipt, current_default.as_deref());
        ensure_retained_ocg_default(ocg_model_id(resulting.as_deref()), &[])?;
        if let Some(serde_yaml_ng::Value::Mapping(custom)) =
            mapping.get_mut(yaml_key("custom_provider"))
        {
            custom.remove(yaml_key(PROVIDER_ID));
        }
        let restoring_default = restore_default(receipt, current_default.as_deref()).is_some();
        match restore_default(receipt, current_default.as_deref()) {
            Some(Some(value_str)) => {
                mapping.insert(
                    yaml_key("defaultModel"),
                    serde_yaml_ng::Value::String(value_str),
                );
            }
            Some(None) => {
                mapping.remove(yaml_key("defaultModel"));
            }
            None => {}
        }
        if restoring_default {
            restore_preferences(mapping, receipt);
        }
        let dumped = dump_yaml(&root)?;
        let target_out =
            if receipt.created_target && as_mapping(&root).is_some_and(Mapping::is_empty) {
                None
            } else {
                Some(dumped)
            };
        Ok(ApplyPlan {
            files: vec![planned_target(target_path.to_path_buf(), target_out)],
            created_target: receipt.created_target,
            created_catalog: false,
            baseline_default: receipt.baseline_default.clone(),
            last_applied_default: None,
            managed: snapshot(Vec::new(), Value::Null, None),
            first_owned: receipt.first_owned.clone(),
        })
    }
}

fn empty_status(user_changed_owned: bool) -> ParsedStatus {
    ParsedStatus {
        incompatible: None,
        collision: false,
        configured_model_ids: Vec::new(),
        current_default: None,
        user_changed_owned,
    }
}

fn incompatible(message: &str) -> ParsedStatus {
    ParsedStatus {
        incompatible: Some(message.into()),
        collision: false,
        configured_model_ids: Vec::new(),
        current_default: None,
        user_changed_owned: false,
    }
}

fn parse_yaml(bytes: &[u8]) -> ByokResult<serde_yaml_ng::Value> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| ByokError::invalid("MiniMax config.yaml is not valid UTF-8"))?;
    if text.trim().is_empty() {
        return Ok(serde_yaml_ng::Value::Mapping(Mapping::new()));
    }
    serde_yaml_ng::from_str(text)
        .map_err(|_| ByokError::invalid("MiniMax config.yaml is not valid YAML"))
}

fn dump_yaml(value: &serde_yaml_ng::Value) -> ByokResult<Vec<u8>> {
    let mut text = serde_yaml_ng::to_string(value)
        .map_err(|_| ByokError::internal("failed to encode MiniMax YAML"))?;
    if let Some(rest) = text.strip_prefix("---\n") {
        text = rest.to_string();
    }
    if let Some(rest) = text.strip_prefix("---\r\n") {
        text = rest.to_string();
    }
    Ok(text.into_bytes())
}

fn yaml_key(key: &str) -> serde_yaml_ng::Value {
    serde_yaml_ng::Value::String(key.into())
}

fn as_mapping(value: &serde_yaml_ng::Value) -> Option<&Mapping> {
    match value {
        serde_yaml_ng::Value::Mapping(mapping) => Some(mapping),
        _ => None,
    }
}

fn as_mapping_mut(value: &mut serde_yaml_ng::Value) -> Option<&mut Mapping> {
    match value {
        serde_yaml_ng::Value::Mapping(mapping) => Some(mapping),
        _ => None,
    }
}

fn unsafe_keys(mapping: &Mapping) -> bool {
    mapping.keys().any(|key| {
        key.as_str()
            .is_some_and(|name| matches!(name, "__proto__" | "prototype" | "constructor"))
    })
}

fn provider_present(mapping: &Mapping) -> bool {
    mapping
        .get(yaml_key("custom_provider"))
        .and_then(as_mapping)
        .and_then(|custom| custom.get(yaml_key(PROVIDER_ID)))
        .is_some()
}

fn string_entry(mapping: &Mapping, key: &str) -> Option<String> {
    mapping
        .get(yaml_key(key))
        .and_then(serde_yaml_ng::Value::as_str)
        .map(str::to_string)
}

fn minimax_default(model_id: &str) -> String {
    format!("{CUSTOM_PROVIDER_PREFIX}{PROVIDER_ID}/{model_id}")
}

fn ocg_default(mapping: &Mapping) -> Option<String> {
    ocg_model_id(string_entry(mapping, "defaultModel").as_deref()).map(str::to_string)
}

fn ocg_model_id(default_model: Option<&str>) -> Option<&str> {
    let value = default_model?;
    let prefix = format!("{CUSTOM_PROVIDER_PREFIX}{PROVIDER_ID}/");
    Some(value.strip_prefix(&prefix)?)
}

fn ocg_model_ids(mapping: &Mapping) -> Vec<String> {
    let Some(provider) = mapping
        .get(yaml_key("custom_provider"))
        .and_then(as_mapping)
        .and_then(|custom| custom.get(yaml_key(PROVIDER_ID)))
        .and_then(as_mapping)
    else {
        return Vec::new();
    };
    let Some(models) = provider.get(yaml_key("models")).and_then(as_mapping) else {
        return Vec::new();
    };
    models
        .keys()
        .filter_map(serde_yaml_ng::Value::as_str)
        .map(str::to_string)
        .collect()
}

fn write_provider(mapping: &mut Mapping, input: &ConfigureInput<'_>) -> ByokResult<()> {
    if mapping.get(yaml_key("custom_provider")).is_none() {
        mapping.insert(
            yaml_key("custom_provider"),
            serde_yaml_ng::Value::Mapping(Mapping::new()),
        );
    }
    let custom = mapping
        .get_mut(yaml_key("custom_provider"))
        .and_then(as_mapping_mut)
        .ok_or_else(|| ByokError::invalid("custom_provider must be a mapping"))?;
    if custom.get(yaml_key(PROVIDER_ID)).is_none() {
        custom.insert(
            yaml_key(PROVIDER_ID),
            serde_yaml_ng::Value::Mapping(Mapping::new()),
        );
    }
    let provider = custom
        .get_mut(yaml_key(PROVIDER_ID))
        .and_then(as_mapping_mut)
        .ok_or_else(|| ByokError::invalid("ocg custom provider must be a mapping"))?;
    provider.insert(
        yaml_key("name"),
        serde_yaml_ng::Value::String(PROVIDER_NAME.into()),
    );
    provider.insert(
        yaml_key("kind"),
        serde_yaml_ng::Value::String("custom".into()),
    );
    provider.insert(yaml_key("enabled"), serde_yaml_ng::Value::Bool(true));
    provider.insert(
        yaml_key("api"),
        serde_yaml_ng::Value::String("openai-completions".into()),
    );
    let mut options = provider
        .get(yaml_key("options"))
        .and_then(as_mapping)
        .cloned()
        .unwrap_or_default();
    options.insert(
        yaml_key("apiKey"),
        serde_yaml_ng::Value::String(input.secret.into()),
    );
    options.insert(
        yaml_key("baseURL"),
        serde_yaml_ng::Value::String(input.gateway_v1_url.into()),
    );
    options.insert(
        yaml_key("authMode"),
        serde_yaml_ng::Value::String("api-key".into()),
    );
    provider.insert(yaml_key("options"), serde_yaml_ng::Value::Mapping(options));
    let mut models = Mapping::new();
    for model in input.models {
        models.insert(yaml_key(&model.id), model_value(model));
    }
    provider.insert(yaml_key("models"), serde_yaml_ng::Value::Mapping(models));
    Ok(())
}

fn model_value(model: &ByokModel) -> serde_yaml_ng::Value {
    let mut mapping = Mapping::new();
    mapping.insert(
        yaml_key("name"),
        serde_yaml_ng::Value::String(display_name(model).into()),
    );
    mapping.insert(yaml_key("enabled"), serde_yaml_ng::Value::Bool(true));
    mapping.insert(
        yaml_key("configuration_source"),
        serde_yaml_ng::Value::String("manual".into()),
    );
    let mut limit = Mapping::new();
    if let Some(context) = model.metadata.context_window {
        limit.insert(
            yaml_key("context"),
            serde_yaml_ng::Value::Number(context.into()),
        );
    }
    if let Some(output) = model.metadata.max_output_tokens {
        limit.insert(
            yaml_key("output"),
            serde_yaml_ng::Value::Number(output.into()),
        );
    }
    if !limit.is_empty() {
        mapping.insert(yaml_key("limit"), serde_yaml_ng::Value::Mapping(limit));
    }
    if let Some(tool_call) = model.metadata.tool_calling {
        mapping.insert(yaml_key("tool_call"), serde_yaml_ng::Value::Bool(tool_call));
    }
    if model.metadata.reasoning == Some(true) {
        mapping.insert(yaml_key("reasoning"), serde_yaml_ng::Value::Bool(true));
    }
    if has_image(&model.metadata) {
        mapping.insert(yaml_key("attachment"), serde_yaml_ng::Value::Bool(true));
        let mut capabilities = Mapping::new();
        capabilities.insert(yaml_key("support_image"), serde_yaml_ng::Value::Bool(true));
        mapping.insert(
            yaml_key("capabilities"),
            serde_yaml_ng::Value::Mapping(capabilities),
        );
    }
    serde_yaml_ng::Value::Mapping(mapping)
}

const PREFERENCE_KEYS: [&str; 2] = ["defaultModelThinking", "defaultModelContextWindow"];

fn captured_preferences(mapping: &Mapping) -> Value {
    let mut fields = serde_json::Map::new();
    fields.insert("captured".into(), Value::Bool(true));
    for key in PREFERENCE_KEYS {
        fields.insert(key.into(), yaml_to_json(mapping.get(yaml_key(key))));
    }
    Value::Object(fields)
}

fn yaml_to_json(value: Option<&serde_yaml_ng::Value>) -> Value {
    value
        .and_then(|value| serde_json::to_value(value).ok())
        .unwrap_or(Value::Null)
}

fn json_to_yaml(value: &Value) -> Option<serde_yaml_ng::Value> {
    match value {
        Value::Null => None,
        Value::Bool(flag) => Some(serde_yaml_ng::Value::Bool(*flag)),
        Value::String(text) => Some(serde_yaml_ng::Value::String(text.clone())),
        Value::Number(number) => {
            if let Some(value) = number.as_i64() {
                Some(serde_yaml_ng::Value::Number(value.into()))
            } else if let Some(value) = number.as_u64() {
                Some(serde_yaml_ng::Value::Number(value.into()))
            } else {
                number
                    .as_f64()
                    .map(|value| serde_yaml_ng::Value::Number(serde_yaml_ng::Number::from(value)))
            }
        }
        other => serde_json::from_value(other.clone()).ok(),
    }
}

fn restore_preferences(mapping: &mut Mapping, receipt: &Receipt) {
    if receipt.first_owned.get("captured").and_then(Value::as_bool) != Some(true) {
        return;
    }
    for key in PREFERENCE_KEYS {
        if mapping.get(yaml_key(key)).is_some() {
            continue;
        }
        let Some(value) = receipt.first_owned.get(key).and_then(json_to_yaml) else {
            continue;
        };
        mapping.insert(yaml_key(key), value);
    }
}

fn owned_from_mapping(mapping: &Mapping) -> Value {
    let provider = mapping
        .get(yaml_key("custom_provider"))
        .and_then(as_mapping)
        .and_then(|custom| custom.get(yaml_key(PROVIDER_ID)));
    json!({
        "provider": provider.and_then(|value| serde_json::to_value(value).ok()),
    })
}
