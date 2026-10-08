//! Read-only reviewed update planning; commit retains the native file lock and journal.
use super::adapters::semantic;
use super::receipt::{ApplyPlan, FileRole, new_receipt, without_secrets};
use super::*;
use crate::byok_application::{
    ByokPreview, ByokReview, CopilotTokenBudget, with_copilot_token_budget,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub(super) struct ReviewedPlan {
    pub inspection: ByokInspection,
    pub plan: ApplyPlan,
    pub prior: Option<Receipt>,
    pub budget: Option<CopilotTokenBudget>,
    pub adopted: bool,
    pub generated: Value,
}

impl ByokNativeHost {
    pub(super) fn preview_plan(
        &self,
        client: ByokClient,
        target: &ResolvedTarget,
        gateway: &str,
        models: &[ByokModel],
        budget: Option<CopilotTokenBudget>,
        secret: &str,
    ) -> ByokResult<ReviewedPlan> {
        let target = normalize_target(target)?;
        let store = Store::open(&self.data_dir, &target)?;
        let prior = load_receipt(&store, &target)?;
        let journal = store.journal_bytes()?;
        let bytes = read_optional(&target.path)?;
        let catalog_path = catalog_path(&target);
        let catalog = catalog_path
            .as_deref()
            .map(read_optional)
            .transpose()?
            .flatten();
        let mut inspection = self.view(
            client,
            &target,
            &store,
            prior.as_ref(),
            bytes.as_deref(),
            catalog.as_deref(),
            journal.as_deref(),
            false,
        );
        if inspection.status == ByokStatus::RecoveryRequired {
            return Err(ByokError::precondition(
                "Recover the interrupted write before configuring",
            ));
        }
        if !inspection.configure_supported {
            return Err(ByokError::precondition(
                "This configuration cannot be safely reviewed",
            ));
        }
        if models.is_empty() {
            return Err(ByokError::precondition(
                "No models are published; configure a model source first",
            ));
        }
        validate_models(client, models)?;
        if client != ByokClient::Copilot && budget.is_some() {
            return Err(ByokError::invalid(
                "copilotTokenBudget is only valid for Copilot",
            ));
        }
        let published = models;
        let saved_budget = prior
            .as_ref()
            .and_then(|receipt| receipt.copilot_token_budget);
        let applied_budget =
            (client == ByokClient::Copilot).then(|| budget.or(saved_budget).unwrap_or_default());
        let models = match applied_budget {
            Some(budget) => with_copilot_token_budget(models.to_vec(), budget)?,
            None => models.to_vec(),
        };
        let current = adapter(client).managed(bytes.as_deref(), catalog.as_deref())?;
        validate_owned(client, &current)?;
        let parsed =
            adapter(client).inspect_bytes(bytes.as_deref(), catalog.as_deref(), prior.as_ref());
        let previous_default = parsed
            .current_default
            .clone()
            .or(adapter(client).raw_default(bytes.as_deref())?);
        let takeover = parsed.collision;
        let overwrite = parsed.user_changed_owned;
        if takeover && !has_provider(client, &current) {
            return Err(ByokError::conflict(
                "An existing catalog without a unique managed provider cannot be adopted",
            ));
        }
        let default = if client == ByokClient::Copilot {
            None
        } else {
            parsed
                .current_default
                .clone()
                .filter(|id| models.iter().any(|model| &model.id == id))
                .or_else(|| models.first().map(|model| model.id.clone()))
        };
        // The adapter still performs every structural, alias and reference check.
        // A reviewed receipt changes only the comparison baseline for this plan.
        let mut effective = prior.clone().unwrap_or_else(|| new_receipt(&target));
        effective.adopted = takeover || prior.as_ref().is_some_and(|receipt| receipt.adopted);
        effective.last_managed.owned = current.clone();
        effective.last_managed.model_ids = parsed.configured_model_ids.clone();
        let use_effective = takeover || overwrite;
        let mut plan = adapter(client).configure(
            &target.path,
            catalog_path.as_deref(),
            bytes.as_deref(),
            catalog.as_deref(),
            if use_effective {
                Some(&effective)
            } else {
                prior.as_ref()
            },
            adapters::ConfigureInput {
                gateway_v1_url: gateway,
                secret,
                models: &models,
                default_model_id: default.as_deref(),
            },
        )?;
        // A synthetic receipt must not turn a first adoption into a created file.
        if prior.is_none() {
            plan.created_target = bytes.is_none();
            plan.created_catalog = catalog.is_none() && client == ByokClient::Codex;
        }
        let generated = semantic::generated_projection(client, &plan.managed.owned);
        preserve_plan(
            client,
            bytes.as_deref(),
            &current,
            prior.as_ref(),
            &mut plan,
            budget.is_some(),
        )?;
        if client == ByokClient::Copilot {
            constrain_copilot_plan(&mut plan, published)?;
        }
        let old_rows = model_rows(client, &current);
        let next_rows = model_rows(client, &plan.managed.owned);
        let old_ids: BTreeSet<_> = old_rows.keys().cloned().collect();
        let next_ids: BTreeSet<_> = models.iter().map(|model| model.id.clone()).collect();
        let removed: Vec<_> = old_ids.difference(&next_ids).cloned().collect();
        let added: Vec<_> = next_ids.difference(&old_ids).cloned().collect();
        let updated = old_ids
            .intersection(&next_ids)
            .filter(|id| {
                let old = without_secrets(old_rows.get(*id).expect("old row"));
                let next = without_secrets(next_rows.get(*id).unwrap_or(&Value::Null));
                old != next
                    || route_for_model(client, &current, id)
                        != route_for_model(client, &plan.managed.owned, id)
            })
            .cloned()
            .collect();
        let recorded = recorded_generated(client, prior.as_ref(), &current);
        let legacy_catalog_uncertain = client == ByokClient::Codex
            && prior.as_ref().is_some_and(|receipt| {
                receipt.version == 1
                    && receipt.last_managed.owned.get("catalog").is_none()
                    && parsed.user_changed_owned
            });
        let mut customized = Vec::new();
        for id in &removed {
            if row_customized(
                client,
                old_rows.get(id).expect("old row"),
                recorded
                    .as_ref()
                    .and_then(|generated| model_rows(client, generated).remove(id))
                    .as_ref(),
            ) || legacy_catalog_uncertain
            {
                customized.push(id.clone());
            }
        }
        // Provider extras also require acknowledgement when a protocol group disappears.
        for (provider, ids) in
            removed_provider_customizations(client, &current, &plan.managed.owned)
        {
            let _ = provider;
            customized.extend(ids);
        }
        customized.sort();
        customized.dedup();
        let mut preview = ByokPreview {
            plan_fingerprint: String::new(),
            added_model_ids: added,
            removed_model_ids: removed,
            updated_model_ids: updated,
            previous_default_model_id: previous_default,
            default_model_id: default,
            requires_takeover: takeover,
            requires_overwrite: overwrite,
            removed_models_with_customizations: customized,
        };
        let material = json!({"fingerprint":inspection.fingerprint,"gateway":gateway,"models":models,"budget":applied_budget,"explicitBudget":budget.is_some(),"plan":without_secrets(&plan.managed.owned),"default":plan.last_applied_default,"preview":preview});
        preview.plan_fingerprint = fs::sha256_hex(
            &serde_json::to_vec(&material)
                .map_err(|_| ByokError::internal("failed to fingerprint BYOK plan"))?,
        );
        inspection.copilot_token_budget = applied_budget;
        inspection.preview = Some(preview);
        Ok(ReviewedPlan {
            inspection,
            plan,
            adopted: takeover || prior.as_ref().is_some_and(|receipt| receipt.adopted),
            prior,
            budget: applied_budget,
            generated,
        })
    }

    pub(super) fn validate_reviewed(
        &self,
        client: ByokClient,
        target: &ResolvedTarget,
        expected: &str,
        gateway: &str,
        models: &[ByokModel],
        budget: Option<CopilotTokenBudget>,
        client_closed: bool,
        review: &ByokReview,
    ) -> ByokResult<ByokInspection> {
        require_closed(client, client_closed)?;
        self.require_fingerprint(client, &normalize_target(target)?, expected)?;
        let plan = self.preview_plan(client, target, gateway, models, budget, "")?;
        acknowledge(&plan.inspection, review)?;
        Ok(plan.inspection)
    }

    pub(super) fn configure_reviewed(
        &self,
        client: ByokClient,
        target: &ResolvedTarget,
        expected: &str,
        gateway: &str,
        secret: &str,
        models: &[ByokModel],
        budget: Option<CopilotTokenBudget>,
        client_closed: bool,
        review: &ByokReview,
    ) -> ByokResult<ByokInspection> {
        require_closed(client, client_closed)?;
        let target = normalize_target(target)?;
        prepare_lock_parent(&target)?;
        let cross = CrossProcessLock::acquire(client, &target.path, &self.lock_policy)?;
        self.require_fingerprint(client, &target, expected)?;
        let reviewed = self.preview_plan(client, &target, gateway, models, budget, secret)?;
        acknowledge(&reviewed.inspection, review)?;
        let store = Store::open(&self.data_dir, &target)?;
        #[cfg(test)]
        if let (Ok(mut injected), Ok(mut slot)) = (
            self.fail_after_writes.lock(),
            store.fail_after_writes.lock(),
        ) {
            *slot = injected.take();
        }
        let hashes = if reviewed.adopted {
            secret_hashes(&reviewed.plan.managed.owned)
        } else {
            Value::Null
        };
        cross.assert_held()?;
        store.apply_reviewed(
            &target,
            reviewed.prior,
            reviewed.plan,
            PendingKind::Configure,
            Some((
                reviewed.adopted,
                reviewed.budget,
                hashes,
                reviewed.generated,
            )),
        )?;
        self.inspect_store(client, &target, &store).map(|mut view| {
            view.activation_required = true;
            view
        })
    }

    pub(super) fn undo_takeover(
        &self,
        client: ByokClient,
        target: &ResolvedTarget,
        store: &Store,
        receipt: &Receipt,
        bytes: Option<&[u8]>,
        catalog: Option<&[u8]>,
    ) -> ByokResult<ApplyPlan> {
        let original = read_optional(&store.origin_dir().join("target.bin"))?;
        let original_catalog = read_optional(&store.origin_dir().join("catalog.bin"))?;
        let current = adapter(client).managed(bytes, catalog)?;
        if receipt.adopted_secret_hashes != Value::Null
            && secret_hashes(&current) != receipt.adopted_secret_hashes
        {
            return Err(ByokError::conflict(
                "Authentication fields changed after takeover; undo was not applied",
            ));
        }
        let baseline = adapter(client).managed(original.as_deref(), original_catalog.as_deref())?;
        let restored = semantic::undo(
            client,
            &current,
            &baseline,
            &receipt.last_managed.owned,
            &receipt.last_generated,
        )?;
        let next = adapter(client).replace_managed(bytes, &restored)?;
        let next = adapter(client).restore_selection(&next, original.as_deref(), receipt)?;
        let mut files = vec![adapters::planned_target(target.path.clone(), Some(next))];
        if let Some(path) = catalog_path(target) {
            let next = if restored
                .get("catalog")
                .is_some_and(|value| !value.is_null())
            {
                Some(
                    serde_json::to_vec_pretty(&restored["catalog"])
                        .map_err(|_| ByokError::internal("failed to encode restored catalog"))?,
                )
            } else {
                original_catalog
            };
            files.push(adapters::planned_catalog(path, next));
        }
        Ok(ApplyPlan {
            files,
            created_target: receipt.created_target,
            created_catalog: receipt.created_catalog,
            baseline_default: receipt.baseline_default.clone(),
            last_applied_default: None,
            managed: adapters::snapshot(Vec::new(), Value::Null, None),
            first_owned: receipt.first_owned.clone(),
        })
    }
}

fn acknowledge(inspection: &ByokInspection, review: &ByokReview) -> ByokResult<()> {
    let preview = inspection
        .preview
        .as_ref()
        .ok_or_else(|| ByokError::internal("BYOK preview is missing"))?;
    if review
        .preview_fingerprint
        .as_deref()
        .is_some_and(|expected| expected != preview.plan_fingerprint)
    {
        return Err(ByokError::conflict(
            "The reviewed update changed; preview it again",
        ));
    }
    let reviewed = review.preview_fingerprint.is_some();
    if preview.requires_takeover && !(reviewed && review.acknowledge_takeover) {
        return Err(ByokError::conflict(
            "Review and acknowledge takeover of the existing OCG provider",
        ));
    }
    if preview.requires_overwrite && !(reviewed && review.acknowledge_overwrite) {
        return Err(ByokError::conflict(
            "Review and acknowledge overwriting changed OCG fields",
        ));
    }
    if !preview.removed_models_with_customizations.is_empty()
        && !(reviewed && review.acknowledge_removal)
    {
        return Err(ByokError::conflict(
            "Review and acknowledge removal of customized models or providers",
        ));
    }
    Ok(())
}

// Version 1 compared the entire last-applied owned block, so its recorded
// limits are a usable generation baseline. Codex only recorded a catalog hash;
// its row history remains unknown and deletion is reviewed conservatively.
fn recorded_generated(
    client: ByokClient,
    receipt: Option<&Receipt>,
    current: &Value,
) -> Option<Value> {
    receipt.and_then(|receipt| {
        if !receipt.last_generated.is_null() {
            Some(receipt.last_generated.clone())
        } else if receipt.version == 1 {
            let baseline = if client == ByokClient::Codex
                && adapters::legacy_codex_catalog_matches(receipt, current)
            {
                current
            } else {
                &receipt.last_managed.owned
            };
            Some(semantic::generated_projection(client, baseline))
        } else {
            None
        }
    })
}

pub(super) fn preserve_plan(
    client: ByokClient,
    bytes: Option<&[u8]>,
    current: &Value,
    receipt: Option<&Receipt>,
    plan: &mut ApplyPlan,
    explicit_budget: bool,
) -> ByokResult<()> {
    let mut recorded = recorded_generated(client, receipt, current);
    // V1 stored exported budgets, but not the operator's global envelope.
    // Keep compatible per-row values until an explicit budget is supplied.
    if client == ByokClient::Copilot
        && !explicit_budget
        && receipt
            .is_some_and(|receipt| receipt.version == 1 && receipt.copilot_token_budget.is_none())
    {
        if let Some(rows) = recorded
            .as_mut()
            .and_then(|value| value.get_mut("provider"))
            .and_then(|value| value.get_mut("models"))
            .and_then(Value::as_array_mut)
        {
            for row in rows {
                if let Some(map) = row.as_object_mut() {
                    map.remove("maxInputTokens");
                    map.remove("maxOutputTokens");
                }
            }
        }
    }
    let restored = semantic::preserve(
        client,
        current,
        recorded.as_ref(),
        &plan.managed.owned,
        explicit_budget,
    );
    if restored != plan.managed.owned {
        for file in &mut plan.files {
            match file.role {
                FileRole::Target => {
                    file.new_bytes = Some(
                        adapter(client)
                            .replace_managed(file.new_bytes.as_deref().or(bytes), &restored)?,
                    )
                }
                FileRole::Catalog => {
                    file.new_bytes = Some(
                        serde_json::to_vec_pretty(&restored["catalog"]).map_err(|_| {
                            ByokError::internal("failed to encode preserved catalog")
                        })?,
                    )
                }
            }
        }
        plan.managed.owned = restored;
    }
    Ok(())
}

pub(super) fn reviewable(client: ByokClient, bytes: Option<&[u8]>, catalog: Option<&[u8]>) -> bool {
    adapter(client)
        .managed(bytes, catalog)
        .and_then(|owned| validate_owned(client, &owned))
        .is_ok()
}
fn has_provider(client: ByokClient, owned: &Value) -> bool {
    match client {
        ByokClient::Codex | ByokClient::Copilot => {
            owned.get("provider").is_some_and(Value::is_object)
        }
        ByokClient::Kimi | ByokClient::Minimax => owned["providers"]
            .as_object()
            .is_some_and(|map| map.values().any(Value::is_object)),
        ByokClient::Zcode => owned["providers"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty()),
    }
}
fn validate_owned(client: ByokClient, owned: &Value) -> ByokResult<()> {
    match client {
        ByokClient::Copilot => {
            if !owned["provider"].is_null() && !owned["provider"].is_object() {
                return Err(ByokError::invalid("Managed provider must be an object"));
            }
            validate_rows(&owned["provider"]["models"], "id")?;
        }
        ByokClient::Codex => {
            if !owned["provider"].is_null() && !owned["provider"].is_object() {
                return Err(ByokError::invalid("Managed provider must be an object"));
            }
            validate_rows(&owned["catalog"]["models"], "slug")?;
        }
        ByokClient::Zcode => {
            validate_rows(&owned["providers"], "providerId")?;
            let mut seen = BTreeSet::new();
            let mut ids = BTreeSet::new();
            for row in owned["models"].as_array().into_iter().flatten() {
                let key = (row["providerId"].as_str(), row["modelId"].as_str());
                if key.0.is_none() || key.1.is_none() || !seen.insert(key) || !ids.insert(key.1) {
                    return Err(ByokError::invalid(
                        "Ambiguous managed model rules cannot be reviewed",
                    ));
                }
            }
        }
        ByokClient::Kimi | ByokClient::Minimax => {
            let mut ids = BTreeSet::new();
            if client == ByokClient::Kimi {
                for (alias, row) in owned["models"].as_object().into_iter().flatten() {
                    let id = row["model"]
                        .as_str()
                        .ok_or_else(|| ByokError::invalid("Kimi model identity is missing"))?;
                    if alias != &format!("ocg/{id}") || !ids.insert(id) {
                        return Err(ByokError::invalid(
                            "Ambiguous Kimi aliases cannot be reviewed",
                        ));
                    }
                }
            } else {
                for provider in owned["providers"]
                    .as_object()
                    .into_iter()
                    .flat_map(|map| map.values())
                {
                    if !provider["options"].is_null() && !provider["options"].is_object() {
                        return Err(ByokError::invalid(
                            "MiniMax provider options must be an object",
                        ));
                    }
                    if !provider["models"].is_null() && !provider["models"].is_object() {
                        return Err(ByokError::invalid("MiniMax model list must be an object"));
                    }
                    for (id, row) in provider["models"].as_object().into_iter().flatten() {
                        if !row.is_object() || !ids.insert(id) {
                            return Err(ByokError::invalid(
                                "Ambiguous MiniMax model identities cannot be reviewed",
                            ));
                        }
                    }
                }
            }
            for row in owned["providers"]
                .as_object()
                .into_iter()
                .flat_map(|map| map.values())
            {
                if !row.is_null() && !row.is_object() {
                    return Err(ByokError::invalid("Managed provider must be an object"));
                }
            }
        }
    }
    Ok(())
}
fn validate_rows(value: &Value, key: &str) -> ByokResult<()> {
    if value.is_null() {
        return Ok(());
    }
    let rows = value
        .as_array()
        .ok_or_else(|| ByokError::invalid("Managed model list must be an array"))?;
    let mut seen = BTreeSet::new();
    for row in rows {
        let id = row
            .get(key)
            .and_then(Value::as_str)
            .ok_or_else(|| ByokError::invalid("Managed row identity is missing"))?;
        if !seen.insert(id) {
            return Err(ByokError::invalid(
                "Duplicate managed row identity cannot be reviewed",
            ));
        }
    }
    Ok(())
}
fn model_rows(client: ByokClient, owned: &Value) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    match client {
        ByokClient::Codex | ByokClient::Copilot => {
            let (rows, key) = if client == ByokClient::Codex {
                (&owned["catalog"]["models"], "slug")
            } else {
                (&owned["provider"]["models"], "id")
            };
            for row in rows.as_array().into_iter().flatten() {
                if let Some(id) = row[key].as_str() {
                    out.insert(id.into(), row.clone());
                }
            }
        }
        ByokClient::Kimi => {
            for row in owned["models"]
                .as_object()
                .into_iter()
                .flat_map(|map| map.values())
            {
                if let Some(id) = row["model"].as_str() {
                    out.insert(id.into(), row.clone());
                }
            }
        }
        ByokClient::Minimax => {
            for (provider, row) in owned["providers"].as_object().into_iter().flatten() {
                for (id, model) in row["models"].as_object().into_iter().flatten() {
                    let mut model = model.clone();
                    model["_provider"] = json!(provider);
                    out.insert(id.clone(), model);
                }
            }
        }
        ByokClient::Zcode => {
            for row in owned["models"].as_array().into_iter().flatten() {
                if let Some(id) = row["modelId"].as_str() {
                    out.insert(id.into(), row.clone());
                }
            }
        }
    }
    out
}
fn model_projection(client: ByokClient, row: &Value) -> Value {
    let owned = match client {
        ByokClient::Codex => json!({"catalog":{"models":[row]}}),
        ByokClient::Copilot => json!({"provider":{"models":[row]}}),
        ByokClient::Kimi => json!({"models":{"row":row}}),
        ByokClient::Minimax => {
            json!({"providers":{"ocg":{"models":{"row":row},"api":row["_provider"]}}})
        }
        ByokClient::Zcode => json!({"models":[row]}),
    };
    semantic::projection(client, &owned)
}
fn row_customized(client: ByokClient, row: &Value, last: Option<&Value>) -> bool {
    let mut row = row.clone();
    if let Some(map) = row.as_object_mut() {
        map.remove("_provider");
    }
    let budget_paths: &[&str] = match client {
        ByokClient::Codex => &[
            "default_reasoning_level",
            "default_verbosity",
            "context_window",
            "max_context_window",
        ],
        ByokClient::Kimi => &["max_context_size"],
        ByokClient::Minimax => &["limit"],
        ByokClient::Zcode => &[
            "config.properties.contextWindow",
            "config.optionSpecs.maxOutputTokens",
        ],
        ByokClient::Copilot => &["maxInputTokens", "maxOutputTokens"],
    };
    let mut projected = model_projection(client, &row);
    // Known generated limits are not custom settings unless changed since the receipt.
    let changed_budget = last.is_some_and(|last| {
        budget_paths
            .iter()
            .any(|path| path_value(&row, path) != path_value(last, path))
    });
    let mut stripped = row.clone();
    for path in budget_paths {
        remove_path(&mut stripped, path);
    }
    // Wrap then compare after projection; known generated leaves are removed by their mask.
    let _ = &mut projected;
    changed_budget || unknown_model_fields(client, &stripped)
}
fn unknown_model_fields(client: ByokClient, row: &Value) -> bool {
    let wrapper = match client {
        ByokClient::Codex => json!({"catalog":{"models":[row]}}),
        ByokClient::Copilot => json!({"provider":{"models":[row]}}),
        ByokClient::Kimi => json!({"models":{"row":row}}),
        ByokClient::Minimax => json!({"providers":{"ocg":{"models":{"row":row}}}}),
        ByokClient::Zcode => json!({"models":[row]}),
    };
    semantic::customized(client, &wrapper)
}
fn path_value<'a>(value: &'a Value, path: &str) -> &'a Value {
    let mut value = value;
    for key in path.split('.') {
        value = value.get(key).unwrap_or(&Value::Null);
    }
    value
}
fn remove_path(value: &mut Value, path: &str) {
    let mut keys = path.split('.').collect::<Vec<_>>();
    let last = keys.pop().unwrap_or("");
    let mut value = value;
    for key in keys {
        let Some(next) = value.get_mut(key) else {
            return;
        };
        value = next;
    }
    if let Some(map) = value.as_object_mut() {
        map.remove(last);
    }
}
fn removed_provider_customizations(
    client: ByokClient,
    current: &Value,
    next: &Value,
) -> Vec<(String, Vec<String>)> {
    let mut out = Vec::new();
    if matches!(client, ByokClient::Kimi | ByokClient::Minimax) {
        for (id, provider) in current["providers"].as_object().into_iter().flatten() {
            if provider.is_null() || !next["providers"][id].is_null() {
                continue;
            }
            let mut provider = provider.clone();
            if let Some(map) = provider.as_object_mut() {
                map.remove("models");
                if client == ByokClient::Kimi {
                    let name = match id.as_str() {
                        "ocg-chat" => "Open Console Gateway Chat",
                        "ocg-responses" => "Open Console Gateway Responses",
                        "ocg-messages" => "Open Console Gateway Messages",
                        _ => "Open Console Gateway",
                    };
                    if map.get("name").and_then(Value::as_str) == Some(name) {
                        map.remove("name");
                    }
                }
            }
            let wrapper = json!({"providers":{id:provider}});
            if semantic::customized(client, &wrapper) {
                out.push((
                    id.clone(),
                    model_rows(client, current).keys().cloned().collect(),
                ));
            }
        }
    }
    if client == ByokClient::Zcode {
        for provider in current["providers"].as_array().into_iter().flatten() {
            let id = provider["providerId"].as_str().unwrap_or("");
            if next["providers"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|row| row["providerId"].as_str() == Some(id))
            {
                continue;
            }
            if semantic::customized(client, &json!({"providers":[provider]})) {
                out.push((
                    id.into(),
                    model_rows(client, current).keys().cloned().collect(),
                ));
            }
        }
    }
    out
}
pub(super) fn secret_hashes(value: &Value) -> Value {
    fn visit(value: &Value, path: &str, out: &mut serde_json::Map<String, Value>) {
        match value {
            Value::Object(map) => {
                for (key, value) in map {
                    let path = format!("{path}/{key}");
                    if [
                        "api_key",
                        "apiKey",
                        "Authorization",
                        "experimental_bearer_token",
                    ]
                    .contains(&key.as_str())
                    {
                        out.insert(path, json!(fs::sha256_hex(value.to_string().as_bytes())));
                    } else {
                        visit(value, &path, out);
                    }
                }
            }
            Value::Array(rows) => {
                for (index, row) in rows.iter().enumerate() {
                    visit(row, &format!("{path}/{index}"), out);
                }
            }
            _ => {}
        }
    }
    let mut out = serde_json::Map::new();
    visit(value, "", &mut out);
    Value::Object(out)
}

