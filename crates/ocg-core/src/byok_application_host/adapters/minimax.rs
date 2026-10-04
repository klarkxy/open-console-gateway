use super::{
    ConfigureInput, FormatAdapter, ParsedStatus, default_after_restore, default_plan_defaults,
    display_name, ensure_default_selected, ensure_retained_ocg_default, first_owned, has_image,
    planned_target, preserve_created, restore_default, snapshot,
};
use crate::byok_application::{ByokClient, ByokError, ByokModel, ByokResult};
use crate::byok_application_host::receipt::{ApplyPlan, Receipt};
use crate::model_metadata::PublishedUpstreamProtocol;
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
            return empty_status();
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
            user_changed_owned: minimax_conflict(receipt, true, &owned),
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
        super::validate_models(ByokClient::Minimax, input.models)?;
        let assignments = super::assign_protocols(input.models)?;
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
                "An unowned managed MiniMax provider already exists",
            ));
        }
        if minimax_conflict(
            receipt,
            target_bytes.is_some(),
            &owned_from_mapping(mapping),
        ) {
            return Err(ByokError::conflict(
                "Owned MiniMax fields changed outside OCG",
            ));
        }
        ensure_default_selected(input.default_model_id, input.models)?;
        let model_ids: Vec<String> = input.models.iter().map(|model| model.id.clone()).collect();
        let current_default = string_entry(mapping, "defaultModel");
        let current_public = current_default
            .as_deref()
            .and_then(split_managed_default)
            .map(|(_, model_id)| model_id.to_string());
        let retained = input
            .default_model_id
            .map(str::to_string)
            .or(current_public.clone());
        ensure_retained_ocg_default(retained.as_deref(), &model_ids)?;
        let requested_default = match input.default_model_id {
            Some(model_id) => Some(minimax_default(
                super::routed_provider(&assignments, model_id)?,
                model_id,
            )),
            None => None,
        };
        let migrated_default = if requested_default.is_none() {
            current_default.as_deref().and_then(|value| {
                let (_, model_id) = split_managed_default(value)?;
                if !model_ids.iter().any(|id| id == model_id) {
                    return None;
                }
                let next = minimax_default(
                    super::routed_provider(&assignments, model_id).ok()?,
                    model_id,
                );
                (next != value).then_some(next)
            })
        } else {
            None
        };
        let planned_default = requested_default.clone().or(migrated_default.clone());
        let (baseline_default, last_applied_default, effective_default) = default_plan_defaults(
            receipt,
            planned_default.as_deref(),
            current_default.as_deref(),
        );
        let preference_seed = if input.default_model_id.is_some() {
            captured_preferences(mapping)
        } else {
            Value::Null
        };
        let preferences = first_owned(receipt, &preference_seed);
        write_provider(mapping, &input, &assignments)?;
        if let Some(model_id) = input.default_model_id {
            let next = requested_default.clone().ok_or_else(|| {
                ByokError::invalid("defaultModelId must be one of the selected models")
            })?;
            if current_public.as_deref() != Some(model_id) {
                mapping.remove(yaml_key("defaultModelThinking"));
                mapping.remove(yaml_key("defaultModelContextWindow"));
            }
            mapping.insert(yaml_key("defaultModel"), serde_yaml_ng::Value::String(next));
        } else if let Some(next) = migrated_default {
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
        // Nothing remains to restore once the target file itself is gone.
        let Some(bytes) = target_bytes else {
            return Ok(removal_plan(target_path, receipt));
        };
        let mut root = parse_yaml(bytes)?;
        let mapping = as_mapping_mut(&mut root).ok_or_else(|| {
            ByokError::invalid("Malformed MiniMax configuration cannot be overwritten")
        })?;
        if minimax_conflict(Some(receipt), true, &owned_from_mapping(mapping)) {
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
            for id in super::MANAGED_PROVIDER_IDS {
                custom.remove(yaml_key(id));
            }
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
    let Some(custom) = mapping
        .get(yaml_key("custom_provider"))
        .and_then(as_mapping)
    else {
        return false;
    };
    super::MANAGED_PROVIDER_IDS
        .iter()
        .any(|id| custom.get(yaml_key(id)).is_some())
}

fn minimax_conflict(receipt: Option<&Receipt>, present: bool, current: &Value) -> bool {
    super::ownership_conflict_view(receipt, present, current, minimax_view)
}

fn minimax_view(stripped: &Value) -> Value {
    json!({
        "providers": super::managed_provider_object(stripped),
    })
}

fn string_entry(mapping: &Mapping, key: &str) -> Option<String> {
    mapping
        .get(yaml_key(key))
        .and_then(serde_yaml_ng::Value::as_str)
        .map(str::to_string)
}

fn minimax_default(provider_id: &str, model_id: &str) -> String {
    format!("{CUSTOM_PROVIDER_PREFIX}{provider_id}/{model_id}")
}

/// Longest managed id first, so `ocg-chat` is not read as legacy `ocg`.
fn split_managed_default(value: &str) -> Option<(&str, &str)> {
    let rest = value.strip_prefix(CUSTOM_PROVIDER_PREFIX)?;
    let mut ids = super::MANAGED_PROVIDER_IDS.to_vec();
    ids.sort_by_key(|id| std::cmp::Reverse(id.len()));
    for id in ids {
        let Some(model_id) = rest
            .strip_prefix(id)
            .and_then(|tail| tail.strip_prefix('/'))
        else {
            continue;
        };
        if !model_id.is_empty() {
            return Some((id, model_id));
        }
    }
    None
}

fn ocg_default(mapping: &Mapping) -> Option<String> {
    ocg_model_id(string_entry(mapping, "defaultModel").as_deref()).map(str::to_string)
}

fn ocg_model_id(default_model: Option<&str>) -> Option<&str> {
    split_managed_default(default_model?).map(|(_, model_id)| model_id)
}

fn ocg_model_ids(mapping: &Mapping) -> Vec<String> {
    let Some(custom) = mapping
        .get(yaml_key("custom_provider"))
        .and_then(as_mapping)
    else {
        return Vec::new();
    };
    let mut ids = Vec::new();
    for provider_id in super::MANAGED_PROVIDER_IDS {
        let Some(models) = custom
            .get(yaml_key(provider_id))
            .and_then(as_mapping)
            .and_then(|provider| provider.get(yaml_key("models")))
            .and_then(as_mapping)
        else {
            continue;
        };
        for key in models.keys().filter_map(serde_yaml_ng::Value::as_str) {
            if !ids.iter().any(|id| id == key) {
                ids.push(key.to_string());
            }
        }
    }
    ids
}

fn write_provider(
    mapping: &mut Mapping,
    input: &ConfigureInput<'_>,
    assignments: &[super::ProtocolAssignment<'_>],
) -> ByokResult<()> {
    let saved_models = saved_managed_models(mapping);
    let mut used = Vec::new();
    for assignment in assignments {
        if !used.contains(&assignment.protocol) {
            used.push(assignment.protocol);
        }
    }
    if mapping.get(yaml_key("custom_provider")).is_none() {
        if used.is_empty() {
            return Ok(());
        }
        mapping.insert(
            yaml_key("custom_provider"),
            serde_yaml_ng::Value::Mapping(Mapping::new()),
        );
    }
    let custom = mapping
        .get_mut(yaml_key("custom_provider"))
        .and_then(as_mapping_mut)
        .ok_or_else(|| ByokError::invalid("custom_provider must be a mapping"))?;
    for id in super::MANAGED_PROVIDER_IDS {
        let keep = used
            .iter()
            .any(|protocol| super::provider_id_for(*protocol) == id);
        if !keep {
            custom.remove(yaml_key(id));
        }
    }
    for protocol in used {
        let id = super::provider_id_for(protocol);
        if custom.get(yaml_key(id)).is_none() {
            custom.insert(yaml_key(id), serde_yaml_ng::Value::Mapping(Mapping::new()));
        }
        let provider = custom
            .get_mut(yaml_key(id))
            .and_then(as_mapping_mut)
            .ok_or_else(|| ByokError::invalid("managed custom provider must be a mapping"))?;
        provider.insert(
            yaml_key("name"),
            serde_yaml_ng::Value::String(super::provider_name_for(protocol).into()),
        );
        provider.insert(
            yaml_key("kind"),
            serde_yaml_ng::Value::String("custom".into()),
        );
        provider.insert(yaml_key("enabled"), serde_yaml_ng::Value::Bool(true));
        provider.insert(
            yaml_key("api"),
            serde_yaml_ng::Value::String(minimax_api(protocol).into()),
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
            serde_yaml_ng::Value::String(super::base_for(protocol, input.gateway_v1_url)),
        );
        options.insert(
            yaml_key("authMode"),
            serde_yaml_ng::Value::String("api-key".into()),
        );
        provider.insert(yaml_key("options"), serde_yaml_ng::Value::Mapping(options));
        let mut models = Mapping::new();
        for assignment in assignments
            .iter()
            .filter(|assignment| assignment.protocol == protocol)
        {
            let previous = previous_saved_model(&saved_models, id, &assignment.model.id);
            models.insert(
                yaml_key(&assignment.model.id),
                model_value(assignment.model, protocol, previous),
            );
        }
        provider.insert(yaml_key("models"), serde_yaml_ng::Value::Mapping(models));
    }
    Ok(())
}

fn minimax_api(protocol: PublishedUpstreamProtocol) -> &'static str {
    match protocol {
        PublishedUpstreamProtocol::ChatCompletions => "openai-completions",
        PublishedUpstreamProtocol::Responses => "openai-responses",
        PublishedUpstreamProtocol::Messages => "anthropic-messages",
    }
}

struct SavedModel {
    provider_id: String,
    model_id: String,
    body: Mapping,
}

fn saved_managed_models(mapping: &Mapping) -> Vec<SavedModel> {
    let Some(custom) = mapping
        .get(yaml_key("custom_provider"))
        .and_then(as_mapping)
    else {
        return Vec::new();
    };
    let mut saved = Vec::new();
    for provider_id in super::MANAGED_PROVIDER_IDS {
        let Some(models) = custom
            .get(yaml_key(provider_id))
            .and_then(as_mapping)
            .and_then(|provider| provider.get(yaml_key("models")))
            .and_then(as_mapping)
        else {
            continue;
        };
        let model_ids: Vec<String> = models
            .keys()
            .filter_map(serde_yaml_ng::Value::as_str)
            .map(str::to_string)
            .collect();
        for model_id in model_ids {
            let Some(body) = models.get(yaml_key(&model_id)).and_then(as_mapping) else {
                continue;
            };
            saved.push(SavedModel {
                provider_id: (*provider_id).to_string(),
                model_id,
                body: body.clone(),
            });
        }
    }
    saved
}

fn previous_saved_model<'a>(
    saved: &'a [SavedModel],
    provider_id: &str,
    model_id: &str,
) -> Option<&'a Mapping> {
    saved
        .iter()
        .find(|row| row.provider_id == provider_id && row.model_id == model_id)
        .or_else(|| saved.iter().find(|row| row.model_id == model_id))
        .map(|row| &row.body)
}

