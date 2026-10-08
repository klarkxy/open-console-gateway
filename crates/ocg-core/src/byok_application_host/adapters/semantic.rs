//! Sealed adapters' managed fields and three-way preservation helpers.
use super::super::receipt::without_secrets;
use crate::byok_application::{ByokClient, ByokError, ByokResult};
use serde_json::{Map, Value, json};

fn pick(value: &Value, paths: &[&str]) -> Value {
    if value.is_null() {
        return Value::Null;
    }
    let mut out = json!({});
    for path in paths {
        let keys: Vec<_> = path.split('.').collect();
        let mut source = value;
        for key in &keys {
            source = source.get(*key).unwrap_or(&Value::Null);
        }
        if !source.is_null() {
            put(&mut out, &keys, source.clone());
        }
    }
    out
}
fn put(value: &mut Value, keys: &[&str], field: Value) {
    if keys.is_empty() {
        *value = field;
        return;
    }
    if !value.is_object() {
        *value = json!({});
    }
    let map = value.as_object_mut().expect("object");
    if keys.len() == 1 {
        map.insert(keys[0].into(), field);
    } else {
        put(
            map.entry(keys[0]).or_insert_with(|| json!({})),
            &keys[1..],
            field,
        );
    }
}
fn map_rows(value: &Value, f: impl Fn(&Value) -> Value) -> Value {
    match value {
        Value::Object(rows) => {
            Value::Object(rows.iter().map(|(id, row)| (id.clone(), f(row))).collect())
        }
        Value::Array(rows) => Value::Array(rows.iter().map(f).collect()),
        _ => value.clone(),
    }
}

pub(crate) fn projection(client: ByokClient, owned: &Value) -> Value {
    let clean = without_secrets(owned);
    match client {
        ByokClient::Codex => json!({
            "provider": pick(&clean["provider"], &["name", "base_url", "wire_api", "requires_openai_auth", "supports_websockets", "env_key"]),
            "catalog": clean.get("catalog").map(|catalog| {
                if catalog.is_null(){return Value::Null;}
                json!({"models":map_rows(&catalog["models"],|model|pick(model,&["slug","display_name","description","supported_reasoning_levels","shell_type","visibility","supported_in_api","priority","upgrade","support_verbosity","apply_patch_tool_type","truncation_policy","supports_image_detail_original","experimental_supported_tools","input_modalities","model_messages"]))})
            }).unwrap_or_else(|| clean["catalog_sha256"].clone()),
        }),
        ByokClient::Kimi => json!({
            "providers": map_rows(&super::managed_provider_object(&clean), |row| pick(row, &["type", "base_url"])),
            "models": map_rows(clean.get("models").unwrap_or(&json!({})), |row| pick(row, &["provider", "model", "display_name", "capabilities", "support_efforts", "off_effort"])),
        }),
        ByokClient::Minimax => json!({
            "providers": map_rows(&super::managed_provider_object(&clean), |row| {
                if row.is_null() { return Value::Null; }
                let mut out = pick(row, &["name", "kind", "enabled", "api", "options.baseURL", "options.authMode"]);
                out["models"] = map_rows(row.get("models").unwrap_or(&json!({})), |model| pick(model, &["name", "enabled", "configuration_source", "tool_call", "reasoning", "attachment", "capabilities.support_image", "thinking.effortOptions"]));
                out
            }),
        }),
        ByokClient::Zcode => json!({
            "providers": map_rows(&clean["providers"], |row| pick(row, &["providerId", "providerName", "enabled", "config.group", "config.api.type", "config.api.baseUrl", "config.access.type", "config.personalModelIds", "config.modelOrder"])),
            "providerOrder": clean["providerOrder"],
            "models": map_rows(&clean["models"], |row| {
                let mut out=pick(row,&["providerId","modelId","config.enabled","config.properties.supportsToolCall","config.properties.inputFormat.supportsImage","config.optionSpecs.reasoningLevel.values","config.optionSpecs.reasoningLevel.map"]);
                if !row.is_null(){if out["config"].get("properties").is_none(){out["config"]["properties"]=json!({});}if out["config"].get("optionSpecs").is_none(){out["config"]["optionSpecs"]=json!({});}}
                out
            }),
        }),
        ByokClient::Copilot => {
            let provider = &clean["provider"];
            if provider.is_null() {
                return json!({"provider": null});
            }
            let mut out = pick(provider, &["name", "vendor", "url"]);
            out["models"] = map_rows(&provider["models"], |model| {
                let mut out = pick(
                    model,
                    &[
                        "id",
                        "name",
                        "apiType",
                        "url",
                        "toolCalling",
                        "vision",
                        "thinking",
                        "supportsReasoningEffort",
                        "reasoningEffortFormat",
                    ],
                );
                out["requestHeaders"] = json!({});
                out
            });
            json!({"provider": out})
        }
    }
}