fn route_for_model(client: ByokClient, owned: &Value, id: &str) -> Value {
    let projected = semantic::projection(client, owned);
    match client {
        ByokClient::Codex=>projected["provider"].clone(),
        ByokClient::Copilot=>model_rows(client,owned).get(id).map(|row|json!({"apiType":row["apiType"],"url":row["url"],"providerUrl":projected["provider"]["url"]})).unwrap_or(Value::Null),
        ByokClient::Kimi=>model_rows(client,owned).get(id).map(|row|projected["providers"][row["provider"].as_str().unwrap_or("")].clone()).unwrap_or(Value::Null),
        ByokClient::Minimax=>model_rows(client,owned).get(id).map(|row|{let mut provider=projected["providers"][row["_provider"].as_str().unwrap_or("")].clone();if let Some(map)=provider.as_object_mut(){map.remove("models");}provider}).unwrap_or(Value::Null),
        ByokClient::Zcode=>model_rows(client,owned).get(id).and_then(|row|projected["providers"].as_array().into_iter().flatten().find(|provider|provider["providerId"]==row["providerId"])).map(|provider|json!({"api":provider["config"]["api"],"accessType":provider["config"]["access"]["type"]})).unwrap_or(Value::Null),
    }
}

fn constrain_copilot_plan(plan: &mut ApplyPlan, published: &[ByokModel]) -> ByokResult<()> {
    let previous = plan.managed.owned.clone();
    for row in plan.managed.owned["provider"]["models"]
        .as_array_mut()
        .into_iter()
        .flatten()
    {
        let model = published
            .iter()
            .find(|model| row["id"].as_str() == Some(model.id.as_str()))
            .ok_or_else(|| ByokError::invalid("Copilot model is no longer published"))?;
        let input = row["maxInputTokens"]
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| {
                ByokError::precondition("Copilot input budget must be a positive supported integer")
            })?;
        let output = row["maxOutputTokens"]
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| {
                ByokError::precondition(
                    "Copilot output budget must be a positive supported integer",
                )
            })?;
        let adjusted = with_copilot_token_budget(
            vec![model.clone()],
            CopilotTokenBudget {
                max_input_tokens: input,
                max_output_tokens: output,
            },
        )?;
        let metadata = &adjusted[0].metadata;
        let output = metadata
            .max_output_tokens
            .ok_or_else(|| ByokError::precondition("Copilot output budget is missing"))?;
        let context = metadata
            .context_window
            .ok_or_else(|| ByokError::precondition("Copilot context budget is missing"))?;
        row["maxOutputTokens"] = json!(output);
        row["maxInputTokens"] = json!(context - output);
    }
    if previous != plan.managed.owned {
        for file in &mut plan.files {
            if file.role == FileRole::Target {
                file.new_bytes = Some(
                    adapter(ByokClient::Copilot)
                        .replace_managed(file.new_bytes.as_deref(), &plan.managed.owned)?,
                );
            }
        }
    }
    Ok(())
}