fn model_value(
    model: &ByokModel,
    protocol: PublishedUpstreamProtocol,
    previous: Option<&Mapping>,
) -> serde_yaml_ng::Value {
    let mut mapping = previous.cloned().unwrap_or_default();
    mapping.insert(
        yaml_key("name"),
        serde_yaml_ng::Value::String(display_name(model).into()),
    );
    mapping.insert(yaml_key("enabled"), serde_yaml_ng::Value::Bool(true));
    mapping.insert(
        yaml_key("configuration_source"),
        serde_yaml_ng::Value::String("manual".into()),
    );
    sync_limit(&mut mapping, model);
    match model.metadata.tool_calling {
        Some(flag) => {
            mapping.insert(yaml_key("tool_call"), serde_yaml_ng::Value::Bool(flag));
        }
        None => {
            mapping.remove(yaml_key("tool_call"));
        }
    }
    if model.metadata.reasoning == Some(true) {
        mapping.insert(yaml_key("reasoning"), serde_yaml_ng::Value::Bool(true));
    } else {
        mapping.remove(yaml_key("reasoning"));
    }
    // Messages does not receive a Chat effort menu. An old OCG menu is removed
    // when this model moves to a protocol that cannot express it. Unrelated
    // thinking settings stay.
    let wires = if protocol == PublishedUpstreamProtocol::Messages {
        Vec::new()
    } else {
        minimax_effort_options(model)
    };
    sync_effort_options(&mut mapping, &wires);
    if has_image(&model.metadata) {
        mapping.insert(yaml_key("attachment"), serde_yaml_ng::Value::Bool(true));
        let capabilities = yaml_mapping(&mut mapping, "capabilities");
        capabilities.insert(yaml_key("support_image"), serde_yaml_ng::Value::Bool(true));
    } else {
        mapping.remove(yaml_key("attachment"));
        if let Some(capabilities) = mapping
            .get_mut(yaml_key("capabilities"))
            .and_then(as_mapping_mut)
        {
            capabilities.remove(yaml_key("support_image"));
        }
        prune_empty_mapping(&mut mapping, "capabilities");
    }
    serde_yaml_ng::Value::Mapping(mapping)
}

