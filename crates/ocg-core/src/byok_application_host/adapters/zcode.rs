use super::{
    ConfigureInput, FormatAdapter, PROVIDER_ID, PROVIDER_NAME, ParsedStatus, default_after_restore,
    default_plan_defaults, ensure_default_selected, ensure_retained_ocg_default, first_owned,
    ownership_conflict, planned_target, preserve_created, restore_default, snapshot,
};
use crate::byok_application::{ByokError, ByokModel, ByokResult};
use crate::byok_application_host::receipt::{ApplyPlan, Receipt};
use serde_json::{Map, Value, json};
use std::path::Path;

pub struct ZcodeAdapter;

impl FormatAdapter for ZcodeAdapter {
    fn inspect_bytes(
        &self,
        target_bytes: Option<&[u8]>,
        _catalog_bytes: Option<&[u8]>,
        receipt: Option<&Receipt>,
    ) -> ParsedStatus {
        let Some(bytes) = target_bytes else {
            return empty_status(receipt.is_some());
        };
        let Ok(root) = parse_json(bytes) else {
            return incompatible("ZCode provider_config.json is not valid JSON");
        };
        if let Some(reason) = unsupported_schema(&root) {
            return incompatible(&reason);
        }
        let present = provider_rule(&root).is_some();
        let owned = owned_from_root(&root);
        ParsedStatus {
            incompatible: None,
            collision: present && receipt.is_none(),
            configured_model_ids: ocg_model_ids(&root),
            current_default: ocg_default(&root),
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
                "Owned ZCode fields changed outside OCG",
            ));
        }
        let mut root = match target_bytes {
            None => empty_document(),
            Some(bytes) => parse_json(bytes)?,
        };
        if target_bytes.is_some()
            && let Some(reason) = unsupported_schema(&root)
        {
            return Err(ByokError::invalid(reason));
        }
        if provider_rule(&root).is_some() && receipt.is_none() {
            return Err(ByokError::conflict(
                "An unowned ocg ZCode provider already exists",
            ));
        }
        if ownership_conflict(receipt, true, &owned_from_root(&root)) {
            return Err(ByokError::conflict(
                "Owned ZCode fields changed outside OCG",
            ));
        }
        ensure_default_selected(input.default_model_id, input.models)?;
        let model_ids: Vec<String> = input.models.iter().map(|model| model.id.clone()).collect();
        let current_default = selection_string(&root);
        let requested = input.default_model_id.map(selection_value);
        let requested_text = requested.as_ref().map(ToString::to_string);
        let (baseline_default, last_applied_default, effective_default) = default_plan_defaults(
            receipt,
            requested_text.as_deref(),
            current_default.as_deref(),
        );
        match input.default_model_id {
            Some(id) => ensure_retained_ocg_default(Some(id), &model_ids)?,
            None => {
                if let Some(id) = ocg_selection_id(current_default.as_deref()) {
                    ensure_retained_ocg_default(Some(&id), &model_ids)?;
                }
            }
        }
        write_provider(&mut root, &input)?;
        if let Some(selection) = requested {
            set_selection(&mut root, selection);
        }
        let owned = owned_from_root(&root);
        let bytes = encode(&root)?;
        let (created_target, created_catalog) =
            preserve_created(receipt, target_bytes.is_none(), false);
        Ok(ApplyPlan {
            files: vec![planned_target(target_path.to_path_buf(), Some(bytes))],
            created_target,
            created_catalog,
            baseline_default,
            last_applied_default,
            managed: snapshot(model_ids, owned.clone(), effective_default),
            first_owned: first_owned(receipt, &Value::Null),
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
                "Owned ZCode fields changed outside OCG",
            ));
        };
        let mut root = parse_json(bytes)?;
        if ownership_conflict(Some(receipt), true, &owned_from_root(&root)) {
            return Err(ByokError::conflict(
                "Owned ZCode fields changed outside OCG",
            ));
        }
        let current_default = selection_string(&root);
        let resulting = default_after_restore(receipt, current_default.as_deref());
        if let Some(id) = ocg_selection_id(resulting.as_deref()) {
            ensure_retained_ocg_default(Some(&id), &[])?;
        }
        remove_ocg(&mut root);
        match restore_default(receipt, current_default.as_deref()) {
            Some(Some(text)) => {
                if let Ok(value) = serde_json::from_str(&text) {
                    set_selection(&mut root, value);
                }
            }
            Some(None) => {
                if let Some(config) = config_mut(&mut root) {
                    config.remove("defaultModelSelection");
                }
            }
            None => {}
        }
        let encoded = encode(&root)?;
        let target_out = if receipt.created_target && !has_user_data(&root) {
            None
        } else {
            Some(encoded)
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

fn parse_json(bytes: &[u8]) -> ByokResult<Value> {
    serde_json::from_slice(bytes)
        .map_err(|_| ByokError::invalid("ZCode provider_config.json is not valid JSON"))
}

fn encode(value: &Value) -> ByokResult<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|_| ByokError::internal("failed to encode ZCode provider config"))?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn empty_document() -> Value {
    json!({
        "schemaVersion": 1,
        "config": {
            "providerOrder": [],
            "providerConfigRules": { "providerRules": [] },
            "modelConfigRules": {
                "providerModelRules": [],
                "manualProviderModelRules": []
            }
        }
    })
}

