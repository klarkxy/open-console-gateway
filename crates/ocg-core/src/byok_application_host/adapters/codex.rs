use super::super::fs::content_hash;
use super::{
    ConfigureInput, FormatAdapter, PROVIDER_ID, PROVIDER_NAME, ParsedStatus, default_plan_defaults,
    display_name, ensure_default_selected, ensure_retained_ocg_default, first_owned, has_image,
    ownership_conflict, planned_catalog, planned_target, preserve_created, snapshot,
    toml_item_json,
};
use crate::byok_application::{ByokError, ByokModel, ByokResult};
use crate::byok_application_host::receipt::{ApplyPlan, Receipt};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, Item, Table, value};

pub struct CodexAdapter;

impl FormatAdapter for CodexAdapter {
    fn inspect_bytes(
        &self,
        target_bytes: Option<&[u8]>,
        catalog_bytes: Option<&[u8]>,
        receipt: Option<&Receipt>,
    ) -> ParsedStatus {
        let Some(bytes) = target_bytes else {
            return ParsedStatus {
                incompatible: None,
                collision: catalog_unowned(catalog_bytes, receipt),
                configured_model_ids: Vec::new(),
                current_default: None,
                user_changed_owned: false,
            };
        };
        let Ok(doc) = parse_toml(bytes) else {
            return ParsedStatus {
                incompatible: Some("Codex config.toml is not valid TOML".into()),
                collision: false,
                configured_model_ids: Vec::new(),
                current_default: None,
                user_changed_owned: false,
            };
        };
        if !root_shape_ok(&doc) {
            return ParsedStatus {
                incompatible: Some("Codex config.toml has an unsupported table shape".into()),
                collision: false,
                configured_model_ids: Vec::new(),
                current_default: None,
                user_changed_owned: false,
            };
        }
        let present = provider_present(&doc);
        let owned = owned_from_doc(&doc, catalog_bytes);
        let collision = present && receipt.is_none();
        let catalog_collision = catalog_unowned(catalog_bytes, receipt);
        ParsedStatus {
            incompatible: None,
            collision: collision || catalog_collision,
            configured_model_ids: receipt
                .map(|r| r.last_managed.model_ids.clone())
                .filter(|_| present)
                .unwrap_or_default(),
            current_default: ocg_default(&doc),
            user_changed_owned: ownership_conflict(receipt, true, &owned),
        }
    }