/// Keep client-owned leaves. A customized budget replaces its prior generated
/// value only when it changed since the receipt. Fresh metadata still updates
/// unchanged generated limits; an explicit Copilot budget wins over local limits.
pub(crate) fn preserve(
    client: ByokClient,
    current: &Value,
    last: Option<&Value>,
    next: &Value,
    explicit_budget: bool,
) -> Value {
    let mut result = next.clone();
    let masks = union_mask(projection(client, next), &projection(client, current));
    if client == ByokClient::Minimax {
        if let Some(providers) = result["providers"].as_object_mut() {
            for (provider_id, provider) in providers {
                if let Some(models) = provider.get_mut("models").and_then(Value::as_object_mut) {
                    for (id, model) in models {
                        let find = |owned: &Value| {
                            owned["providers"]
                                .as_object()
                                .into_iter()
                                .flat_map(|map| map.values())
                                .find_map(|provider| {
                                    provider.get("models").and_then(|models| models.get(id))
                                })
                                .cloned()
                        };
                        if let Some(current) = find(current) {
                            let old = projection(
                                client,
                                &json!({"providers":{"ocg":{"models":{"row":current}}}}),
                            );
                            let row_mask = union_mask(
                                masks["providers"][provider_id]["models"][id].clone(),
                                &old["providers"]["ocg"]["models"]["row"],
                            );
                            preserve_inner(
                                &current,
                                &last.and_then(find).unwrap_or(Value::Null),
                                model,
                                &row_mask,
                                explicit_budget,
                                true,
                            );
                        }
                    }
                }
            }
        }
    }
    preserve_inner(
        current,
        last.unwrap_or(&Value::Null),
        &mut result,
        &masks,
        explicit_budget,
        client != ByokClient::Copilot,
    );
    if client == ByokClient::Copilot {
        if let Some(provider) = result["provider"].as_object_mut() {
            provider.remove("url");
        }
    }
    // The catalog hash belongs to the private snapshot, not client settings.
    if client == ByokClient::Codex && !result["catalog"].is_null() {
        if let Ok(bytes) = serde_json::to_vec_pretty(&result["catalog"]) {
            result["catalog_sha256"] = json!(super::super::fs::content_hash(Some(&bytes)));
        }
    }
    result
}
/// The union recognizes managed leaves that the next export deliberately removes.
/// Otherwise a stale effort menu could be mistaken for a client extension.
fn union_mask(mut next: Value, current: &Value) -> Value {
    match (&mut next, current) {
        (Value::Object(next), Value::Object(current)) => {
            for (key, value) in current {
                match next.get_mut(key) {
                    Some(next) => *next = union_mask(next.clone(), value),
                    None => {
                        next.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        (Value::Array(next), Value::Array(current)) => {
            for next in next {
                if let Some(current) = row(current, next) {
                    *next = union_mask(next.clone(), current);
                }
            }
        }
        (Value::Null, _) => next = current.clone(),
        _ => {}
    }
    next
}
fn identity(value: &Value) -> Option<String> {
    for key in ["slug", "id", "modelId", "providerId"] {
        if let Some(id) = value.get(key).and_then(Value::as_str) {
            return Some(format!(
                "{}:{}",
                value
                    .get("providerId")
                    .and_then(Value::as_str)
                    .unwrap_or(""),
                id
            ));
        }
    }
    None
}
fn row<'a>(values: &'a [Value], value: &Value) -> Option<&'a Value> {
    let id = identity(value)?;
    values
        .iter()
        .find(|candidate| identity(candidate).as_deref() == Some(id.as_str()))
        .or_else(|| {
            values.iter().find(|candidate| {
                candidate.get("modelId") == value.get("modelId") && value.get("modelId").is_some()
            })
        })
}
fn preserve_inner(
    current: &Value,
    last: &Value,
    next: &mut Value,
    managed: &Value,
    explicit_budget: bool,
    clamp_generated: bool,
) {
    match (current, next) {
        (Value::Object(src), Value::Object(dst)) => {
            for (key, value) in src {
                if [
                    "api_key",
                    "apiKey",
                    "Authorization",
                    "experimental_bearer_token",
                ]
                .contains(&key.as_str())
                {
                    continue;
                }
                let owned = managed.get(key);
                if owned.is_none() {
                    let budget = ["maxInputTokens", "maxOutputTokens"].contains(&key.as_str());
                    let generated_limit = [
                        "context_window",
                        "max_context_window",
                        "max_context_size",
                        "limit",
                        "maxInputTokens",
                        "maxOutputTokens",
                        "contextWindow",
                        "max",
                    ]
                    .contains(&key.as_str());
                    if !(explicit_budget && budget)
                        && (last.get(key) != Some(value)
                            || (!dst.contains_key(key) && !generated_limit))
                    {
                        let mut preserved = if generated_limit && value.is_object() {
                            preserve_budget(
                                value,
                                last.get(key).unwrap_or(&Value::Null),
                                dst.get(key).unwrap_or(&Value::Null),
                            )
                        } else {
                            value.clone()
                        };
                        if generated_limit && clamp_generated {
                            clamp_limits(&mut preserved, dst.get(key).unwrap_or(&Value::Null));
                        }
                        dst.insert(key.clone(), preserved);
                    }
                } else if let Some(next) = dst.get_mut(key) {
                    preserve_inner(
                        value,
                        last.get(key).unwrap_or(&Value::Null),
                        next,
                        owned.expect("owned"),
                        explicit_budget,
                        clamp_generated,
                    );
                }
            }
        }
        (Value::Array(src), Value::Array(dst)) => {
            let empty = Vec::new();
            let prior = last.as_array().unwrap_or(&empty);
            let masks = managed.as_array().unwrap_or(&empty);
            for next in dst {
                if let Some(current) = row(src, next) {
                    let mask = row(masks, next).unwrap_or(&Value::Null);
                    preserve_inner(
                        current,
                        row(prior, next).unwrap_or(&Value::Null),
                        next,
                        mask,
                        explicit_budget,
                        clamp_generated,
                    );
                }
            }
        }
        _ => {}
    }
}

pub(crate) fn generated_projection(client: ByokClient, owned: &Value) -> Value {
    let mut out = projection(client, owned);
    fn copy_limits(full: &Value, out: &mut Value) {
        match (full, out) {
            (Value::Object(src), Value::Object(dst)) => {
                for (key, value) in src {
                    if key == "maxOutputTokens" && value.is_object() {
                        dst.insert(key.clone(), pick(value, &["max"]));
                    } else if [
                        "maxInputTokens",
                        "maxOutputTokens",
                        "max_context_size",
                        "context_window",
                        "max_context_window",
                        "default_reasoning_level",
                        "default_verbosity",
                    ]
                    .contains(&key.as_str())
                    {
                        dst.insert(key.clone(), value.clone());
                    } else if key == "limit" && value.is_object() {
                        dst.insert(key.clone(), pick(value, &["context", "output"]));
                    } else if key == "contextWindow" || key == "max" {
                        dst.insert(key.clone(), value.clone());
                    } else if let Some(next) = dst.get_mut(key) {
                        copy_limits(value, next);
                    } else if value.is_object()
                        && ["properties", "optionSpecs"].contains(&key.as_str())
                    {
                        let mut next = json!({});
                        copy_limits(value, &mut next);
                        dst.insert(key.clone(), next);
                    }
                }
            }
            (Value::Array(src), Value::Array(dst)) => {
                for (source, target) in src.iter().zip(dst) {
                    copy_limits(source, target);
                }
            }
            _ => {}
        }
    }
    copy_limits(&without_secrets(owned), &mut out);
    if client == ByokClient::Kimi {
        for id in super::MANAGED_PROVIDER_IDS {
            if out["providers"][id].is_object() {
                let name = match id {
                    "ocg-chat" => "Open Console Gateway Chat",
                    "ocg-responses" => "Open Console Gateway Responses",
                    "ocg-messages" => "Open Console Gateway Messages",
                    _ => "Open Console Gateway",
                };
                out["providers"][id]["name"] = json!(name);
            }
        }
    }
    out
}

pub(crate) fn undo(
    client: ByokClient,
    current: &Value,
    baseline: &Value,
    applied: &Value,
    generated: &Value,
) -> ByokResult<Value> {
    let mut out = current.clone();
    undo_inner(
        &mut out,
        baseline,
        applied,
        &projection(client, applied),
        &projection(client, baseline),
        generated,
    )?;
    Ok(out)
}
fn undo_inner(
    current: &mut Value,
    baseline: &Value,
    applied: &Value,
    managed: &Value,
    baseline_managed: &Value,
    generated: &Value,
) -> ByokResult<()> {
    let is_row = applied.is_object()
        && [
            "slug",
            "id",
            "modelId",
            "providerId",
            "model",
            "wire_api",
            "vendor",
            "kind",
            "type",
            "configuration_source",
        ]
        .iter()
        .any(|key| applied.get(*key).is_some());
    if baseline.is_null() && is_row {
        if normalized(&without_secrets(current)) == normalized(generated) {
            *current = Value::Null;
            return Ok(());
        }
        return Err(ByokError::conflict(
            "An entry added after takeover contains client settings; preserve it before undoing takeover",
        ));
    }
    match current {
        Value::Object(map) => {
            let mut keys: Vec<String> = managed
                .as_object()
                .into_iter()
                .flat_map(|m| m.keys().cloned())
                .collect();
            keys.extend(
                baseline_managed
                    .as_object()
                    .into_iter()
                    .flat_map(|m| m.keys().cloned()),
            );
            keys.sort();
            keys.dedup();
            // Secrets are absent from receipts; restore them only from the private origin.
            for key in [
                "api_key",
                "apiKey",
                "experimental_bearer_token",
                "Authorization",
            ] {
                if map.contains_key(key) || baseline.get(key).is_some() {
                    keys.push(key.into());
                }
            }
            for key in keys {
                let base = baseline.get(&key).unwrap_or(&Value::Null);
                let last = applied.get(&key).unwrap_or(&Value::Null);
                let mask = managed.get(&key).unwrap_or(&Value::Null);
                let base_mask = baseline_managed.get(&key).unwrap_or(&Value::Null);
                if let Some(value) = map.get_mut(&key) {
                    if (value.is_object() && (mask.is_object() || base_mask.is_object()))
                        || (value.is_array() && mask.is_array())
                    {
                        undo_inner(
                            value,
                            base,
                            last,
                            mask,
                            base_mask,
                            generated.get(&key).unwrap_or(&Value::Null),
                        )?;
                        if base.is_null()
                            && (value.is_null() || value.as_object().is_some_and(Map::is_empty))
                        {
                            map.remove(&key);
                        }
                    } else if (mask != base_mask && *value == *last)
                        || [
                            "api_key",
                            "apiKey",
                            "experimental_bearer_token",
                            "Authorization",
                        ]
                        .contains(&key.as_str())
                    {
                        if base.is_null() {
                            map.remove(&key);
                        } else {
                            map.insert(key, base.clone());
                        }
                    }
                } else if !base.is_null() {
                    map.insert(key, base.clone());
                }
            }
        }
        Value::Array(values) => {
            let empty = Vec::new();
            let bases = baseline.as_array().unwrap_or(&empty);
            let last = applied.as_array().unwrap_or(&empty);
            let masks = managed.as_array().unwrap_or(&empty);
            let base_masks = baseline_managed.as_array().unwrap_or(&empty);
            if values.iter().all(|v| identity(v).is_some()) {
                let mut out = Vec::new();
                for mut value in values.drain(..) {
                    let base = row(bases, &value).unwrap_or(&Value::Null);
                    let applied = row(last, &value).unwrap_or(&Value::Null);
                    let mask = row(masks, &value).unwrap_or(&Value::Null);
                    let base_mask = row(base_masks, &value).unwrap_or(&Value::Null);
                    let generated_row = row(generated.as_array().unwrap_or(&empty), &value)
                        .cloned()
                        .unwrap_or(Value::Null);
                    undo_inner(&mut value, base, applied, mask, base_mask, &generated_row)?;
                    if !base.is_null() {
                        out.push(value);
                    } else if !value.is_null() && !value.as_object().is_some_and(Map::is_empty) {
                        return Err(ByokError::conflict(
                            "A model added after takeover contains client settings; preserve it before undoing takeover",
                        ));
                    }
                }
                for base in bases {
                    if row(&out, base).is_none() {
                        out.push(base.clone());
                    }
                }
                *values = out;
            } else if managed != baseline_managed {
                *values = bases.clone();
            }
        }
        _ => {
            if managed != baseline_managed {
                *current = baseline.clone();
            }
        }
    }
    Ok(())
}

pub(crate) fn customized(client: ByokClient, value: &Value) -> bool {
    let clean = without_secrets(value);
    let projected = projection(client, value);
    has_extra(&clean, &projected)
}
fn has_extra(full: &Value, mask: &Value) -> bool {
    match (full, mask) {
        (Value::Object(map), _) => map.iter().any(|(key, value)| {
            (mask.get(key).is_none()
                && !value.is_null()
                && !value.as_object().is_some_and(Map::is_empty))
                || (mask.get(key).is_some() && has_extra(value, &mask[key]))
        }),
        (Value::Array(rows), Value::Array(masks)) => rows
            .iter()
            .zip(masks)
            .any(|(row, mask)| has_extra(row, mask)),
        _ => false,
    }
}

fn normalized(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .filter_map(|(key, value)| {
                    let next = normalized(value);
                    (!next.is_null() && !next.as_object().is_some_and(Map::is_empty))
                        .then(|| (key.clone(), next))
                })
                .collect(),
        ),
        Value::Array(rows) => Value::Array(rows.iter().map(normalized).collect()),
        _ => value.clone(),
    }
}