fn sync_limit(mapping: &mut Mapping, model: &ByokModel) {
    let write_context = model.metadata.context_window.is_some();
    let write_output = model.metadata.max_output_tokens.is_some();
    if mapping.get(yaml_key("limit")).is_none() && !write_context && !write_output {
        return;
    }
    if !write_context && !write_output {
        if let Some(limit) = mapping.get_mut(yaml_key("limit")).and_then(as_mapping_mut) {
            limit.remove(yaml_key("context"));
            limit.remove(yaml_key("output"));
        }
        prune_empty_mapping(mapping, "limit");
        return;
    }
    {
        let limit = yaml_mapping(mapping, "limit");
        match model.metadata.context_window {
            Some(context) => {
                limit.insert(
                    yaml_key("context"),
                    serde_yaml_ng::Value::Number(context.into()),
                );
            }
            None => {
                limit.remove(yaml_key("context"));
            }
        }
        match model.metadata.max_output_tokens {
            Some(output) => {
                limit.insert(
                    yaml_key("output"),
                    serde_yaml_ng::Value::Number(output.into()),
                );
            }
            None => {
                limit.remove(yaml_key("output"));
            }
        }
    }
    prune_empty_mapping(mapping, "limit");
}

fn sync_effort_options(mapping: &mut Mapping, wires: &[String]) {
    if wires.is_empty() {
        if let Some(thinking) = mapping
            .get_mut(yaml_key("thinking"))
            .and_then(as_mapping_mut)
        {
            thinking.remove(yaml_key("effortOptions"));
        }
        prune_empty_mapping(mapping, "thinking");
        return;
    }
    let thinking = yaml_mapping(mapping, "thinking");
    thinking.insert(
        yaml_key("effortOptions"),
        serde_yaml_ng::Value::Sequence(
            wires
                .iter()
                .cloned()
                .map(serde_yaml_ng::Value::String)
                .collect(),
        ),
    );
}

