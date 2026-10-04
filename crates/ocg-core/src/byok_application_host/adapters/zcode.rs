use super::{
    ConfigureInput, FormatAdapter, ParsedStatus, default_after_restore, default_plan_defaults,
    ensure_default_selected, ensure_retained_ocg_default, first_owned, ownership_conflict,
    planned_target, preserve_created, restore_default, snapshot,
};
use crate::byok_application::{ByokClient, ByokError, ByokModel, ByokResult};
use crate::byok_application_host::receipt::{ApplyPlan, Receipt};
use crate::model_metadata::PublishedUpstreamProtocol;
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
            return empty_status();
        };
        let Ok(root) = parse_json(bytes) else {
            return incompatible("ZCode provider_config.json is not valid JSON");
        };
        if let Some(reason) = unsupported_schema(&root) {
            return incompatible(&reason);
        }
        let present = managed_provider_present(&root);
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
        super::validate_models(ByokClient::Zcode, input.models)?;
        let assignments = super::assign_protocols(input.models)?;
        let mut root = match target_bytes {
            None => empty_document(),
            Some(bytes) => parse_json(bytes)?,
        };
        if target_bytes.is_some()
            && let Some(reason) = unsupported_schema(&root)
        {
            return Err(ByokError::invalid(reason));
        }
        if managed_provider_present(&root) && receipt.is_none() {
            return Err(ByokError::conflict(
                "An unowned managed ZCode provider already exists",
            ));
        }
        if ownership_conflict(receipt, target_bytes.is_some(), &owned_from_root(&root)) {
            return Err(ByokError::conflict(
                "Owned ZCode fields changed outside OCG",
            ));
        }
        ensure_default_selected(input.default_model_id, input.models)?;
        let model_ids: Vec<String> = input.models.iter().map(|model| model.id.clone()).collect();
        let current_default = selection_string(&root);
        let requested = match input.default_model_id {
            Some(model_id) => Some(selection_value(
                super::routed_provider(&assignments, model_id)?,
                model_id,
            )),
            None => None,
        };
        let migrated = if requested.is_none() {
            migrated_selection(current_default.as_deref(), &assignments, &model_ids)
        } else {
            None
        };
        let planned = requested.clone().or(migrated.clone());
        let planned_text = planned.as_ref().map(ToString::to_string);
        let (baseline_default, last_applied_default, effective_default) =
            default_plan_defaults(receipt, planned_text.as_deref(), current_default.as_deref());
        match input.default_model_id {
            Some(id) => ensure_retained_ocg_default(Some(id), &model_ids)?,
            None => {
                if let Some(id) = ocg_selection_id(current_default.as_deref()) {
                    ensure_retained_ocg_default(Some(&id), &model_ids)?;
                }
            }
        }
        reject_manual_rules(&root, &assignments)?;
        write_provider(&mut root, &input, &assignments)?;
        if let Some(selection) = planned {
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
        // Nothing remains to restore once the target file itself is gone.
        let Some(bytes) = target_bytes else {
            return Ok(removal_plan(target_path, receipt));
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
        let no_assignments: &[super::ProtocolAssignment<'_>] = &[];
        reject_manual_rules(&root, no_assignments)?;
        remove_managed(&mut root);
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
        Ok(removal_plan_with(target_path, receipt, target_out))
    }
}

fn removal_plan(target_path: &Path, receipt: &Receipt) -> ApplyPlan {
    removal_plan_with(target_path, receipt, None)
}

fn removal_plan_with(
    target_path: &Path,
    receipt: &Receipt,
    target_out: Option<Vec<u8>>,
) -> ApplyPlan {
    ApplyPlan {
        files: vec![planned_target(target_path.to_path_buf(), target_out)],
        created_target: receipt.created_target,
        created_catalog: false,
        baseline_default: receipt.baseline_default.clone(),
        last_applied_default: None,
        managed: snapshot(Vec::new(), Value::Null, None),
        first_owned: receipt.first_owned.clone(),
    }
}

fn empty_status() -> ParsedStatus {
    ParsedStatus {
        incompatible: None,
        collision: false,
        configured_model_ids: Vec::new(),
        current_default: None,
        user_changed_owned: false,
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

fn managed_provider_present(root: &Value) -> bool {
    provider_rules(root).is_some_and(|rules| {
        rules.iter().any(|rule| {
            rule.get("providerId")
                .and_then(Value::as_str)
                .is_some_and(super::is_managed_provider)
        })
    })
}

fn ocg_model_ids(root: &Value) -> Vec<String> {
    let Some(rules) = provider_rules(root) else {
        return Vec::new();
    };
    let mut ids = Vec::new();
    for rule in rules {
        if !rule
            .get("providerId")
            .and_then(Value::as_str)
            .is_some_and(super::is_managed_provider)
        {
            continue;
        }
        let Some(list) = rule
            .get("config")
            .and_then(|config| config.get("personalModelIds"))
            .and_then(Value::as_array)
        else {
            continue;
        };
        for id in list.iter().filter_map(Value::as_str) {
            if !ids.iter().any(|kept| kept == id) {
                ids.push(id.to_string());
            }
        }
    }
    ids
}

fn ocg_default(root: &Value) -> Option<String> {
    let selection = config(root)?.get("defaultModelSelection")?;
    let provider = selection.get("providerId").and_then(Value::as_str)?;
    if !super::is_managed_provider(provider) {
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
    let provider = value.get("providerId").and_then(Value::as_str)?;
    if !super::is_managed_provider(provider) {
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

fn selection_value(provider_id: &str, model_id: &str) -> Value {
    json!({
        "providerId": provider_id,
        "modelId": model_id
    })
}

fn migrated_selection(
    current: Option<&str>,
    assignments: &[super::ProtocolAssignment<'_>],
    model_ids: &[String],
) -> Option<Value> {
    let current_text = current?;
    let value: Value = serde_json::from_str(current_text).ok()?;
    let provider = value.get("providerId").and_then(Value::as_str)?;
    if !super::is_managed_provider(provider) {
        return None;
    }
    let model_id = value.get("modelId").and_then(Value::as_str)?;
    if !model_ids.iter().any(|id| id == model_id) {
        return None;
    }
    let next_provider = super::routed_provider(assignments, model_id).ok()?;
    if provider == next_provider {
        return None;
    }
    Some(selection_value(next_provider, model_id))
}

fn set_selection(root: &mut Value, selection: Value) {
    if let Some(config) = config_mut(root) {
        config.insert("defaultModelSelection".into(), selection);
    }
}

fn write_provider(
    root: &mut Value,
    input: &ConfigureInput<'_>,
    assignments: &[super::ProtocolAssignment<'_>],
) -> ByokResult<()> {
    if root.get("schemaVersion").is_none() {
        *root = empty_document();
    }
    let mut used = Vec::new();
    for assignment in assignments {
        if !used.contains(&assignment.protocol) {
            used.push(assignment.protocol);
        }
    }
    // Appended provider ids follow Chat, Responses, Messages. Existing rows stay put.
    used.sort();
    {
        let rules = provider_rules_mut(root)?;
        rules.retain(|rule| {
            let Some(id) = rule.get("providerId").and_then(Value::as_str) else {
                return true;
            };
            if !super::is_managed_provider(id) {
                return true;
            }
            used.iter()
                .any(|protocol| super::provider_id_for(*protocol) == id)
        });
        for protocol in &used {
            let ids: Vec<Value> = assignments
                .iter()
                .filter(|assignment| assignment.protocol == *protocol)
                .map(|assignment| Value::String(assignment.model.id.clone()))
                .collect();
            let order = ids.clone();
            let id = super::provider_id_for(*protocol);
            let rule = json!({
                "providerId": id,
                "providerName": super::provider_name_for(*protocol),
                "enabled": true,
                "config": {
                    "group": "standard-personal",
                    "access": { "type": "api-key", "apiKey": input.secret },
                    "api": {
                        "type": zcode_api_type(*protocol),
                        "baseUrl": super::base_for(*protocol, input.gateway_v1_url)
                    },
                    "personalModelIds": ids,
                    "modelOrder": order
                }
            });
            match rules
                .iter()
                .position(|item| item.get("providerId").and_then(Value::as_str) == Some(id))
            {
                Some(index) => merge_rule(&mut rules[index], rule),
                None => rules.push(rule),
            }
        }
    }
    sync_order(root, &used);
    write_model_rules(root, assignments)?;
    Ok(())
}

fn zcode_api_type(protocol: PublishedUpstreamProtocol) -> &'static str {
    match protocol {
        PublishedUpstreamProtocol::ChatCompletions => "openai-chat-completions",
        PublishedUpstreamProtocol::Responses => "openai-responses",
        PublishedUpstreamProtocol::Messages => "anthropic-messages",
    }
}

fn merge_rule(existing: &mut Value, next: Value) {
    let (Some(dst), Some(src)) = (existing.as_object_mut(), next.as_object()) else {
        return;
    };
    for (key, value) in src {
        if key == "config" {
            merge_config(dst, value);
        } else {
            dst.insert(key.clone(), value.clone());
        }
    }
}

/// Same provider identity keeps unknown nested `api` and `access` keys.
/// Known keys are overwritten. A legacy provider split does not use this path:
/// that row is replaced by a new id, so its extras are not reassigned.
fn merge_config(dst: &mut Map<String, Value>, value: &Value) {
    let Some(src_config) = value.as_object() else {
        dst.insert("config".into(), value.clone());
        return;
    };
    let dst_config = dst.entry("config").or_insert_with(|| json!({}));
    if !dst_config.is_object() {
        *dst_config = json!({});
    }
    let Some(dst_config) = dst_config.as_object_mut() else {
        return;
    };
    for (key, incoming) in src_config {
        if key == "api" || key == "access" {
            merge_nested_known(dst_config, key, incoming);
        } else {
            dst_config.insert(key.clone(), incoming.clone());
        }
    }
}

fn merge_nested_known(dst_config: &mut Map<String, Value>, key: &str, incoming: &Value) {
    let known: &[&str] = if key == "api" {
        &["type", "baseUrl"]
    } else {
        &["type", "apiKey"]
    };
    let Some(src) = incoming.as_object() else {
        dst_config.insert(key.to_string(), incoming.clone());
        return;
    };
    match dst_config.get_mut(key) {
        Some(Value::Object(existing)) => {
            for name in known {
                if let Some(value) = src.get(*name) {
                    existing.insert((*name).to_string(), value.clone());
                }
            }
        }
        _ => {
            dst_config.insert(key.to_string(), incoming.clone());
        }
    }
}

fn sync_order(root: &mut Value, used: &[PublishedUpstreamProtocol]) {
    let Some(config) = config_mut(root) else {
        return;
    };
    let order = config.entry("providerOrder").or_insert_with(|| json!([]));
    let Some(list) = order.as_array_mut() else {
        return;
    };
    let kept: Vec<&str> = used
        .iter()
        .map(|protocol| super::provider_id_for(*protocol))
        .collect();
    list.retain(|item| match item.as_str() {
        Some(id) if super::is_managed_provider(id) => kept.contains(&id),
        _ => true,
    });
    for id in kept {
        if !list.iter().any(|item| item.as_str() == Some(id)) {
            list.push(Value::String(id.into()));
        }
    }
}

fn write_model_rules(
    root: &mut Value,
    assignments: &[super::ProtocolAssignment<'_>],
) -> ByokResult<()> {
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
    let previous: Vec<Value> = list
        .iter()
        .filter(|rule| {
            rule.get("providerId")
                .and_then(Value::as_str)
                .is_some_and(super::is_managed_provider)
        })
        .cloned()
        .collect();
    list.retain(|rule| {
        !rule
            .get("providerId")
            .and_then(Value::as_str)
            .is_some_and(super::is_managed_provider)
    });
    for assignment in assignments {
        let provider_id = super::provider_id_for(assignment.protocol);
        list.push(merge_managed_model_rule(
            previous_model_rule(&previous, &assignment.model.id, provider_id),
            assignment.model,
            assignment.protocol,
        ));
    }
    Ok(())
}

fn previous_model_rule<'a>(
    rules: &'a [Value],
    model_id: &str,
    provider_id: &str,
) -> Option<&'a Value> {
    rules
        .iter()
        .find(|rule| model_rule_ids(rule) == Some((provider_id, model_id)))
        .or_else(|| {
            rules
                .iter()
                .find(|rule| rule.get("modelId").and_then(Value::as_str) == Some(model_id))
        })
}

fn model_rule_ids(rule: &Value) -> Option<(&str, &str)> {
    Some((
        rule.get("providerId")?.as_str()?,
        rule.get("modelId")?.as_str()?,
    ))
}

/// Keep unknown fields on the same model id, including when that model moves
/// to another managed provider. Overwrite only fields this generator owns.
/// A protocol that cannot express the reasoning menu drops `values` and `map`
/// only. Other keys inside that object stay, and the object goes away when
/// nothing else remains.
fn merge_managed_model_rule(
    previous: Option<&Value>,
    model: &ByokModel,
    protocol: PublishedUpstreamProtocol,
) -> Value {
    let mut rule = previous
        .filter(|value| value.is_object())
        .cloned()
        .unwrap_or_else(|| json!({}));
    rule["providerId"] = json!(super::provider_id_for(protocol));
    rule["modelId"] = json!(model.id.as_str());
    let mut config = object_or_empty(rule.get("config"));
    config["enabled"] = json!(true);
    let mut properties = object_or_empty(config.get("properties"));
    match model.metadata.context_window {
        Some(context) => properties["contextWindow"] = json!(context),
        None => {
            properties
                .as_object_mut()
                .expect("properties")
                .remove("contextWindow");
        }
    }
    match model.metadata.tool_calling {
        Some(flag) => properties["supportsToolCall"] = json!(flag),
        None => {
            properties
                .as_object_mut()
                .expect("properties")
                .remove("supportsToolCall");
        }
    }
    match &model.metadata.input_modalities {
        Some(modalities) => {
            let mut input = object_or_empty(properties.get("inputFormat"));
            input["supportsImage"] = json!(modalities.iter().any(|item| item == "image"));
            properties["inputFormat"] = input;
        }
        None => {
            if let Some(input) = properties
                .get_mut("inputFormat")
                .and_then(Value::as_object_mut)
            {
                input.remove("supportsImage");
            }
            if properties
                .get("inputFormat")
                .and_then(Value::as_object)
                .is_some_and(Map::is_empty)
            {
                properties
                    .as_object_mut()
                    .expect("properties")
                    .remove("inputFormat");
            }
        }
    }
    let mut option_specs = object_or_empty(config.get("optionSpecs"));
    match model.metadata.max_output_tokens {
        Some(max) => {
            let mut slot = object_or_empty(option_specs.get("maxOutputTokens"));
            slot["max"] = json!(max);
            option_specs["maxOutputTokens"] = slot;
        }
        None => {
            if let Some(slot) = option_specs
                .get_mut("maxOutputTokens")
                .and_then(Value::as_object_mut)
            {
                slot.remove("max");
            }
            if option_specs
                .get("maxOutputTokens")
                .and_then(Value::as_object)
                .is_some_and(Map::is_empty)
            {
                option_specs
                    .as_object_mut()
                    .expect("optionSpecs")
                    .remove("maxOutputTokens");
            }
        }
    }
    let wires = if protocol == PublishedUpstreamProtocol::ChatCompletions
        && model.metadata.reasoning != Some(false)
    {
        reasoning_wires(model.metadata.reasoning_efforts.as_ref())
    } else {
        Vec::new()
    };
    if wires.is_empty() {
        if let Some(level) = option_specs
            .get_mut("reasoningLevel")
            .and_then(Value::as_object_mut)
        {
            level.remove("values");
            level.remove("map");
        }
        if option_specs
            .get("reasoningLevel")
            .and_then(Value::as_object)
            .is_some_and(Map::is_empty)
        {
            option_specs
                .as_object_mut()
                .expect("optionSpecs")
                .remove("reasoningLevel");
        }
    } else {
        let mut level = object_or_empty(option_specs.get("reasoningLevel"));
        level["values"] = json!(wires);
        level["map"] = json!("{\"reasoning_effort\": reasoningLevel}");
        option_specs["reasoningLevel"] = level;
    }
    config["properties"] = properties;
    config["optionSpecs"] = option_specs;
    rule["config"] = config;
    rule
}

fn object_or_empty(value: Option<&Value>) -> Value {
    value
        .filter(|value| value.is_object())
        .cloned()
        .unwrap_or_else(|| json!({}))
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

fn reject_manual_rules(
    root: &Value,
    assignments: &[super::ProtocolAssignment<'_>],
) -> ByokResult<()> {
    let mut planned = Vec::new();
    for assignment in assignments {
        let id = super::provider_id_for(assignment.protocol);
        if !planned.contains(&id) {
            planned.push(id);
        }
    }
    for (provider, model) in manual_identities(root) {
        if assignments.iter().any(|assignment| {
            super::provider_id_for(assignment.protocol) == provider && assignment.model.id == model
        }) {
            return Err(ByokError::conflict(
                "A manual ZCode model rule uses the same provider and model as a managed rule",
            ));
        }
        if super::is_managed_provider(provider)
            && provider_rule_exists(root, provider)
            && !planned.contains(&provider)
        {
            return Err(ByokError::conflict(
                "A manual ZCode model rule still references a managed provider that would be removed",
            ));
        }
    }
    Ok(())
}

fn manual_identities(root: &Value) -> Vec<(&str, &str)> {
    config(root)
        .and_then(|config| config.get("modelConfigRules"))
        .and_then(|rules| rules.get("manualProviderModelRules"))
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|rule| model_rule_ids(rule))
                .collect()
        })
        .unwrap_or_default()
}

fn provider_rule_exists(root: &Value, provider_id: &str) -> bool {
    provider_rules(root).is_some_and(|rules| {
        rules
            .iter()
            .any(|rule| rule.get("providerId").and_then(Value::as_str) == Some(provider_id))
    })
}

fn remove_managed(root: &mut Value) {
    if let Ok(rules) = provider_rules_mut(root) {
        rules.retain(|rule| {
            !rule
                .get("providerId")
                .and_then(Value::as_str)
                .is_some_and(super::is_managed_provider)
        });
    }
    if let Some(config) = config_mut(root) {
        if let Some(order) = config
            .get_mut("providerOrder")
            .and_then(Value::as_array_mut)
        {
            order.retain(|item| !item.as_str().is_some_and(super::is_managed_provider));
        }
        if let Some(rules) = config
            .get_mut("modelConfigRules")
            .and_then(Value::as_object_mut)
            && let Some(list) = rules
                .get_mut("providerModelRules")
                .and_then(Value::as_array_mut)
        {
            list.retain(|rule| {
                !rule
                    .get("providerId")
                    .and_then(Value::as_str)
                    .is_some_and(super::is_managed_provider)
            });
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
                    .filter(|rule| {
                        rule.get("providerId")
                            .and_then(Value::as_str)
                            .is_some_and(super::is_managed_provider)
                    })
                    .cloned()
                    .collect::<Vec<_>>()
            }),
    })
}

fn ocg_provider_rules(root: &Value) -> Vec<Value> {
    provider_rules(root)
        .map(|list| {
            list.iter()
                .filter(|rule| {
                    rule.get("providerId")
                        .and_then(Value::as_str)
                        .is_some_and(super::is_managed_provider)
                })
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
                .filter(|item| item.as_str().is_some_and(super::is_managed_provider))
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
