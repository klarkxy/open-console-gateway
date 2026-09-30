//! Account-import merge.
//!
//! merge_import_account_on and insert_import_account_on participate in
//! the caller transaction. Database::import_accounts_with_contracts and
//! Database::import_node_state_with_cipher own the transaction boundary.

use super::*;

pub(super) fn insert_import_account_on(
    conn: &Connection,
    record: &AccountImportRecord,
    platform_contract_authoritative: bool,
) -> Result<()> {
    validate_import_account_on(conn, record, platform_contract_authoritative, None)?;
    let account = &record.account;
    let purchase_date = if account.purchase_date.trim().is_empty() {
        local_today()
    } else {
        normalize_purchase_date(&account.purchase_date)?
    };
    insert_account_row(conn, account, &purchase_date, record.verification_status)?;
    if let Some(config) = &record.custom_config {
        persist_account_custom_config_on(conn, &account.id, config)?;
    }
    if !record.capabilities.is_empty() {
        persist_account_model_capabilities_on(conn, &account.id, &record.capabilities)?;
    }
    restore_import_verification_on(conn, record)?;
    persist_ollama_billing_on(conn, record)?;
    Ok(())
}

fn validate_import_account_on(
    conn: &Connection,
    record: &AccountImportRecord,
    platform_contract_authoritative: bool,
    destination_override: Option<&str>,
) -> Result<()> {
    let account = &record.account;
    anyhow::ensure!(
        account.id != ZEN_FREE_ACCOUNT_ID,
        "Zen Free is database-owned and cannot be imported"
    );
    if let Some(plan) = builtin_provider(&account.provider_id) {
        if is_custom_api(&account.provider_id)
            && let Some(destination_override) = destination_override
        {
            let auth: String = conn.query_row(
                "SELECT auth_scheme FROM destinations WHERE id = ?1 AND adapter = 'http'",
                [destination_override],
                |row| row.get(0),
            )?;
            let expected = if auth == "none" {
                CredentialKind::None
            } else {
                CredentialKind::ApiKey
            };
            anyhow::ensure!(
                account.credential_kind == expected && account.quota_scope == QuotaScope::Key,
                "imported credential binding does not match HTTP destination authentication"
            );
        } else {
            account.validate_provider_binding()?;
        }
        ensure_enabled_provider_is_routable(&account.provider_id, account.enabled)?;
        let verification_gates_enablement = plan.verification_policy
            == VerificationPolicy::Required
            && ProviderRegistry::get(&account.provider_id)
                .is_some_and(|descriptor| descriptor.card_actions.enable_requires_verification);
        anyhow::ensure!(
            !account.enabled
                || !verification_gates_enablement
                || record.verification_status.allows_enablement(),
            "an enabled imported account must retain an enabling verification state"
        );
        if plan_requires_custom_config(plan) {
            // A platform-linked Custom Key belongs to the platform destination,
            // so V7/V8 deliberately carry no standalone Custom Endpoint facts.
            // Every other Custom account must retain the complete contract.
            let linked_without_custom_contract = platform_contract_authoritative
                && record.custom_config.is_none()
                && record.capabilities.is_empty();
            if !linked_without_custom_contract {
                anyhow::ensure!(
                    record.custom_config.is_some(),
                    "Custom API accounts require a complete endpoint"
                );
                anyhow::ensure!(
                    !record.capabilities.is_empty(),
                    "Custom API accounts require at least one model capability"
                );
            }
        } else {
            anyhow::ensure!(
                record.custom_config.is_none(),
                "custom config is only available for Custom API accounts"
            );
            anyhow::ensure!(
                record.capabilities.is_empty(),
                "model capabilities are only available for Custom API accounts"
            );
        }
        return Ok(());
    }
    let runtime = get_dynamic_provider_on(conn, &account.provider_id)?
        .ok_or_else(|| anyhow::anyhow!("unknown provider `{}`", account.provider_id))?;
    anyhow::ensure!(
        account.credential_kind == runtime.auth_kind.credential_kind()
            && account.quota_scope == runtime.auth_kind.quota_scope(),
        "provider binding does not match `{}`",
        account.provider_id
    );
    anyhow::ensure!(
        record.custom_config.is_none(),
        "custom config is only available for Custom API accounts"
    );
    anyhow::ensure!(
        record.capabilities.is_empty(),
        "model capabilities are only available for Custom API accounts"
    );
    Ok(())
}