    fn configure(
        &self,
        target_path: &Path,
        catalog_path: Option<&Path>,
        target_bytes: Option<&[u8]>,
        catalog_bytes: Option<&[u8]>,
        receipt: Option<&Receipt>,
        input: ConfigureInput<'_>,
    ) -> ByokResult<ApplyPlan> {
        let catalog_path = catalog_path.ok_or_else(|| {
            ByokError::internal("Codex catalog path was not derived from the target")
        })?;
        if target_bytes.is_none() && receipt.is_some() && catalog_bytes.is_some() {
            return Err(ByokError::conflict(
                "Owned Codex fields changed outside OCG",
            ));
        }
        let mut doc = match target_bytes {
            None => DocumentMut::new(),
            Some(bytes) => parse_toml(bytes)?,
        };
        if target_bytes.is_some() && !root_shape_ok(&doc) {
            return Err(ByokError::invalid(
                "Malformed Codex configuration cannot be overwritten",
            ));
        }
        if provider_present(&doc) && receipt.is_none() {
            return Err(ByokError::conflict(
                "An unowned ocg Codex provider already exists",
            ));
        }
        if catalog_unowned(catalog_bytes, receipt) {
            return Err(ByokError::conflict(
                "An unowned Codex model catalog already exists",
            ));
        }
        if ownership_conflict(
            receipt,
            target_bytes.is_some(),
            &owned_from_doc(&doc, catalog_bytes),
        ) {
            return Err(ByokError::conflict(
                "Owned Codex fields changed outside OCG",
            ));
        }
        ensure_default_selected(input.default_model_id, input.models)?;
        let model_ids: Vec<String> = input.models.iter().map(|model| model.id.clone()).collect();
        reject_dangling_selection(&doc, input.default_model_id, &model_ids)?;
        let original_model = string_key(&doc, "model");
        let (baseline_default, last_applied_default, effective_default) =
            default_plan_defaults(receipt, input.default_model_id, original_model.as_deref());
        // Selection baseline is recorded only when this call activates it.
        // A stored null means preparation has not claimed model, provider, or catalog.
        let selection_seed = if input.default_model_id.is_some() {
            selection_baseline(&doc, catalog_path)
        } else {
            Value::Null
        };
        let baseline = first_owned(receipt, &selection_seed);
        write_provider(&mut doc, input.gateway_v1_url, input.secret)?;
        if let Some(model) = input.default_model_id {
            activate_selection(&mut doc, model, catalog_path);
        }
        let catalog = build_catalog(input.models);
        let next_catalog = serde_json::to_vec_pretty(&catalog)
            .map_err(|_| ByokError::internal("failed to encode Codex model catalog"))?;
        let owned = owned_from_doc(&doc, Some(next_catalog.as_slice()));
        let (created_target, created_catalog) =
            preserve_created(receipt, target_bytes.is_none(), catalog_bytes.is_none());
        Ok(ApplyPlan {
            files: vec![
                planned_target(
                    target_path.to_path_buf(),
                    Some(doc.to_string().into_bytes()),
                ),
                planned_catalog(catalog_path.to_path_buf(), Some(next_catalog)),
            ],
            created_target,
            created_catalog,
            baseline_default,
            last_applied_default,
            managed: snapshot(model_ids, owned.clone(), effective_default),
            first_owned: baseline,
        })
    }

    fn remove(
        &self,
        target_path: &Path,
        catalog_path: Option<&Path>,
        target_bytes: Option<&[u8]>,
        catalog_bytes: Option<&[u8]>,
        receipt: &Receipt,
    ) -> ByokResult<ApplyPlan> {
        let catalog_path = catalog_path.ok_or_else(|| {
            ByokError::internal("Codex catalog path was not derived from the target")
        })?;
        // The target file is already gone, so there is nothing to restore and
        // no owned bytes left to compare. A present file still goes through the
        // ownership check below: a deleted provider entry with a rewritten
        // catalog is an external edit and must conflict. Retire the catalog the
        // same way a present-but-empty removal would.
        let Some(bytes) = target_bytes else {
            if catalog_bytes.is_some() {
                return Err(ByokError::conflict(
                    "Owned Codex fields changed outside OCG",
                ));
            }
            return Ok(removal_plan(target_path, catalog_path, receipt, None, None));
        };
        let mut doc = parse_toml(bytes)?;
        if ownership_conflict(Some(receipt), true, &owned_from_doc(&doc, catalog_bytes)) {
            return Err(ByokError::conflict(
                "Owned Codex fields changed outside OCG",
            ));
        }
        restore_applied_selection(&mut doc, receipt)?;
        if receipt.created_catalog && catalog_is_referenced(&doc, catalog_path) {
            return Err(ByokError::conflict(
                "The Codex model catalog is still referenced and cannot be removed safely",
            ));
        }
        if let Some(providers) = doc
            .get_mut("model_providers")
            .and_then(Item::as_table_like_mut)
        {
            providers.remove(PROVIDER_ID);
        }
        let remaining = doc.to_string();
        let target_out = if receipt.created_target && remaining.trim().is_empty() {
            None
        } else {
            Some(remaining.into_bytes())
        };
        let catalog_out = if receipt.created_catalog {
            None
        } else {
            catalog_bytes.map(|bytes| bytes.to_vec())
        };
        Ok(removal_plan(
            target_path,
            catalog_path,
            receipt,
            target_out,
            catalog_out,
        ))
    }
}