fn yaml_mapping<'a>(parent: &'a mut Mapping, key: &str) -> &'a mut Mapping {
    let ykey = yaml_key(key);
    if !matches!(parent.get(&ykey), Some(serde_yaml_ng::Value::Mapping(_))) {
        parent.insert(ykey.clone(), serde_yaml_ng::Value::Mapping(Mapping::new()));
    }
    match parent.get_mut(&ykey) {
        Some(serde_yaml_ng::Value::Mapping(mapping)) => mapping,
        _ => unreachable!("yaml mapping was just inserted"),
    }
}

fn prune_empty_mapping(parent: &mut Mapping, key: &str) {
    let ykey = yaml_key(key);
    let empty = matches!(
        parent.get(&ykey),
        Some(serde_yaml_ng::Value::Mapping(mapping)) if mapping.is_empty()
    );
    if empty {
        parent.remove(ykey);
    }
}

/// Distinct wire spellings the pinned resolver can send unchanged, in
/// selector-key order. Exact `none` stays in the menu and `thinking_config`
/// stays absent: `mode: switchable` without `default_value` makes the catalog
/// variant thinking-off and drops the selected effort. Any other
/// case-insensitive `none` or `off` spelling is rewritten to API `none`, so it
/// is omitted. Other spellings stay exact. Absent and empty maps, and explicit
/// `reasoning: false`, omit the menu. No default is invented.
fn minimax_effort_options(model: &ByokModel) -> Vec<String> {
    if model.metadata.reasoning == Some(false) {
        return Vec::new();
    }
    let Some(efforts) = model.metadata.reasoning_efforts.as_ref() else {
        return Vec::new();
    };
    let mut seen = std::collections::BTreeSet::new();
    let mut wires = Vec::new();
    for spelling in efforts.values() {
        let Some(wire) = preserved_minimax_wire(spelling) else {
            continue;
        };
        if !seen.insert(wire.clone()) {
            continue;
        }
        wires.push(wire);
    }
    wires
}

fn preserved_minimax_wire(spelling: &str) -> Option<String> {
    let trimmed = spelling.trim();
    if trimmed.is_empty() {
        return None;
    }
    match trimmed.to_ascii_lowercase().as_str() {
        "off" => None,
        "none" => (trimmed == "none").then(|| "none".to_string()),
        _ => Some(spelling.to_string()),
    }
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
    let custom = mapping
        .get(yaml_key("custom_provider"))
        .and_then(as_mapping);
    let mut providers = serde_json::Map::new();
    for id in super::MANAGED_PROVIDER_IDS {
        let value = custom
            .and_then(|custom| custom.get(yaml_key(id)))
            .and_then(|value| serde_json::to_value(value).ok())
            .unwrap_or(Value::Null);
        providers.insert((*id).to_string(), value);
    }
    json!({
        "providers": Value::Object(providers),
    })
}