fn unsupported_schema(root: &Value) -> Option<String> {
    let Some(object) = root.as_object() else {
        return Some("ZCode provider config must be a JSON object".into());
    };
    if !object.contains_key("schemaVersion") {
        return Some("ZCode provider config is a legacy format and cannot be rewritten".into());
    }
    match object.get("schemaVersion") {
        Some(Value::Number(number)) if number.as_u64() == Some(1) => {}
        Some(Value::Number(number)) => {
            return Some(format!(
                "ZCode provider config schemaVersion {} is unsupported",
                number
            ));
        }
        _ => return Some("ZCode provider config schemaVersion is invalid".into()),
    }
    let config = object.get("config")?.as_object()?;
    if config
        .get("providerConfigRules")
        .is_some_and(|value| !value.is_object())
        || config
            .get("modelConfigRules")
            .is_some_and(|value| !value.is_object())
    {
        return Some("ZCode provider config has an unsupported container shape".into());
    }
    None
}

fn config(root: &Value) -> Option<&Map<String, Value>> {
    root.get("config")?.as_object()
}

fn config_mut(root: &mut Value) -> Option<&mut Map<String, Value>> {
    root.get_mut("config")?.as_object_mut()
}

fn provider_rules(root: &Value) -> Option<&Vec<Value>> {
    config(root)?
        .get("providerConfigRules")?
        .get("providerRules")?
        .as_array()
}

fn provider_rules_mut(root: &mut Value) -> ByokResult<&mut Vec<Value>> {
    let config = config_mut(root).ok_or_else(|| ByokError::invalid("ZCode config is missing"))?;
    let rules = config
        .entry("providerConfigRules")
        .or_insert_with(|| json!({ "providerRules": [] }));
    let object = rules
        .as_object_mut()
        .ok_or_else(|| ByokError::invalid("providerConfigRules must be an object"))?;
    let list = object.entry("providerRules").or_insert_with(|| json!([]));
    list.as_array_mut()
        .ok_or_else(|| ByokError::invalid("providerRules must be an array"))
}

fn provider_rule(root: &Value) -> Option<&Value> {
    provider_rules(root)?
        .iter()
        .find(|rule| rule.get("providerId").and_then(Value::as_str) == Some(PROVIDER_ID))
}