fn removal_plan(
    target_path: &Path,
    catalog_path: &Path,
    receipt: &Receipt,
    target_out: Option<Vec<u8>>,
    catalog_out: Option<Vec<u8>>,
) -> ApplyPlan {
    ApplyPlan {
        files: vec![
            planned_target(target_path.to_path_buf(), target_out),
            planned_catalog(catalog_path.to_path_buf(), catalog_out),
        ],
        created_target: receipt.created_target,
        created_catalog: receipt.created_catalog,
        baseline_default: receipt.baseline_default.clone(),
        last_applied_default: None,
        managed: snapshot(Vec::new(), Value::Null, None),
        first_owned: receipt.first_owned.clone(),
    }
}

fn parse_toml(bytes: &[u8]) -> ByokResult<DocumentMut> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| ByokError::invalid("Codex config.toml is not valid UTF-8"))?;
    text.parse::<DocumentMut>()
        .map_err(|_| ByokError::invalid("Codex config.toml is not valid TOML"))
}

fn root_shape_ok(doc: &DocumentMut) -> bool {
    match doc.get("model_providers") {
        None | Some(Item::None) => true,
        Some(Item::Table(_)) => true,
        Some(Item::Value(value)) if value.is_inline_table() => true,
        _ => false,
    }
}

fn provider_present(doc: &DocumentMut) -> bool {
    doc.get("model_providers")
        .and_then(|item| item.get(PROVIDER_ID))
        .is_some_and(|item| !item.is_none())
}

fn ocg_default(doc: &DocumentMut) -> Option<String> {
    let provider = doc.get("model_provider").and_then(Item::as_str)?;
    if provider != PROVIDER_ID {
        return None;
    }
    doc.get("model").and_then(Item::as_str).map(str::to_string)
}

fn string_key(doc: &DocumentMut, key: &str) -> Option<String> {
    doc.get(key).and_then(Item::as_str).map(str::to_string)
}

fn catalog_unowned(catalog_bytes: Option<&[u8]>, receipt: Option<&Receipt>) -> bool {
    catalog_bytes.is_some() && !receipt.is_some_and(|receipt| receipt.created_catalog)
}

fn selection_captured(receipt: &Receipt) -> bool {
    receipt.first_owned.get("captured").and_then(Value::as_bool) == Some(true)
}

fn selection_baseline(doc: &DocumentMut, catalog_path: &Path) -> Value {
    json!({
        "captured": true,
        "model": doc.get("model").and_then(Item::as_str),
        "model_provider": doc.get("model_provider").and_then(Item::as_str),
        "model_catalog_json": doc.get("model_catalog_json").and_then(Item::as_str),
        "applied_catalog": catalog_display(catalog_path),
    })
}