fn clamp_limits(value: &mut Value, generated: &Value) {
    match value {
        Value::Number(number) => {
            if let (Some(saved), Some(limit)) = (number.as_u64(), generated.as_u64()) {
                *value = json!(saved.min(limit));
            }
        }
        Value::Object(map) => {
            for (key, value) in map {
                if ["context", "output", "max", "default"].contains(&key.as_str()) {
                    clamp_limits(
                        value,
                        generated
                            .get(key)
                            .or_else(|| generated.get("max"))
                            .unwrap_or(&Value::Null),
                    );
                }
            }
        }
        _ => {}
    }
}

fn preserve_budget(current: &Value, last: &Value, next: &Value) -> Value {
    let mut out = next.as_object().cloned().unwrap_or_default();
    if let Some(fields) = current.as_object() {
        for (key, value) in fields {
            let numeric = ["context", "input", "output", "max", "default"].contains(&key.as_str());
            if numeric && value.is_number() {
                if key != "default" && last.get(key) == Some(value) {
                    continue;
                }
                let mut saved = value.clone();
                clamp_limits(
                    &mut saved,
                    out.get(key)
                        .or_else(|| out.get("max"))
                        .unwrap_or(&Value::Null),
                );
                out.insert(key.clone(), saved);
            } else {
                out.insert(key.clone(), value.clone());
            }
        }
    }
    Value::Object(out)
}