fn ocg_model_ids(root: &Value) -> Vec<String> {
    provider_rule(root)
        .and_then(|rule| rule.get("config"))
        .and_then(|config| config.get("personalModelIds"))
        .and_then(Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn ocg_default(root: &Value) -> Option<String> {
    let selection = config(root)?.get("defaultModelSelection")?;
    if selection.get("providerId").and_then(Value::as_str) != Some(PROVIDER_ID) {
        return None;
    }
    selection
        .get("modelId")
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn selection_string(root: &Value) -> Option<String> {
    config(root)?
        .get("defaultModelSelection")
        .map(ToString::to_string)
}

fn ocg_selection_id(text: Option<&str>) -> Option<String> {
    let value: Value = serde_json::from_str(text?).ok()?;
    if value.get("providerId").and_then(Value::as_str) != Some(PROVIDER_ID) {
        return None;
    }
    Some(
        value
            .get("modelId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    )
}

fn selection_value(model_id: &str) -> Value {
    json!({
        "providerId": PROVIDER_ID,
        "modelId": model_id
    })
}

fn set_selection(root: &mut Value, selection: Value) {
    if let Some(config) = config_mut(root) {
        config.insert("defaultModelSelection".into(), selection);
    }
}

fn write_provider(root: &mut Value, input: &ConfigureInput<'_>) -> ByokResult<()> {
    if root.get("schemaVersion").is_none() {
        *root = empty_document();
    }
    let model_ids: Vec<Value> = input
        .models
        .iter()
        .map(|model| Value::String(model.id.clone()))
        .collect();
    let rule = json!({
        "providerId": PROVIDER_ID,
        "providerName": PROVIDER_NAME,
        "enabled": true,
        "config": {
            "group": "standard-personal",
            "access": { "type": "api-key", "apiKey": input.secret },
            "api": {
                "type": "openai-chat-completions",
                "baseUrl": input.gateway_v1_url
            },
            "personalModelIds": model_ids,
            "modelOrder": model_ids.clone()
        }
    });
    {
        let rules = provider_rules_mut(root)?;
        match rules
            .iter()
            .position(|item| item.get("providerId").and_then(Value::as_str) == Some(PROVIDER_ID))
        {
            Some(index) => {
                merge_rule(&mut rules[index], rule);
            }
            None => rules.push(rule),
        }
    }
    upsert_order(root);
    write_model_rules(root, input.models)?;
    Ok(())
}

fn merge_rule(existing: &mut Value, next: Value) {
    if let (Some(dst), Some(src)) = (existing.as_object_mut(), next.as_object()) {
        for (key, value) in src {
            if key == "config" {
                if let (Some(dst_config), Some(src_config)) = (
                    dst.entry("config")
                        .or_insert_with(|| json!({}))
                        .as_object_mut(),
                    value.as_object(),
                ) {
                    for (ck, cv) in src_config {
                        dst_config.insert(ck.clone(), cv.clone());
                    }
                }
            } else {
                dst.insert(key.clone(), value.clone());
            }
        }
    }
}

fn upsert_order(root: &mut Value) {
    let Some(config) = config_mut(root) else {
        return;
    };
    let order = config.entry("providerOrder").or_insert_with(|| json!([]));
    if let Some(list) = order.as_array_mut()
        && !list.iter().any(|item| item.as_str() == Some(PROVIDER_ID))
    {
        list.push(Value::String(PROVIDER_ID.into()));
    }
}

fn write_model_rules(root: &mut Value, models: &[ByokModel]) -> ByokResult<()> {
    let config = config_mut(root).ok_or_else(|| ByokError::invalid("ZCode config is missing"))?;
    let rules = config
        .entry("modelConfigRules")
        .or_insert_with(|| json!({ "providerModelRules": [], "manualProviderModelRules": [] }));
    let object = rules
        .as_object_mut()
        .ok_or_else(|| ByokError::invalid("modelConfigRules must be an object"))?;
    object
        .entry("manualProviderModelRules")
        .or_insert_with(|| json!([]));
    let list = object
        .entry("providerModelRules")
        .or_insert_with(|| json!([]));
    let list = list
        .as_array_mut()
        .ok_or_else(|| ByokError::invalid("providerModelRules must be an array"))?;
    list.retain(|rule| rule.get("providerId").and_then(Value::as_str) != Some(PROVIDER_ID));
    for model in models {
        list.push(sparse_model_rule(model));
    }
    Ok(())
}

fn sparse_model_rule(model: &ByokModel) -> Value {
    let mut properties = Map::new();
    if let Some(context) = model.metadata.context_window {
        properties.insert("contextWindow".into(), json!(context));
    }
    if let Some(tool_calling) = model.metadata.tool_calling {
        properties.insert("supportsToolCall".into(), json!(tool_calling));
    }
    if let Some(modalities) = &model.metadata.input_modalities {
        properties.insert(
            "inputFormat".into(),
            json!({
                "supportsImage": modalities.iter().any(|item| item == "image")
            }),
        );
    }
    let mut option_specs = Map::new();
    if let Some(max) = model.metadata.max_output_tokens {
        option_specs.insert("maxOutputTokens".into(), json!({ "max": max }));
    }
    let wires = reasoning_wires(model.metadata.reasoning_efforts.as_ref());
    if !wires.is_empty() {
        option_specs.insert(
            "reasoningLevel".into(),
            json!({
                "values": wires,
                "map": "{\"reasoning_effort\": reasoningLevel}"
            }),
        );
    }
    json!({
        "providerId": PROVIDER_ID,
        "modelId": model.id,
        "config": {
            "enabled": true,
            "properties": properties,
            "optionSpecs": option_specs
        }
    })
}

fn reasoning_wires(efforts: Option<&std::collections::BTreeMap<String, String>>) -> Vec<String> {
    let Some(efforts) = efforts else {
        return Vec::new();
    };
    let mut seen = std::collections::BTreeSet::new();
    let mut wires = Vec::new();
    for spelling in efforts.values() {
        if spelling.trim().is_empty() || !seen.insert(spelling.clone()) {
            continue;
        }
        wires.push(spelling.clone());
    }
    wires
}

fn remove_ocg(root: &mut Value) {
    if let Ok(rules) = provider_rules_mut(root) {
        rules.retain(|rule| rule.get("providerId").and_then(Value::as_str) != Some(PROVIDER_ID));
    }
    if let Some(config) = config_mut(root) {
        if let Some(order) = config
            .get_mut("providerOrder")
            .and_then(Value::as_array_mut)
        {
            order.retain(|item| item.as_str() != Some(PROVIDER_ID));
        }
        if let Some(rules) = config
            .get_mut("modelConfigRules")
            .and_then(Value::as_object_mut)
            && let Some(list) = rules
                .get_mut("providerModelRules")
                .and_then(Value::as_array_mut)
        {
            list.retain(|rule| rule.get("providerId").and_then(Value::as_str) != Some(PROVIDER_ID));
        }
    }
}

fn owned_from_root(root: &Value) -> Value {
    json!({
        "providers": ocg_provider_rules(root),
        "providerOrder": ocg_order_entries(root),
        "models": config(root)
            .and_then(|config| config.get("modelConfigRules"))
            .and_then(|rules| rules.get("providerModelRules"))
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter(|rule| rule.get("providerId").and_then(Value::as_str) == Some(PROVIDER_ID))
                    .cloned()
                    .collect::<Vec<_>>()
            }),
    })
}

fn ocg_provider_rules(root: &Value) -> Vec<Value> {
    provider_rules(root)
        .map(|list| {
            list.iter()
                .filter(|rule| rule.get("providerId").and_then(Value::as_str) == Some(PROVIDER_ID))
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

fn ocg_order_entries(root: &Value) -> Vec<Value> {
    config(root)
        .and_then(|config| config.get("providerOrder"))
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter(|item| item.as_str() == Some(PROVIDER_ID))
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

fn has_user_data(root: &Value) -> bool {
    let Some(object) = root.as_object() else {
        return true;
    };
    if object
        .keys()
        .any(|key| key != "schemaVersion" && key != "config")
    {
        return true;
    }
    if object
        .get("schemaVersion")
        .is_some_and(|value| value.as_u64() != Some(1))
    {
        return true;
    }
    let Some(config) = object.get("config").and_then(Value::as_object) else {
        return object.get("config").is_some();
    };
    const KNOWN: [&str; 4] = [
        "providerOrder",
        "providerConfigRules",
        "modelConfigRules",
        "defaultModelSelection",
    ];
    if config.keys().any(|key| !KNOWN.contains(&key.as_str())) {
        return true;
    }
    if config.get("defaultModelSelection").is_some() {
        return true;
    }
    if let Some(order) = config.get("providerOrder") {
        match order.as_array() {
            Some(list) if list.is_empty() => {}
            _ => return true,
        }
    }
    if let Some(rules) = config.get("providerConfigRules")
        && !provider_rules_empty(rules)
    {
        return true;
    }
    if let Some(rules) = config.get("modelConfigRules")
        && !model_rules_empty(rules)
    {
        return true;
    }
    false
}

fn provider_rules_empty(rules: &Value) -> bool {
    let Some(object) = rules.as_object() else {
        return false;
    };
    if object.keys().any(|key| key != "providerRules") {
        return false;
    }
    match object.get("providerRules") {
        None => true,
        Some(Value::Array(list)) => list.is_empty(),
        Some(_) => false,
    }
}

fn model_rules_empty(rules: &Value) -> bool {
    let Some(object) = rules.as_object() else {
        return false;
    };
    const KEYS: [&str; 2] = ["providerModelRules", "manualProviderModelRules"];
    if object.keys().any(|key| !KEYS.contains(&key.as_str())) {
        return false;
    }
    KEYS.iter().all(|key| match object.get(*key) {
        None => true,
        Some(Value::Array(list)) => list.is_empty(),
        Some(_) => false,
    })
}
