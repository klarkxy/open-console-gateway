use super::{
    ConfigureInput, FormatAdapter, ParsedStatus, default_after_restore, default_plan_defaults,
    display_name, ensure_default_selected, ensure_retained_ocg_default, first_owned, has_image,
    planned_target, preserve_created, restore_default, snapshot, toml_item_json,
};
use crate::byok_application::{ByokClient, ByokError, ByokModel, ByokResult};
use crate::byok_application_host::receipt::{ApplyPlan, Receipt};
use crate::model_metadata::PublishedUpstreamProtocol;
use serde_json::{Value, json};
use std::path::Path;
use toml_edit::{Array, DocumentMut, Item, Table, value};

pub struct KimiAdapter;

impl FormatAdapter for KimiAdapter {
    fn inspect_bytes(
        &self,
        target_bytes: Option<&[u8]>,
        _catalog_bytes: Option<&[u8]>,
        receipt: Option<&Receipt>,
    ) -> ParsedStatus {
        let Some(bytes) = target_bytes else {
            return empty_status();
        };
        let Ok(doc) = parse_toml(bytes) else {
            return incompatible("Kimi config.toml is not valid TOML");
        };
        if !root_shape_ok(&doc) {
            return incompatible("Kimi config.toml has an unsupported table shape");
        }
        let present = provider_present(&doc);
        let owned = owned_from_doc(&doc);
        ParsedStatus {
            incompatible: None,
            collision: present && receipt.is_none(),
            configured_model_ids: ocg_model_ids(&doc),
            current_default: ocg_default(&doc),
            user_changed_owned: kimi_conflict(receipt, true, &owned),
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
        super::validate_models(ByokClient::Kimi, input.models)?;
        let assignments = super::assign_protocols(input.models)?;
        let mut doc = match target_bytes {
            None => DocumentMut::new(),
            Some(bytes) => parse_toml(bytes)?,
        };
        if target_bytes.is_some() && !root_shape_ok(&doc) {
            return Err(ByokError::invalid(
                "Malformed Kimi configuration cannot be overwritten",
            ));
        }
        if provider_present(&doc) && receipt.is_none() {
            return Err(ByokError::conflict(
                "An unowned managed Kimi provider already exists",
            ));
        }
        if kimi_conflict(receipt, target_bytes.is_some(), &owned_from_doc(&doc)) {
            return Err(ByokError::conflict("Owned Kimi fields changed outside OCG"));
        }
        ensure_default_selected(input.default_model_id, input.models)?;
        let model_ids: Vec<String> = input.models.iter().map(|model| model.id.clone()).collect();
        let current_default = string_key(&doc, "default_model");
        let requested_alias = input.default_model_id.map(alias_for);
        let (baseline_default, last_applied_default, effective_default) = default_plan_defaults(
            receipt,
            requested_alias.as_deref(),
            current_default.as_deref(),
        );
        let resulting = requested_alias.as_deref().or(current_default.as_deref());
        ensure_retained_ocg_default(ocg_model_id(resulting), &model_ids)?;
        write_providers(&mut doc, &assignments, input.gateway_v1_url, input.secret)?;
        sync_models(&mut doc, &assignments)?;
        if let Some(model) = input.default_model_id {
            doc["default_model"] = value(alias_for(model));
        }
        let owned = owned_from_doc(&doc);
        let (created_target, created_catalog) =
            preserve_created(receipt, target_bytes.is_none(), false);
        Ok(ApplyPlan {
            files: vec![planned_target(
                target_path.to_path_buf(),
                Some(doc.to_string().into_bytes()),
            )],
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
        let mut doc = parse_toml(bytes)?;
        if kimi_conflict(Some(receipt), true, &owned_from_doc(&doc)) {
            return Err(ByokError::conflict("Owned Kimi fields changed outside OCG"));
        }
        let current_default = string_key(&doc, "default_model");
        let resulting = default_after_restore(receipt, current_default.as_deref());
        ensure_retained_ocg_default(ocg_model_id(resulting.as_deref()), &[])?;
        if let Some(providers) = doc.get_mut("providers").and_then(Item::as_table_like_mut) {
            for id in super::MANAGED_PROVIDER_IDS {
                providers.remove(id);
            }
        }
        remove_managed_models(&mut doc);
        match restore_default(receipt, current_default.as_deref()) {
            Some(Some(value_str)) => doc["default_model"] = value(value_str),
            Some(None) => {
                doc.remove("default_model");
            }
            None => {}
        }
        let remaining = doc.to_string();
        let target_out = if receipt.created_target && remaining.trim().is_empty() {
            None
        } else {
            Some(remaining.into_bytes())
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

fn parse_toml(bytes: &[u8]) -> ByokResult<DocumentMut> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| ByokError::invalid("Kimi config.toml is not valid UTF-8"))?;
    text.parse::<DocumentMut>()
        .map_err(|_| ByokError::invalid("Kimi config.toml is not valid TOML"))
}

fn root_shape_ok(doc: &DocumentMut) -> bool {
    for key in ["providers", "models"] {
        match doc.get(key) {
            None | Some(Item::None) | Some(Item::Table(_)) => {}
            Some(Item::Value(value)) if value.is_inline_table() => {}
            _ => return false,
        }
    }
    true
}

fn provider_present(doc: &DocumentMut) -> bool {
    super::MANAGED_PROVIDER_IDS.iter().any(|id| {
        doc.get("providers")
            .and_then(|item| item.get(*id))
            .is_some_and(|item| !item.is_none())
    })
}

fn kimi_conflict(receipt: Option<&Receipt>, present: bool, current: &Value) -> bool {
    super::ownership_conflict_view(receipt, present, current, kimi_view)
}

fn kimi_view(stripped: &Value) -> Value {
    let models = match stripped.get("models") {
        Some(models) if models.is_object() => models.clone(),
        _ => json!({}),
    };
    json!({
        "providers": super::managed_provider_object(stripped),
        "models": models,
    })
}

fn alias_for(model_id: &str) -> String {
    format!("{}/{}", super::PROVIDER_ID, model_id)
}

fn string_key(doc: &DocumentMut, key: &str) -> Option<String> {
    doc.get(key).and_then(Item::as_str).map(str::to_string)
}

fn ocg_default(doc: &DocumentMut) -> Option<String> {
    ocg_model_id(string_key(doc, "default_model").as_deref()).map(str::to_string)
}

fn ocg_model_id(default_model: Option<&str>) -> Option<&str> {
    let value = default_model?;
    let prefix = format!("{}/", super::PROVIDER_ID);
    value.strip_prefix(&prefix)
}

fn ocg_model_ids(doc: &DocumentMut) -> Vec<String> {
    let Some(models) = doc.get("models").and_then(Item::as_table) else {
        return Vec::new();
    };
    let mut ids = Vec::new();
    for (key, item) in models.iter() {
        let Some(table) = item.as_table() else {
            continue;
        };
        if !table
            .get("provider")
            .and_then(Item::as_str)
            .is_some_and(super::is_managed_provider)
        {
            continue;
        }
        if let Some(id) = table.get("model").and_then(Item::as_str) {
            ids.push(id.to_string());
        } else if let Some(id) = key.strip_prefix(&format!("{}/", super::PROVIDER_ID)) {
            ids.push(id.to_string());
        }
    }
    ids
}

fn ensure_table<'a>(doc: &'a mut DocumentMut, key: &str) -> ByokResult<&'a mut Table> {
    if doc.get(key).is_none() {
        doc[key] = Item::Table(Table::new());
    }
    doc.get_mut(key)
        .and_then(Item::as_table_mut)
        .ok_or_else(|| ByokError::invalid("Kimi configuration table is not a mapping"))
}

fn write_providers(
    doc: &mut DocumentMut,
    assignments: &[super::ProtocolAssignment<'_>],
    gateway: &str,
    secret: &str,
) -> ByokResult<()> {
    let mut used = Vec::new();
    for assignment in assignments {
        if !used.contains(&assignment.protocol) {
            used.push(assignment.protocol);
        }
    }
    if let Some(providers) = doc.get_mut("providers").and_then(Item::as_table_mut) {
        let stale: Vec<String> = providers
            .iter()
            .filter_map(|(key, item)| {
                if item.is_none() || !super::is_managed_provider(key) {
                    return None;
                }
                let keep = used
                    .iter()
                    .any(|protocol| super::provider_id_for(*protocol) == key);
                (!keep).then_some(key.to_string())
            })
            .collect();
        for key in stale {
            providers.remove(&key);
        }
    }
    for protocol in used {
        upsert_provider(doc, protocol, gateway, secret)?;
    }
    Ok(())
}

fn upsert_provider(
    doc: &mut DocumentMut,
    protocol: PublishedUpstreamProtocol,
    gateway: &str,
    secret: &str,
) -> ByokResult<()> {
    let id = super::provider_id_for(protocol);
    let providers = ensure_table(doc, "providers")?;
    if providers.get(id).is_none() {
        providers.insert(id, Item::Table(Table::new()));
    }
    let provider = providers
        .get_mut(id)
        .and_then(Item::as_table_mut)
        .ok_or_else(|| ByokError::invalid("Kimi managed provider must be a table"))?;
    provider["type"] = value(kimi_provider_type(protocol));
    provider["base_url"] = value(super::base_for(protocol, gateway));
    provider["api_key"] = value(secret);
    if provider.get("name").is_none() {
        provider["name"] = value(super::provider_name_for(protocol));
    }
    Ok(())
}

fn kimi_provider_type(protocol: PublishedUpstreamProtocol) -> &'static str {
    match protocol {
        PublishedUpstreamProtocol::ChatCompletions => "openai",
        PublishedUpstreamProtocol::Responses => "openai_responses",
        PublishedUpstreamProtocol::Messages => "anthropic",
    }
}

fn sync_models(
    doc: &mut DocumentMut,
    assignments: &[super::ProtocolAssignment<'_>],
) -> ByokResult<()> {
    let wanted: Vec<String> = assignments
        .iter()
        .map(|assignment| alias_for(&assignment.model.id))
        .collect();
    {
        let table = ensure_table(doc, "models")?;
        for alias in &wanted {
            if let Some(item) = table.get(alias.as_str()) {
                let provider = item
                    .as_table()
                    .and_then(|table| table.get("provider"))
                    .and_then(Item::as_str);
                if !provider.is_some_and(super::is_managed_provider) {
                    return Err(ByokError::conflict(
                        "A model alias in the ocg namespace is not owned by OCG",
                    ));
                }
            }
        }
    }
    {
        let table = ensure_table(doc, "models")?;
        let stale: Vec<String> = table
            .iter()
            .filter_map(|(key, item)| {
                let provider = item.as_table()?.get("provider")?.as_str()?;
                (super::is_managed_provider(provider) && !wanted.iter().any(|alias| alias == key))
                    .then_some(key.to_string())
            })
            .collect();
        for key in stale {
            table.remove(&key);
        }
    }
    for assignment in assignments {
        let alias = alias_for(&assignment.model.id);
        let models_table = ensure_table(doc, "models")?;
        if models_table.get(&alias).is_none() {
            models_table.insert(&alias, Item::Table(Table::new()));
        }
        let entry = models_table
            .get_mut(&alias)
            .and_then(Item::as_table_mut)
            .ok_or_else(|| ByokError::invalid("Kimi model entry must be a table"))?;
        entry["provider"] = value(super::provider_id_for(assignment.protocol));
        entry["model"] = value(assignment.model.id.as_str());
        match assignment.model.metadata.context_window {
            Some(context) => entry["max_context_size"] = value(context as i64),
            None => {
                entry.remove("max_context_size");
            }
        }
        entry["display_name"] = value(display_name(assignment.model));
        match kimi_capabilities(assignment.model) {
            Some(capabilities) => {
                let mut array = Array::new();
                for capability in capabilities {
                    array.push(capability);
                }
                entry["capabilities"] = Item::Value(array.into());
            }
            None => {
                entry.remove("capabilities");
            }
        }
        sync_reasoning_tiers(entry, assignment.model, assignment.protocol);
    }
    Ok(())
}

fn remove_managed_models(doc: &mut DocumentMut) {
    let Some(table) = doc.get_mut("models").and_then(Item::as_table_mut) else {
        return;
    };
    let keys: Vec<String> = table
        .iter()
        .filter_map(|(key, item)| {
            let provider = item.as_table()?.get("provider")?.as_str()?;
            super::is_managed_provider(provider).then_some(key.to_string())
        })
        .collect();
    for key in keys {
        table.remove(&key);
    }
}

fn kimi_capabilities(model: &ByokModel) -> Option<Vec<&'static str>> {
    let mut caps = Vec::new();
    let mut declared_tools = false;
    match model.metadata.tool_calling {
        Some(true) => {
            declared_tools = true;
            caps.push("tool_use");
        }
        Some(false) => declared_tools = true,
        None => {}
    }
    if declares_thinking(model) {
        caps.push("thinking");
    }
    if has_image(&model.metadata) {
        caps.push("image_in");
    }
    if declared_tools || !caps.is_empty() {
        Some(caps)
    } else {
        None
    }
}

struct KimiTiers {
    support: Vec<String>,
    off: Option<String>,
}

/// Selector key `off` is stored in `off_effort` with its declared spelling.
/// Every other spelling is advertised only when the native selector would send
/// that same string: it trims and lowercases the choice, `on` adds no
/// parameter, and `off` is the disable path. Absent and empty maps omit both,
/// and explicit `reasoning: false` suppresses them. No default is invented.
fn kimi_tiers(model: &ByokModel) -> KimiTiers {
    if model.metadata.reasoning == Some(false) {
        return KimiTiers {
            support: Vec::new(),
            off: None,
        };
    }
    let Some(efforts) = model.metadata.reasoning_efforts.as_ref() else {
        return KimiTiers {
            support: Vec::new(),
            off: None,
        };
    };
    let off = efforts
        .get("off")
        .filter(|spelling| !spelling.trim().is_empty())
        .cloned();
    let mut seen = std::collections::BTreeSet::new();
    if let Some(spelling) = &off {
        seen.insert(spelling.clone());
    }
    let mut support = Vec::new();
    for spelling in efforts.values() {
        let Some(wire) = preserved_kimi_support_wire(spelling) else {
            continue;
        };
        if !seen.insert(wire.to_string()) {
            continue;
        }
        support.push(wire.to_string());
    }
    KimiTiers { support, off }
}

fn preserved_kimi_support_wire(spelling: &str) -> Option<&str> {
    let normalized = spelling.trim().to_lowercase();
    if normalized.is_empty() || spelling != normalized || normalized == "on" || normalized == "off"
    {
        return None;
    }
    Some(spelling)
}

fn declares_thinking(model: &ByokModel) -> bool {
    model.metadata.reasoning == Some(true)
}

fn sync_reasoning_tiers(entry: &mut Table, model: &ByokModel, protocol: PublishedUpstreamProtocol) {
    entry.remove("default_effort");
    entry.remove("adaptive_thinking");
    // Messages thinking budgets and adaptive controls are a different API.
    // Chat wire spellings stay on Chat and Responses, where the gateway copies
    // the same effort string. An omitted menu is support without choices.
    if protocol == PublishedUpstreamProtocol::Messages {
        entry.remove("support_efforts");
        entry.remove("off_effort");
        return;
    }
    let tiers = kimi_tiers(model);
    if tiers.support.is_empty() {
        entry.remove("support_efforts");
    } else {
        let mut array = Array::new();
        for spelling in &tiers.support {
            array.push(spelling.as_str());
        }
        entry["support_efforts"] = Item::Value(array.into());
    }
    match tiers.off.as_deref() {
        Some(spelling) => entry["off_effort"] = value(spelling),
        None => {
            entry.remove("off_effort");
        }
    }
}

fn owned_from_doc(doc: &DocumentMut) -> Value {
    let mut providers = serde_json::Map::new();
    for id in super::MANAGED_PROVIDER_IDS {
        let value = doc
            .get("providers")
            .and_then(|item| item.get(id))
            .filter(|item| !item.is_none())
            .map(toml_item_json)
            .unwrap_or(Value::Null);
        providers.insert((*id).to_string(), value);
    }
    let mut models = serde_json::Map::new();
    if let Some(table) = doc.get("models").and_then(Item::as_table_like) {
        for (key, item) in table.iter() {
            let provider = item
                .as_table_like()
                .and_then(|entry| entry.get("provider"))
                .and_then(Item::as_str);
            if !provider.is_some_and(super::is_managed_provider) {
                continue;
            }
            models.insert(key.to_string(), toml_item_json(item));
        }
    }
    json!({
        "providers": Value::Object(providers),
        "models": models,
    })
}