fn restore_import_verification_on(conn: &Connection, record: &AccountImportRecord) -> Result<()> {
    conn.execute(
        "UPDATE credentials SET verification_status = ?2,
             connection_verified_at = ?3, verification_error = NULL
         WHERE legacy_account_id = ?1",
        params![
            record.account.id,
            record.verification_status.as_str(),
            record
                .connection_verified_at
                .map(|value| value.to_rfc3339()),
        ],
    )?;
    Ok(())
}

fn imported_key_unchanged(
    existing_cipher: &str,
    incoming_cipher: &str,
    cipher: Option<&dyn KeyCipher>,
) -> Result<bool> {
    if existing_cipher == incoming_cipher {
        return Ok(true);
    }
    let Some(cipher) = cipher else {
        return Ok(false);
    };
    if existing_cipher.is_empty() || incoming_cipher.is_empty() {
        return Ok(existing_cipher.is_empty() && incoming_cipher.is_empty());
    }
    let existing = cipher
        .decrypt(existing_cipher)
        .context("imported credential could not be compared with the stored Key")?;
    let incoming = cipher
        .decrypt(incoming_cipher)
        .context("imported credential could not be compared with the stored Key")?;
    Ok(existing == incoming)
}

pub(super) fn merge_import_account_on(
    conn: &Connection,
    record: &AccountImportRecord,
    platform_contract_authoritative: bool,
    destination_override: Option<&str>,
    cipher: Option<&dyn KeyCipher>,
) -> Result<()> {
    if conn
        .query_row(
            "SELECT 1 FROM credentials WHERE legacy_account_id = ?1",
            [&record.account.id],
            |_| Ok(()),
        )
        .optional()?
        .is_none()
    {
        if let Some(destination_id) = destination_override {
            validate_import_account_on(conn, record, true, Some(destination_id))?;
            let account = &record.account;
            let purchase_date = if account.purchase_date.trim().is_empty() {
                local_today()
            } else {
                normalize_purchase_date(&account.purchase_date)?
            };
            insert_account_row_for_destination(
                conn,
                account,
                destination_id,
                &purchase_date,
                record.verification_status,
            )?;
            restore_import_verification_on(conn, record)?;
            persist_ollama_billing_on(conn, record)?;
            return Ok(());
        }
        return insert_import_account_on(conn, record, platform_contract_authoritative);
    }
    validate_import_account_on(
        conn,
        record,
        platform_contract_authoritative,
        destination_override,
    )?;
    let account = &record.account;
    let purchase_date = if account.purchase_date.trim().is_empty() {
        local_today()
    } else {
        normalize_purchase_date(&account.purchase_date)?
    };
    let existing_cipher: Option<String> = conn
        .query_row(
            "SELECT key_cipher FROM credentials WHERE legacy_account_id = ?1",
            [&account.id],
            |row| row.get(0),
        )
        .optional()?;
    let existing = existing_cipher.as_deref().unwrap_or("");
    let same_key = imported_key_unchanged(existing, account.key_cipher.as_str(), cipher)?;
    if !same_key {
        quota_recovery::clear_for_account_on(conn, &account.id)?;
    }
    let key_cipher = if same_key {
        existing
    } else {
        account.key_cipher.as_str()
    };
    conn.execute(
        "UPDATE credentials SET
             name = ?2, username = ?3, key_cipher = ?4, enabled = ?5,
             purchase_date = ?6, account_type = ?7, setup_step = ?8, notes = ?9,
             provider_id = ?10, credential_kind = ?11,
             quota_scope = ?12, verification_status = ?13,
             connection_verified_at = ?14, verification_error = NULL,
             auth_error = NULL, last_error = NULL, updated_at = ?15
         WHERE legacy_account_id = ?1",
        params![
            account.id,
            account.name,
            account.username,
            key_cipher,
            account.enabled as i32,
            purchase_date,
            account.account_type.as_str(),
            account.setup_step.as_str(),
            account.notes,
            account.provider_id,
            account.credential_kind.as_str(),
            account.quota_scope.as_str(),
            record.verification_status.as_str(),
            record
                .connection_verified_at
                .map(|value| value.to_rfc3339()),
            Utc::now().to_rfc3339(),
        ],
    )?;
    if let Some(destination_id) = destination_override {
        conn.execute(
            "UPDATE credentials SET destination_id = ?2 WHERE legacy_account_id = ?1",
            params![account.id, destination_id],
        )?;
    } else {
        custom_store::delete_custom_destination_facts(conn, &account.id)?;
        conn.execute(
            "DELETE FROM provider_contract_model_protocol_overrides
             WHERE scope_kind = ?1 AND scope_id = ?2",
            params![SCOPE_KIND_CUSTOM_ENDPOINT, account.id],
        )?;
        conn.execute(
            "DELETE FROM provider_contract_model_protocols
             WHERE scope_kind = ?1 AND scope_id = ?2",
            params![SCOPE_KIND_CUSTOM_ENDPOINT, account.id],
        )?;
        conn.execute(
            "DELETE FROM provider_contract_scopes WHERE scope_kind = ?1 AND scope_id = ?2",
            params![SCOPE_KIND_CUSTOM_ENDPOINT, account.id],
        )?;
        if let Some(config) = &record.custom_config {
            persist_account_custom_config_on(conn, &account.id, config)?;
        }
        if !record.capabilities.is_empty() {
            persist_account_model_capabilities_on(conn, &account.id, &record.capabilities)?;
        }
    }
    // Child-table writers correctly invalidate verification during ordinary
    // edits. A validated node snapshot is different: it carries the source
    // verification state as part of the portable account definition.
    restore_import_verification_on(conn, record)?;
    persist_ollama_billing_on(conn, record)?;
    Ok(())
}