fn owned_string(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn reject_dangling_selection(
    doc: &DocumentMut,
    requested: Option<&str>,
    retained: &[String],
) -> ByokResult<()> {
    let provider = if requested.is_some() {
        Some(PROVIDER_ID.to_string())
    } else {
        string_key(doc, "model_provider")
    };
    if provider.as_deref() != Some(PROVIDER_ID) {
        return Ok(());
    }
    let model = match requested {
        Some(id) => Some(id.to_string()),
        None => string_key(doc, "model"),
    };
    let referenced = model.as_deref().filter(|id| !id.is_empty());
    if referenced.is_none() {
        return ensure_retained_ocg_default(Some(""), retained);
    }
    ensure_retained_ocg_default(referenced, retained)
}

fn restore_applied_selection(doc: &mut DocumentMut, receipt: &Receipt) -> ByokResult<()> {
    let current_provider = string_key(doc, "model_provider");
    if current_provider.as_deref() == Some(PROVIDER_ID) && !selection_captured(receipt) {
        return ensure_retained_ocg_default(Some(""), &[]);
    }
    if !selection_captured(receipt) {
        return Ok(());
    }
    let current_model = string_key(doc, "model");
    let current_catalog = string_key(doc, "model_catalog_json");
    let provider_is_ocg = current_provider.as_deref() == Some(PROVIDER_ID);
    let model_matches = current_model.as_deref() == receipt.last_applied_default.as_deref();
    if provider_is_ocg && !model_matches {
        return ensure_retained_ocg_default(current_model.as_deref().or(Some("")), &[]);
    }
    if provider_is_ocg && model_matches {
        set_optional_str(
            doc,
            "model",
            owned_string(&receipt.first_owned, "model").as_deref(),
        );
    }
    if provider_is_ocg {
        set_optional_str(
            doc,
            "model_provider",
            owned_string(&receipt.first_owned, "model_provider").as_deref(),
        );
    }
    let applied_catalog = owned_string(&receipt.first_owned, "applied_catalog");
    if applied_catalog.is_some() && current_catalog.as_deref() == applied_catalog.as_deref() {
        set_optional_str(
            doc,
            "model_catalog_json",
            owned_string(&receipt.first_owned, "model_catalog_json").as_deref(),
        );
    }
    if string_key(doc, "model_provider").as_deref() == Some(PROVIDER_ID) {
        let model = string_key(doc, "model");
        return ensure_retained_ocg_default(model.as_deref().or(Some("")), &[]);
    }
    Ok(())
}

fn write_provider(doc: &mut DocumentMut, gateway: &str, secret: &str) -> ByokResult<()> {
    let providers = match doc.get_mut("model_providers") {
        Some(item) if item.is_none() => {
            *item = Item::Table(Table::new());
            item
        }
        Some(item) => item,
        None => {
            doc["model_providers"] = Item::Table(Table::new());
            &mut doc["model_providers"]
        }
    };
    let table = providers
        .as_table_mut()
        .ok_or_else(|| ByokError::invalid("Codex model_providers must be a table"))?;
    if table.get(PROVIDER_ID).is_none() {
        table.insert(PROVIDER_ID, Item::Table(Table::new()));
    }
    let provider = table
        .get_mut(PROVIDER_ID)
        .and_then(Item::as_table_mut)
        .ok_or_else(|| ByokError::invalid("Codex ocg provider must be a table"))?;
    provider["name"] = value(PROVIDER_NAME);
    provider["base_url"] = value(gateway);
    provider["wire_api"] = value("responses");
    provider["experimental_bearer_token"] = value(secret);
    provider["requires_openai_auth"] = value(false);
    provider["supports_websockets"] = value(false);
    Ok(())
}

fn activate_selection(doc: &mut DocumentMut, model: &str, catalog_path: &Path) {
    doc["model"] = value(model);
    doc["model_provider"] = value(PROVIDER_ID);
    doc["model_catalog_json"] = value(catalog_display(catalog_path));
}

fn catalog_display(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn catalog_is_referenced(doc: &DocumentMut, catalog_path: &Path) -> bool {
    string_key(doc, "model_catalog_json")
        .is_some_and(|configured| same_catalog(&configured, catalog_path))
}

fn same_catalog(configured: &str, catalog_path: &Path) -> bool {
    let configured = configured.trim();
    if configured.is_empty() {
        return false;
    }
    let raw = PathBuf::from(configured);
    let mut candidates = vec![raw.clone()];
    if raw.is_relative() {
        if let Some(catalog_dir) = catalog_path.parent() {
            candidates.push(catalog_dir.join(&raw));
            if let Some(config_dir) = catalog_dir.parent() {
                candidates.push(config_dir.join(&raw));
            }
        }
    }
    candidates
        .iter()
        .any(|candidate| paths_identify(candidate, catalog_path))
}

fn paths_identify(candidate: &Path, catalog_path: &Path) -> bool {
    if paths_eq(candidate, catalog_path) {
        return true;
    }
    match (
        std::fs::canonicalize(candidate),
        std::fs::canonicalize(catalog_path),
    ) {
        (Ok(left), Ok(right)) => paths_eq(&left, &right),
        _ => false,
    }
}

fn paths_eq(left: &Path, right: &Path) -> bool {
    let left = lexical_normal(left);
    let right = lexical_normal(right);
    let mut left_components = left.components();
    let mut right_components = right.components();
    loop {
        match (left_components.next(), right_components.next()) {
            (None, None) => return true,
            (Some(left), Some(right)) if component_eq(left, right) => {}
            _ => return false,
        }
    }
}

fn component_eq(left: std::path::Component<'_>, right: std::path::Component<'_>) -> bool {
    if left == right {
        return true;
    }
    #[cfg(windows)]
    {
        left.as_os_str().eq_ignore_ascii_case(right.as_os_str())
    }
    #[cfg(not(windows))]
    {
        let _ = (left, right);
        false
    }
}

fn lexical_normal(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn set_optional_str(doc: &mut DocumentMut, key: &str, value_str: Option<&str>) {
    match value_str {
        Some(text) => doc[key] = value(text),
        None => {
            doc.remove(key);
        }
    }
}

fn owned_from_doc(doc: &DocumentMut, catalog_bytes: Option<&[u8]>) -> Value {
    let provider = doc
        .get("model_providers")
        .and_then(|item| item.get(PROVIDER_ID))
        .map(toml_item_json);
    json!({
        "provider": provider,
        "catalog_sha256": content_hash(catalog_bytes),
    })
}

const OFFICIAL_PROMPT: &str = include_str!("../../../../../resources/codex-byok/prompt.md");

fn build_catalog(models: &[ByokModel]) -> Value {
    json!({
        "models": models.iter().map(catalog_model).collect::<Vec<_>>(),
    })
}

fn catalog_model(model: &ByokModel) -> Value {
    let mut modalities = vec!["text"];
    if has_image(&model.metadata) {
        modalities.push("image");
    }
    let emitted = wire_efforts(model.metadata.reasoning_efforts.as_ref());
    let supported: Vec<Value> = emitted
        .iter()
        .map(|(wire, level)| {
            json!({
                "effort": wire,
                "description": level,
            })
        })
        .collect();
    let default_reasoning = if model.metadata.reasoning == Some(true) {
        emitted.first().map(|(wire, _)| wire.clone())
    } else {
        None
    };
    let mut entry = json!({
        "slug": model.id,
        "display_name": display_name(model),
        "description": null,
        "default_reasoning_level": default_reasoning,
        "supported_reasoning_levels": supported,
        "shell_type": "shell_command",
        "visibility": "list",
        "supported_in_api": true,
        "priority": 0,
        "upgrade": null,
        "support_verbosity": false,
        "default_verbosity": null,
        "apply_patch_tool_type": null,
        "truncation_policy": {"mode": "bytes", "limit": 10_000},
        "supports_image_detail_original": false,
        "experimental_supported_tools": [],
        "input_modalities": modalities,
        "model_messages": {
            "instructions_template": OFFICIAL_PROMPT,
        },
    });
    if let Some(context) = model.metadata.context_window {
        let context = context as i64;
        entry["context_window"] = json!(context);
        entry["max_context_window"] = json!(context);
    }
    entry
}

fn wire_efforts(
    efforts: Option<&std::collections::BTreeMap<String, String>>,
) -> Vec<(String, String)> {
    let Some(efforts) = efforts else {
        return Vec::new();
    };
    let mut seen = std::collections::BTreeSet::new();
    let mut emitted = Vec::new();
    for (level, spelling) in efforts {
        if spelling.trim().is_empty() || !seen.insert(spelling.clone()) {
            continue;
        }
        emitted.push((spelling.clone(), level.clone()));
    }
    emitted
}