fn persist_ollama_billing_on(conn: &Connection, record: &AccountImportRecord) -> Result<()> {
    let is_ollama = record.account.provider_id == OLLAMA_PROVIDER_ID;
    if let Some(tier) = record.ollama_billing_tier {
        anyhow::ensure!(
            is_ollama,
            "Ollama billing tier is only valid for Ollama Cloud accounts"
        );
        if tier.requires_purchase_date() && record.account.purchase_date.trim().is_empty() {
            anyhow::bail!("a configured Ollama paid tier requires purchase_date");
        }
    } else if !is_ollama {
        return Ok(());
    }
    set_ollama_cloud_billing_tier_on(conn, &record.account.id, record.ollama_billing_tier)
}

pub(super) fn import_node_state_with_cipher<T>(
    db: &Database,
    record: &NodeImportRecord,
    cipher: Option<&dyn KeyCipher>,
    prepare_runtime: impl FnOnce(&Database) -> Result<T>,
) -> Result<T> {
    let tx = db.conn.unchecked_transaction()?;
    let before_import = crate::destination_projection::load_persisted(db)?;
    let mut ordered_ids = Vec::new();
    {
        let mut stmt = tx.prepare(
            "SELECT legacy_account_id FROM credentials
             WHERE COALESCE(credential_purpose, 'inference') = 'inference'
             ORDER BY routing_rank ASC, created_at ASC, legacy_account_id ASC",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        for id in rows {
            ordered_ids.push(id?);
        }
    }
    let imported_account_ids = record
        .accounts
        .iter()
        .map(|record| record.account.id.to_ascii_lowercase())
        .collect::<HashSet<_>>();
    let imported_platform_ids = record
        .platform_accounts
        .iter()
        .map(|parent| parent.id.to_ascii_lowercase())
        .collect::<HashSet<_>>();
    anyhow::ensure!(
        record
            .platform_catalogs
            .keys()
            .all(|id| imported_platform_ids.contains(&id.to_ascii_lowercase())),
        "platform catalog references a parent outside the imported node snapshot"
    );
    let platform_catalog_linked_account_ids = record
        .platform_links
        .iter()
        .filter(|link| {
            record
                .platform_catalogs
                .contains_key(&link.platform_account_id)
        })
        .map(|link| link.account_id.to_ascii_lowercase())
        .collect::<HashSet<_>>();
    let imported_custom_ids = record
        .custom_destinations
        .iter()
        .map(|destination| destination.id.as_str())
        .collect::<HashSet<_>>();
    anyhow::ensure!(
        record
            .custom_credential_destinations
            .iter()
            .all(|(account_id, destination_id)| {
                imported_account_ids.contains(&account_id.to_ascii_lowercase())
                    && imported_custom_ids.contains(destination_id.as_str())
            }),
        "Custom credential association references an account or destination outside the imported node snapshot"
    );
    let original_catalogs = record
        .destination_controls
        .iter()
        .map(|destination| {
            Ok((
                destination.id.clone(),
                destination_store::load_destination_catalog(&tx, &destination.id)?,
            ))
        })
        .collect::<Result<HashMap<_, _>>>()?;
    let mut original_routes = HashMap::new();
    for controls in &record.destination_controls {
        if destination_store::destination_exists(&tx, &controls.id)? {
            original_routes.insert(
                controls.id.clone(),
                destination_store::load_protocol_routes(&tx, &controls.id)?,
            );
            if !controls.protocol_routes.is_empty() {
                http_routes::validate_loaded_destination(controls)?;
                // Legacy import constructors stage the default fields. The
                // validated complete route set is restored before commit.
                tx.execute(
                    "UPDATE destinations SET protocol_routes_json = NULL WHERE id = ?1",
                    [&controls.id],
                )?;
            }
        }
    }
    for destination in &record.custom_destinations {
        custom_store::upsert_imported_custom_destination_on(
            &tx,
            &destination.id,
            &destination.legacy_id,
            &destination.name,
            &destination.endpoint_url,
            destination.protocol,
            destination.auth_scheme,
            &destination.models,
            destination.enabled,
        )?;
    }
    for runtime in &record.dynamic_providers {
        let onboarding_draft = record.draft_provider_ids.contains(&runtime.id)
            || record
                .draft_provider_ids
                .iter()
                .any(|id| id.eq_ignore_ascii_case(&runtime.id));
        upsert_imported_dynamic_provider_on(&tx, runtime, &imported_account_ids, onboarding_draft)?;
    }
    if record.platform_links_authoritative {
        let imported_ids: Vec<String> = record
            .accounts
            .iter()
            .map(|account| account.account.id.clone())
            .collect();
        platform::unlink_imported_accounts(&tx, &imported_ids)?;
    }
    for account in &record.accounts {
        let custom_destination_id = record
            .custom_credential_destinations
            .get(&account.account.id);
        merge_import_account_on(
            &tx,
            account,
            platform_catalog_linked_account_ids.contains(&account.account.id.to_ascii_lowercase())
                || custom_destination_id.is_some(),
            custom_destination_id.map(String::as_str),
            cipher,
        )?;
        if record.identity_snapshot.is_some() {
            // V6 carries cooldowns. Merging a package must not shorten a
            // deadline already observed on the destination host.
            let current = db
                .get_account(&account.account.id)?
                .ok_or_else(|| anyhow::anyhow!("imported account is missing"))?;
            let source = &account.account;
            let generic = current
                .cooldown_generic_until
                .max(source.cooldown_generic_until);
            let five_hours = current.cooldown_5h_until.max(source.cooldown_5h_until);
            let week = current.cooldown_week_until.max(source.cooldown_week_until);
            let month = current
                .cooldown_month_until
                .max(source.cooldown_month_until);
            let free = current.cooldown_free_until.max(source.cooldown_free_until);
            let until = current
                .cooldown_until
                .max(source.cooldown_until)
                .max(generic)
                .max(five_hours)
                .max(week)
                .max(month)
                .max(free);
            tx.execute(
                "UPDATE credentials SET cooldown_until=?2, cooldown_generic_until=?3,
                     cooldown_5h_until=?4, cooldown_week_until=?5,
                     cooldown_month_until=?6, cooldown_free_until=?7 WHERE legacy_account_id=?1",
                params![
                    source.id,
                    until.map(|v| v.to_rfc3339()),
                    generic.map(|v| v.to_rfc3339()),
                    five_hours.map(|v| v.to_rfc3339()),
                    week.map(|v| v.to_rfc3339()),
                    month.map(|v| v.to_rfc3339()),
                    free.map(|v| v.to_rfc3339())
                ],
            )?;
        }
    }
    platform::merge_platforms_on(
        &tx,
        &record.platform_accounts,
        &record.platform_links,
        &imported_account_ids,
    )?;
    for (parent_id, catalog) in &record.platform_catalogs {
        let destination_id =
            ocg_domain::destination::destination_id_for_platform_account(parent_id);
        destination_store::merge_destination_catalog_refuse_conflict(
            &tx,
            &destination_id,
            catalog,
        )?;
    }
    let extra_ids = record
        .platform_snapshots
        .keys()
        .chain(record.platform_versions.keys())
        .cloned()
        .collect::<HashSet<_>>();
    let extras = extra_ids
        .into_iter()
        .map(|id| platform::DestinationPlatformExtras {
            id: id.clone(),
            platform_kind: None,
            platform_version: record.platform_versions.get(&id).copied(),
            platform_snapshot: record.platform_snapshots.get(&id).cloned(),
        })
        .collect::<Vec<_>>();
    platform::restore_destination_platform_extras(&tx, &extras)?;
    for (parent_id, cipher) in &record.platform_observer_ciphers {
        let observer_id =
            ocg_domain::credential::observer_credential_id_for_platform_account(parent_id)
                .to_string();
        tx.execute(
            "UPDATE credentials SET key_cipher = ?2, has_secret = ?3 WHERE id = ?1",
            params![observer_id, cipher, i64::from(!cipher.is_empty())],
        )?;
    }
    if let Some(base_url) = record.cpa_base_url.as_deref() {
        cpa::upsert_destination_and_observer_on(
            &tx,
            base_url,
            record.cpa_management_key_cipher.as_deref().unwrap_or(""),
        )?;
    }

    let (sanitized, primary) = sanitize_config_json_primary_key(&record.config_json)?;
    let primary =
        primary.ok_or_else(|| anyhow::anyhow!("node migration primary Key is missing"))?;
    tx.execute(
        "INSERT INTO settings (key, value) VALUES ('config', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [sanitized],
    )?;
    for key in &record.sub_keys {
        anyhow::ensure!(key.deleted_at.is_none(), "migrated sub Key must be active");
        anyhow::ensure!(
            key.id != PRIMARY_KEY_ID,
            "sub Key cannot use the primary id"
        );
        tx.execute(
            "DELETE FROM access_keys WHERE id = ?1 AND is_primary = 0",
            [&key.id],
        )?;
    }
    upsert_primary_access_key_on(&tx, &primary)?;
    for key in &record.sub_keys {
        tx.execute(
            "INSERT INTO access_keys (id, name, key, is_primary, enabled, deleted_at, created_at)
             VALUES (?1, ?2, ?3, 0, ?4, NULL, ?5)",
            params![
                key.id,
                key.name,
                key.key,
                key.enabled as i32,
                key.created_at.to_rfc3339(),
            ],
        )?;
    }
    let merged_sub_keys: i64 = tx.query_row(
        "SELECT COUNT(*) FROM access_keys WHERE is_primary = 0 AND deleted_at IS NULL",
        [],
        |row| row.get(0),
    )?;
    anyhow::ensure!(
        merged_sub_keys <= 64,
        "merged node would exceed the 64 active sub Key limit"
    );

    let zen_changed = tx.execute(
        "UPDATE credentials SET enabled = ?2, updated_at = ?3
         WHERE legacy_account_id = ?1 AND provider_id = ?4 ",
        params![
            ZEN_FREE_ACCOUNT_ID,
            record.zen_free_enabled as i32,
            Utc::now().to_rfc3339(),
            OPENCODE_ZEN_FREE_PROVIDER_ID,
        ],
    )?;
    anyhow::ensure!(zen_changed == 1, "Zen Free singleton is missing");

    let mut ordered_set = ordered_ids.iter().cloned().collect::<HashSet<_>>();
    for id in &record.account_order {
        if ordered_set.insert(id.clone()) {
            ordered_ids.push(id.clone());
        }
    }
    let current_ids = {
        let mut stmt = tx.prepare(
            "SELECT legacy_account_id FROM credentials
             WHERE COALESCE(credential_purpose, 'inference') = 'inference'",
        )?;
        stmt.query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<HashSet<_>>>()?
    };
    anyhow::ensure!(
        ordered_ids.len() == current_ids.len()
            && ordered_ids.iter().collect::<HashSet<_>>().len() == ordered_ids.len()
            && ordered_ids.iter().all(|id| current_ids.contains(id)),
        "migrated account order does not cover the merged account set"
    );
    for (sort_order, id) in ordered_ids.iter().enumerate() {
        tx.execute(
            "UPDATE credentials SET routing_rank = ?1 WHERE legacy_account_id = ?2",
            params![sort_order as i64, id],
        )?;
    }
    let zen_models_json = serde_json::to_string(&record.zen_catalog.models)?;
    tx.execute(
        "INSERT INTO provider_model_catalogs
         (provider_id, models_json, refreshed_at, source_url)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(provider_id) DO UPDATE SET
             models_json = excluded.models_json,
             refreshed_at = excluded.refreshed_at,
             source_url = excluded.source_url",
        params![
            OPENCODE_ZEN_FREE_PROVIDER_ID,
            zen_models_json,
            record
                .zen_catalog
                .refreshed_at
                .map(|value| value.to_rfc3339()),
            record.zen_catalog.source_url,
        ],
    )?;

    for (scope, row) in &record.provider_contracts.scopes {
        anyhow::ensure!(
            scope.kind() == crate::provider_contracts::ContractScopeKind::Provider,
            "node migration only accepts Provider contract scopes"
        );
        upsert_contract_catalog_on(
            &tx,
            scope,
            &row.catalog_models,
            row.catalog_refreshed_at,
            &row.catalog_source,
            &row.catalog_source_url,
            row.updated_at,
        )?;
        tx.execute(
            "DELETE FROM provider_contract_model_protocols
             WHERE scope_kind = ?1 AND scope_id = ?2",
            params![scope.kind_str(), scope.id()],
        )?;
        tx.execute(
            "DELETE FROM provider_contract_model_protocol_overrides
             WHERE scope_kind = ?1 AND scope_id = ?2",
            params![scope.kind_str(), scope.id()],
        )?;
        for evidence in record
            .provider_contracts
            .evidence
            .get(scope)
            .into_iter()
            .flatten()
        {
            upsert_model_protocol_row_on(&tx, evidence)?;
        }
        for override_row in record
            .provider_contracts
            .overrides
            .get(scope)
            .into_iter()
            .flatten()
        {
            set_model_protocol_override_on(
                &tx,
                scope,
                &override_row.model_id,
                override_row.protocol,
                override_row.state,
                override_row.updated_at,
            )?;
        }
        tx.execute(
            "DELETE FROM provider_model_protocol_preferences WHERE provider_id=?1",
            [scope.id()],
        )?;
        set_model_protocol_preferences_on(
            &tx,
            scope,
            record
                .provider_contracts
                .preferences
                .get(scope)
                .map(Vec::as_slice)
                .unwrap_or(&[]),
        )?;
    }

    if let Some(snapshot) = &record.identity_snapshot {
        identity::restore_imported_identity_snapshot_on(&tx, snapshot, &imported_account_ids)?;
    }
    let imported_custom_scopes: Vec<(&str, &[AccountModelCapabilityInput])> = record
        .accounts
        .iter()
        .filter(|account| {
            is_custom_api(&account.account.provider_id)
                && !platform_catalog_linked_account_ids
                    .contains(&account.account.id.to_ascii_lowercase())
        })
        .map(|account| (account.account.id.as_str(), account.capabilities.as_slice()))
        .collect();
    custom_store::narrow_imported_custom_scopes(&tx, &imported_custom_scopes)?;
    ensure_dynamic_singleton_accounts_on(&tx)?;
    lifecycle::sqlite_foreign_key_check(&tx)?;
    // The callback reads through this same SQLite connection, so it sees
    // the uncommitted merged rows. Every fallible runtime construction
    // step must finish before commit; the caller installs the returned
    // snapshots only after this transaction succeeds.
    for controls in &record.destination_controls {
        if let ocg_domain::destination::LegacyDestinationRef::Builtin(provider_id) =
            &controls.legacy
        {
            let id = destination_store::ensure_builtin_destination(&tx, provider_id)?;
            anyhow::ensure!(
                id == controls.id,
                "imported builtin destination has an incompatible identity"
            );
        }
    }
    for scope in record.provider_contracts.scopes.keys() {
        if let ContractScope::Provider(provider_id) = scope
            && builtin_provider(provider_id).is_some()
        {
            destination_store::ensure_builtin_destination(&tx, provider_id)?;
        }
    }
    db.refresh_destination_shadow()?;
    destination_store::sync_builtin_catalogs(db)?;
    for scope in record.provider_contracts.scopes.keys() {
        destination_store::apply_scope_controls(db, scope, None)?;
    }
    for controls in &record.destination_controls {
        if controls.auth_scheme == AuthScheme::None
            && controls.adapter == ocg_domain::destination::AdapterKind::Http
        {
            let count: i64 = tx.query_row("SELECT COUNT(*) FROM credentials WHERE destination_id = ?1 AND COALESCE(credential_purpose, 'inference') = 'inference'", [&controls.id], |row| row.get(0))?;
            anyhow::ensure!(
                count <= 1,
                "no-auth destination cannot restore multiple execution credentials"
            );
        }
        anyhow::ensure!(
            tx.execute(
                "UPDATE destinations SET enabled = ?2, model_resolution = ?3 WHERE id = ?1",
                params![
                    controls.id,
                    i64::from(controls.enabled),
                    controls.model_resolution.as_str()
                ],
            )? == 1,
            "imported destination controls reference a missing destination"
        );
        if controls.adapter == ocg_domain::destination::AdapterKind::Http {
            let mut restored = controls.clone();
            if restored.protocol_routes.is_empty()
                && let Some(routes) = original_routes
                    .get(&controls.id)
                    .filter(|routes| !routes.is_empty())
            {
                restored.protocol_routes = routes.clone();
                restored.protocols = routes.iter().map(|route| route.protocol).collect();
            }
            http_routes::validate_loaded_destination(&restored)?;
            tx.execute(
                "UPDATE destinations SET protocol_routes_json = ?2, protocols_json = ?3, base_url = ?4, auth_scheme = ?5 WHERE id = ?1",
                params![restored.id, http_routes::encode_protocol_routes_json(&restored.protocol_routes)?,
                    serde_json::to_string(&restored.protocols)?, restored.base_url, restored.auth_scheme.as_str()],
            )?;
        }
        let mut catalog = original_catalogs
            .get(&controls.id)
            .cloned()
            .unwrap_or_default();
        for incoming in &controls.catalog {
            if matches!(
                controls.legacy,
                ocg_domain::destination::LegacyDestinationRef::Builtin(_)
            ) {
                anyhow::ensure!(
                    !catalog.iter().any(|model| model
                        .public_model
                        .eq_ignore_ascii_case(&incoming.public_model)
                        && !model
                            .upstream_model
                            .eq_ignore_ascii_case(&incoming.upstream_model)),
                    "imported model controls conflict with the destination mapping"
                );
                if let Some(existing) = catalog.iter_mut().find(|model| {
                    model
                        .upstream_model
                        .eq_ignore_ascii_case(&incoming.upstream_model)
                }) {
                    *existing = incoming.clone();
                } else {
                    catalog.push(incoming.clone());
                }
                continue;
            }
            if let Some(existing) = catalog.iter_mut().find(|model| {
                model
                    .public_model
                    .eq_ignore_ascii_case(&incoming.public_model)
            }) {
                anyhow::ensure!(
                    existing
                        .upstream_model
                        .eq_ignore_ascii_case(&incoming.upstream_model),
                    "imported model controls conflict with the destination mapping"
                );
                anyhow::ensure!(
                    existing.upstream_override.is_none()
                        || incoming.upstream_override.is_none()
                        || existing.upstream_override == incoming.upstream_override,
                    "imported model controls conflict with the destination route"
                );
                *existing = incoming.clone();
            } else {
                catalog.push(incoming.clone());
            }
        }
        destination_store::replace_destination_catalog(&tx, &controls.id, &catalog)?;
    }
    destination_commands::reconcile_imported_http_grants_on(db, record, &before_import)?;
    let runtime = prepare_runtime(db)?;
    tx.commit()?;
    Ok(runtime)
}
