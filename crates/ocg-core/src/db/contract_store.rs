//! Persisted provider-contract catalog, protocol evidence, and overrides.
//!
//! _on helpers participate in the caller transaction. Transaction-owning
//! Database methods stay on the root and call into this module.

use super::*;

pub(super) fn parse_rfc3339_opt(
    value: Option<String>,
    column: usize,
) -> rusqlite::Result<Option<DateTime<Utc>>> {
    value
        .map(|text| parse_rfc3339_column(text, column))
        .transpose()
}

pub(super) fn parse_rfc3339_column(
    value: String,
    column: usize,
) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(column, Type::Text, Box::new(error))
        })
}

pub(super) fn scope_from_row(kind: &str, id: &str) -> rusqlite::Result<ContractScope> {
    ContractScope::parse(kind, id).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            Type::Text,
            Box::new(std::io::Error::other(error)),
        )
    })
}

pub(super) fn persist_scope_from_row(row: &Row<'_>) -> rusqlite::Result<PersistedScopeRow> {
    let kind: String = row.get(0)?;
    let id: String = row.get(1)?;
    let models_json: String = row.get(2)?;
    let models: Vec<String> = serde_json::from_str(&models_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(2, Type::Text, Box::new(error))
    })?;
    Ok(PersistedScopeRow {
        scope: scope_from_row(&kind, &id)?,
        catalog_models: models,
        catalog_refreshed_at: parse_rfc3339_opt(row.get(3)?, 3)?,
        catalog_source: row.get(4)?,
        catalog_source_url: row.get(5)?,
        revision: row.get::<_, i64>(6)? as u64,
        updated_at: parse_rfc3339_column(row.get(7)?, 7)?,
    })
}

pub(super) fn persist_evidence_from_row(row: &Row<'_>) -> rusqlite::Result<PersistedModelProtocol> {
    let kind: String = row.get(0)?;
    let id: String = row.get(1)?;
    let protocol_value: String = row.get(3)?;
    let protocol = UpstreamProtocolKind::try_from(protocol_value.as_str()).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            3,
            Type::Text,
            Box::new(std::io::Error::other(error.to_string())),
        )
    })?;
    let source_value: String = row.get(4)?;
    let source = ContractEvidenceSource::try_from(source_value.as_str()).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            4,
            Type::Text,
            Box::new(std::io::Error::other(error)),
        )
    })?;
    let last_probe: Option<String> = row.get(7)?;
    let last_probe_result = last_probe
        .map(|value| {
            ProbeResultKind::try_from(value.as_str()).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    7,
                    Type::Text,
                    Box::new(std::io::Error::other(error)),
                )
            })
        })
        .transpose()?;
    Ok(PersistedModelProtocol {
        scope: scope_from_row(&kind, &id)?,
        model_id: row.get(2)?,
        protocol,
        source,
        verified_at: parse_rfc3339_opt(row.get(5)?, 5)?,
        observed_at: parse_rfc3339_opt(row.get(6)?, 6)?,
        last_probe_result,
        last_probe_at: parse_rfc3339_opt(row.get(8)?, 8)?,
        last_probe_error: row.get(9)?,
    })
}

pub(super) fn persist_override_from_row(
    row: &Row<'_>,
) -> rusqlite::Result<PersistedModelProtocolOverride> {
    let kind: String = row.get(0)?;
    let id: String = row.get(1)?;
    let protocol_value: String = row.get(3)?;
    let protocol = UpstreamProtocolKind::try_from(protocol_value.as_str()).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            3,
            Type::Text,
            Box::new(std::io::Error::other(error.to_string())),
        )
    })?;
    let state_value: String = row.get(4)?;
    let state = ProtocolOverrideState::try_from(state_value.as_str()).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            4,
            Type::Text,
            Box::new(std::io::Error::other(error)),
        )
    })?;
    Ok(PersistedModelProtocolOverride {
        scope: scope_from_row(&kind, &id)?,
        model_id: row.get(2)?,
        protocol,
        state,
        updated_at: parse_rfc3339_column(row.get(5)?, 5)?,
    })
}

pub(super) fn load_scope_on(
    conn: &Connection,
    scope: &ContractScope,
) -> Result<Option<PersistedScopeRow>> {
    conn.query_row(
        "SELECT scope_kind, scope_id, catalog_models_json, catalog_refreshed_at,
                catalog_source, catalog_source_url, revision, updated_at
         FROM provider_contract_scopes
         WHERE scope_kind = ?1 AND scope_id = ?2",
        params![scope.kind_str(), scope.id()],
        persist_scope_from_row,
    )
    .optional()
    .map_err(Into::into)
}

pub(super) fn load_scope_evidence_on(
    conn: &Connection,
    scope: &ContractScope,
) -> Result<Vec<PersistedModelProtocol>> {
    let mut stmt = conn.prepare(
        "SELECT scope_kind, scope_id, model_id, protocol, source, verified_at,
                observed_at, last_probe_result, last_probe_at, last_probe_error
         FROM provider_contract_model_protocols
         WHERE scope_kind = ?1 AND scope_id = ?2",
    )?;
    let rows = stmt.query_map(
        params![scope.kind_str(), scope.id()],
        persist_evidence_from_row,
    )?;
    let mut evidence = Vec::new();
    for row in rows {
        evidence.push(row?);
    }
    Ok(evidence)
}

pub(super) fn preference_protocol_allowed(
    conn: &Connection,
    scope: &ContractScope,
    model_id: &str,
    protocol: UpstreamProtocolKind,
) -> Result<bool> {
    if scope.kind_str() != "provider"
        || !crate::provider_contracts::selectable_model_protocol(scope.id(), protocol)
    {
        return Ok(false);
    }
    let Some(descriptor) = crate::provider_contracts::provider_scope_descriptor(scope.id()) else {
        return Ok(true);
    };
    let evidence = load_scope_evidence_on(conn, scope)?;
    Ok(crate::provider_contracts::admitted_protocols(
        descriptor.kind,
        descriptor.protocol_probe,
        model_id,
        &evidence,
    )
    .contains(&protocol))
}

pub(super) fn set_model_protocol_preferences_on(
    conn: &Connection,
    scope: &ContractScope,
    preferences: &[(String, UpstreamProtocolKind)],
) -> Result<()> {
    let mut seen = HashSet::new();
    for (model_id, protocol) in preferences {
        let model_key = model_id.trim().to_ascii_lowercase();
        anyhow::ensure!(
            preference_protocol_allowed(conn, scope, model_id, *protocol)?
                && !model_key.is_empty()
                && seen.insert(model_key.clone()),
            "invalid or duplicate model protocol preference"
        );
        conn.execute(
            "INSERT INTO provider_model_protocol_preferences(provider_id,model_id,protocol)
             VALUES(?1,?2,?3) ON CONFLICT(provider_id,model_id) DO UPDATE SET protocol=excluded.protocol",
            params![scope.id(), model_key, protocol.as_str()],
        )?;
    }
    Ok(())
}

pub(super) fn ensure_contract_scope_row(
    conn: &Connection,
    scope: &ContractScope,
    now: DateTime<Utc>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO provider_contract_scopes (
            scope_kind, scope_id, catalog_models_json, catalog_refreshed_at,
            catalog_source, catalog_source_url,
            revision, updated_at
         ) VALUES (?1, ?2, '[]', NULL, '', '', 1, ?3)
         ON CONFLICT(scope_kind, scope_id) DO NOTHING",
        params![scope.kind_str(), scope.id(), now.to_rfc3339()],
    )?;
    Ok(())
}

pub(super) fn purge_removed_catalog_model_on(
    conn: &Connection,
    scope: &ContractScope,
    model_id: &str,
) -> Result<()> {
    conn.execute(
        "DELETE FROM provider_contract_model_protocol_overrides
         WHERE scope_kind = ?1 AND scope_id = ?2 AND model_id = ?3",
        params![scope.kind_str(), scope.id(), model_id],
    )?;
    conn.execute(
        "DELETE FROM provider_contract_model_protocols
         WHERE scope_kind = ?1 AND scope_id = ?2 AND model_id = ?3",
        params![scope.kind_str(), scope.id(), model_id],
    )?;
    if scope.kind_str() == SCOPE_KIND_PROVIDER {
        conn.execute(
            "DELETE FROM provider_model_protocol_preferences
             WHERE provider_id = ?1 AND model_id = ?2",
            params![scope.id(), model_id],
        )?;
    }
    Ok(())
}

pub(super) fn upsert_contract_catalog_on(
    conn: &Connection,
    scope: &ContractScope,
    models: &[String],
    refreshed_at: Option<DateTime<Utc>>,
    source: &str,
    source_url: &str,
    now: DateTime<Utc>,
) -> Result<()> {
    let models_json = serde_json::to_string(models)?;
    conn.execute(
        "INSERT INTO provider_contract_scopes (
            scope_kind, scope_id, catalog_models_json, catalog_refreshed_at,
            catalog_source, catalog_source_url,
            revision, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7)
         ON CONFLICT(scope_kind, scope_id) DO UPDATE SET
            catalog_models_json = excluded.catalog_models_json,
            catalog_refreshed_at = excluded.catalog_refreshed_at,
            catalog_source = excluded.catalog_source,
            catalog_source_url = excluded.catalog_source_url,
            revision = provider_contract_scopes.revision + 1,
            updated_at = excluded.updated_at",
        params![
            scope.kind_str(),
            scope.id(),
            models_json,
            refreshed_at.map(|value| value.to_rfc3339()),
            source,
            source_url,
            now.to_rfc3339(),
        ],
    )?;
    Ok(())
}

pub(super) fn set_model_protocol_override_on(
    conn: &Connection,
    scope: &ContractScope,
    model_id: &str,
    protocol: UpstreamProtocolKind,
    state: ProtocolOverrideState,
    now: DateTime<Utc>,
) -> Result<()> {
    match state {
        ProtocolOverrideState::Auto => {
            conn.execute(
                "DELETE FROM provider_contract_model_protocol_overrides
                 WHERE scope_kind = ?1 AND scope_id = ?2 AND model_id = ?3 AND protocol = ?4",
                params![scope.kind_str(), scope.id(), model_id, protocol.as_str()],
            )?;
        }
        ProtocolOverrideState::ForceOn | ProtocolOverrideState::ForceOff => {
            conn.execute(
                "INSERT OR REPLACE INTO provider_contract_model_protocol_overrides
                 (scope_kind, scope_id, model_id, protocol, state, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    scope.kind_str(),
                    scope.id(),
                    model_id,
                    protocol.as_str(),
                    state.as_str(),
                    now.to_rfc3339(),
                ],
            )?;
        }
    }
    Ok(())
}

pub(super) fn clear_provider_protocol_judgments_on(
    conn: &Connection,
    scope: &ContractScope,
) -> Result<()> {
    conn.execute(
        "DELETE FROM provider_model_protocol_preferences WHERE provider_id=?1",
        [scope.id()],
    )?;
    conn.execute(
        "DELETE FROM provider_contract_model_protocols
         WHERE scope_kind = ?1 AND scope_id = ?2",
        params![scope.kind_str(), scope.id()],
    )?;
    conn.execute(
        "DELETE FROM provider_contract_model_protocol_overrides
         WHERE scope_kind = ?1 AND scope_id = ?2",
        params![scope.kind_str(), scope.id()],
    )?;
    Ok(())
}

// Capture whole-model OFF before refresh creates new catalog rows. New evidence
// must not enable a previously disabled model, while genuinely new ids stay Auto.
pub(super) fn preserve_disabled_catalog_models_on(
    db: &Database,
    scope: &ContractScope,
    now: DateTime<Utc>,
) -> Result<()> {
    let ContractScope::Provider(provider) = scope else {
        return Ok(());
    };
    let id = ocg_domain::destination::destination_id_for_builtin(provider);
    let persisted = db.load_persisted_contracts()?;
    let explicitly_disabled: HashSet<_> = persisted
        .overrides
        .get(scope)
        .into_iter()
        .flatten()
        .filter(|row| row.state == ProtocolOverrideState::ForceOff)
        .map(|row| row.model_id.to_ascii_lowercase())
        .collect();
    let contracts = crate::provider_contracts::build_effective_contracts(
        &db.zen_free_model_catalog()?.unwrap_or_default(),
        &[],
        persisted,
    );
    for model in destination_store::load_destination_catalog(&db.conn, &id)? {
        if !model.enabled {
            // A model with no evidence and no saved disable is merely awaiting
            // official discovery. Do not turn that temporary state into consent.
            let deliberately_disabled = explicitly_disabled
                .contains(&model.upstream_model.to_ascii_lowercase())
                || contracts
                    .scope(scope)
                    .and_then(|s| s.model(&model.upstream_model))
                    .is_some_and(|m| m.protocols.values().any(|p| p.available));
            if !deliberately_disabled {
                continue;
            }
            for protocol in [
                UpstreamProtocolKind::ChatCompletions,
                UpstreamProtocolKind::Messages,
                UpstreamProtocolKind::Responses,
            ] {
                set_model_protocol_override_on(
                    &db.conn,
                    scope,
                    &model.upstream_model,
                    protocol,
                    ProtocolOverrideState::ForceOff,
                    now,
                )?;
            }
        }
    }
    Ok(())
}

pub(super) fn apply_official_protocol_baseline_on(
    conn: &Connection,
    scope: &ContractScope,
    current_models: &[String],
    baseline: &crate::official_protocols::OfficialProtocolBaseline,
    now: DateTime<Utc>,
    force_off_extras: bool,
) -> Result<()> {
    let evidence = load_scope_evidence_on(conn, scope)?;
    let mut preferences = Vec::new();
    for model_id in current_models {
        let Some(protocols) = baseline.protocols_for(scope.id(), model_id) else {
            continue;
        };
        let Some(protocol) = baseline.protocol_for(scope.id(), model_id) else {
            continue;
        };
        let protocols_json = serde_json::to_string(&protocols)?;
        // Old static declarations may also carry independent probe history.
        // Demote those declarations, keeping their diagnostics, before pruning.
        conn.execute(
            "UPDATE provider_contract_model_protocols SET source = 'probe_observed'
             WHERE scope_kind = ?1 AND scope_id = ?2 AND model_id = ?3
               AND source = 'static' AND protocol NOT IN (SELECT value FROM json_each(?4))
               AND (verified_at IS NOT NULL OR observed_at IS NOT NULL
                    OR last_probe_result IS NOT NULL OR last_probe_at IS NOT NULL
                    OR last_probe_error IS NOT NULL)",
            params![scope.kind_str(), scope.id(), model_id, protocols_json],
        )?;
        conn.execute(
            "DELETE FROM provider_contract_model_protocols
             WHERE scope_kind = ?1 AND scope_id = ?2 AND model_id = ?3
               AND source = 'static' AND protocol NOT IN (SELECT value FROM json_each(?4))",
            params![scope.kind_str(), scope.id(), model_id, protocols_json],
        )?;
        for &declared_protocol in &protocols {
            let mut row = evidence
                .iter()
                .find(|row| row.model_id == *model_id && row.protocol == declared_protocol)
                .cloned()
                .unwrap_or(PersistedModelProtocol {
                    scope: scope.clone(),
                    model_id: model_id.clone(),
                    protocol: declared_protocol,
                    source: ContractEvidenceSource::Static,
                    verified_at: None,
                    observed_at: None,
                    last_probe_result: None,
                    last_probe_at: None,
                    last_probe_error: None,
                });
            row.source = ContractEvidenceSource::Static;
            upsert_model_protocol_row_on(conn, &row)?;
        }
        let has_preference: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM provider_model_protocol_preferences
             WHERE provider_id = ?1 AND model_id = ?2)",
            params![scope.id(), model_id.trim().to_ascii_lowercase()],
            |row| row.get(0),
        )?;
        if (force_off_extras || !has_preference)
            && preference_protocol_allowed(conn, scope, model_id, protocol)?
        {
            preferences.push((model_id.clone(), protocol));
        }
        if force_off_extras {
            for extra in UpstreamProtocolKind::ALL {
                if !protocols.contains(&extra) {
                    set_model_protocol_override_on(
                        conn,
                        scope,
                        model_id,
                        extra,
                        ProtocolOverrideState::ForceOff,
                        now,
                    )?;
                }
            }
        }
    }
    if !preferences.is_empty() {
        set_model_protocol_preferences_on(conn, scope, &preferences)?;
    }
    Ok(())
}

pub(super) fn upsert_model_protocol_row_on(
    conn: &Connection,
    row: &PersistedModelProtocol,
) -> Result<()> {
    conn.execute(
        "INSERT INTO provider_contract_model_protocols (
            scope_kind, scope_id, model_id, protocol, source, verified_at,
            observed_at, last_probe_result, last_probe_at, last_probe_error
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(scope_kind, scope_id, model_id, protocol) DO UPDATE SET
            source = excluded.source,
            verified_at = excluded.verified_at,
            observed_at = excluded.observed_at,
            last_probe_result = excluded.last_probe_result,
            last_probe_at = excluded.last_probe_at,
            last_probe_error = excluded.last_probe_error",
        params![
            row.scope.kind_str(),
            row.scope.id(),
            row.model_id,
            row.protocol.as_str(),
            row.source.as_str(),
            row.verified_at.map(|value| value.to_rfc3339()),
            row.observed_at.map(|value| value.to_rfc3339()),
            row.last_probe_result.map(|value| value.as_str()),
            row.last_probe_at.map(|value| value.to_rfc3339()),
            row.last_probe_error,
        ],
    )?;
    Ok(())
}

/// Drop probe observations for one contract scope and bump its revision.
/// Explicit protocol overrides stay; they are operator policy, not probe evidence.
pub(crate) fn invalidate_probe_evidence_on(
    conn: &Connection,
    scope: &ContractScope,
    now: DateTime<Utc>,
) -> Result<()> {
    if !table_exists(conn, "provider_contract_model_protocols")? {
        return Ok(());
    }
    conn.execute(
        "DELETE FROM provider_contract_model_protocols
         WHERE scope_kind = ?1 AND scope_id = ?2",
        params![scope.kind_str(), scope.id()],
    )?;
    if table_exists(conn, "provider_contract_scopes")? {
        bump_scope_revision_on(conn, scope, now)?;
    }
    Ok(())
}

pub(super) fn bump_scope_revision_on(
    conn: &Connection,
    scope: &ContractScope,
    now: DateTime<Utc>,
) -> Result<u64> {
    ensure_contract_scope_row(conn, scope, now)?;
    conn.execute(
        "UPDATE provider_contract_scopes
         SET revision = revision + 1, updated_at = ?3
         WHERE scope_kind = ?1 AND scope_id = ?2",
        params![scope.kind_str(), scope.id(), now.to_rfc3339()],
    )?;
    let revision = conn.query_row(
        "SELECT revision FROM provider_contract_scopes
         WHERE scope_kind = ?1 AND scope_id = ?2",
        params![scope.kind_str(), scope.id()],
        |row| row.get::<_, i64>(0),
    )?;
    Ok(revision as u64)
}

impl Database {
    pub fn insert_pricing_snapshot(&self, snapshot: &PricingSnapshot) -> Result<()> {
        let snapshot_json = serde_json::to_string(snapshot)?;
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT OR IGNORE INTO pricing_snapshots
             (revision, activated_at, document_updated_at, source_url, content_hash, snapshot_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                snapshot.revision,
                snapshot.activated_at,
                snapshot.document_updated_at,
                snapshot.source_url,
                snapshot.content_hash,
                snapshot_json,
            ],
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO provider_pricing_snapshots
             (provider_id, revision, activated_at, document_updated_at,
              source_url, content_hash, snapshot_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                OPENCODE_PROVIDER_ID,
                snapshot.revision,
                snapshot.activated_at,
                snapshot.document_updated_at,
                snapshot.source_url,
                snapshot.content_hash,
                snapshot_json,
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn zen_free_model_catalog(
        &self,
    ) -> Result<Option<crate::kernel::zen::ZenFreeModelCatalog>> {
        self.conn
            .query_row(
                "SELECT models_json, refreshed_at, source_url
                 FROM provider_model_catalogs
                 WHERE provider_id = ?1",
                params![OPENCODE_ZEN_FREE_PROVIDER_ID],
                |row| {
                    let models_json: String = row.get(0)?;
                    let refreshed_at: Option<String> = row.get(1)?;
                    let models = serde_json::from_str(&models_json).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(0, Type::Text, Box::new(error))
                    })?;
                    let refreshed_at = refreshed_at
                        .map(|value| {
                            DateTime::parse_from_rfc3339(&value)
                                .map(|value| value.with_timezone(&Utc))
                                .map_err(|error| {
                                    rusqlite::Error::FromSqlConversionFailure(
                                        1,
                                        Type::Text,
                                        Box::new(error),
                                    )
                                })
                        })
                        .transpose()?;
                    Ok(crate::kernel::zen::ZenFreeModelCatalog {
                        models,
                        refreshed_at,
                        source_url: row.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn set_zen_free_model_catalog(
        &self,
        catalog: &crate::kernel::zen::ZenFreeModelCatalog,
    ) -> Result<()> {
        self.set_zen_free_model_catalog_preserving_settings(catalog)
    }

    /// Persist the Zen Free catalog without inventing protocol overrides.
    /// Saved force_on / force_off rows and preferred-protocol choices stay as
    /// written; new IDs follow effective-contract defaults from evidence.
    pub fn set_zen_free_model_catalog_preserving_settings(
        &self,
        catalog: &crate::kernel::zen::ZenFreeModelCatalog,
    ) -> Result<()> {
        let now = Utc::now();
        let models_json = serde_json::to_string(&catalog.models)?;
        let refreshed_at = catalog.refreshed_at.map(|value| value.to_rfc3339());
        let tx = self
            .conn
            .is_autocommit()
            .then(|| self.conn.unchecked_transaction())
            .transpose()?;
        preserve_disabled_catalog_models_on(
            self,
            &ContractScope::provider(OPENCODE_ZEN_FREE_PROVIDER_ID),
            now,
        )?;
        self.conn.execute(
            "INSERT INTO provider_model_catalogs
             (provider_id, models_json, refreshed_at, source_url)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(provider_id) DO UPDATE SET
                 models_json = excluded.models_json,
                 refreshed_at = excluded.refreshed_at,
                 source_url = excluded.source_url",
            params![
                OPENCODE_ZEN_FREE_PROVIDER_ID,
                models_json,
                refreshed_at,
                catalog.source_url,
            ],
        )?;
        upsert_contract_catalog_on(
            &self.conn,
            &ContractScope::provider(OPENCODE_ZEN_FREE_PROVIDER_ID),
            &catalog.models,
            catalog.refreshed_at,
            CATALOG_SOURCE_OFFICIAL_ZEN,
            &catalog.source_url,
            now,
        )?;
        destination_store::refresh_builtin_catalog(
            self,
            &ContractScope::provider(OPENCODE_ZEN_FREE_PROVIDER_ID),
        )?;
        if let Some(tx) = tx {
            tx.commit()?;
        }
        Ok(())
    }

    pub fn load_persisted_contracts(&self) -> Result<PersistedContracts> {
        let mut persisted = PersistedContracts::default();
        {
            let mut stmt = self.conn.prepare(
                "SELECT provider_id, model_id, protocol FROM provider_model_protocol_preferences ORDER BY provider_id, model_id",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?;
            for row in rows {
                let (provider_id, model_id, protocol) = row?;
                let protocol = UpstreamProtocolKind::try_from(protocol.as_str())?;
                persisted
                    .preferences
                    .entry(ContractScope::provider(&provider_id))
                    .or_default()
                    .push((model_id, protocol));
            }
        }
        {
            let mut stmt = self.conn.prepare(
                "SELECT scope_kind, scope_id, catalog_models_json, catalog_refreshed_at,
                        catalog_source, catalog_source_url, revision, updated_at
                 FROM provider_contract_scopes",
            )?;
            let rows = stmt.query_map([], persist_scope_from_row)?;
            for row in rows {
                let row = row?;
                persisted.scopes.insert(row.scope.clone(), row);
            }
        }
        {
            let mut stmt = self.conn.prepare(
                "SELECT scope_kind, scope_id, model_id, protocol, source, verified_at,
                        observed_at, last_probe_result, last_probe_at, last_probe_error
                 FROM provider_contract_model_protocols",
            )?;
            let rows = stmt.query_map([], persist_evidence_from_row)?;
            for row in rows {
                let row = row?;
                persisted
                    .evidence
                    .entry(row.scope.clone())
                    .or_default()
                    .push(row);
            }
        }
        {
            let mut stmt = self.conn.prepare(
                "SELECT scope_kind, scope_id, model_id, protocol, state, updated_at
                 FROM provider_contract_model_protocol_overrides",
            )?;
            let rows = stmt.query_map([], persist_override_from_row)?;
            for row in rows {
                let row = row?;
                persisted
                    .overrides
                    .entry(row.scope.clone())
                    .or_default()
                    .push(row);
            }
        }
        Ok(persisted)
    }

    pub fn load_persisted_scope(&self, scope: &ContractScope) -> Result<Option<PersistedScopeRow>> {
        self.conn
            .query_row(
                "SELECT scope_kind, scope_id, catalog_models_json, catalog_refreshed_at,
                        catalog_source, catalog_source_url, revision, updated_at
                 FROM provider_contract_scopes
                 WHERE scope_kind = ?1 AND scope_id = ?2",
                params![scope.kind_str(), scope.id()],
                persist_scope_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn set_contract_catalog(
        &self,
        scope: &ContractScope,
        models: &[String],
        refreshed_at: Option<DateTime<Utc>>,
        source: &str,
        source_url: &str,
        now: DateTime<Utc>,
    ) -> Result<PersistedScopeRow> {
        let tx = self.conn.unchecked_transaction()?;
        upsert_contract_catalog_on(&tx, scope, models, refreshed_at, source, source_url, now)?;
        let row = load_scope_on(&tx, scope)?
            .ok_or_else(|| anyhow::anyhow!("contract scope was not persisted"))?;
        destination_store::refresh_builtin_catalog(self, scope)?;
        tx.commit()?;
        Ok(row)
    }

    /// Drop models from the persisted local catalog snapshot.
    ///
    /// Source metadata and `refreshed_at` stay as last written. An official
    /// refresh may add the same IDs back as new catalog rows. Satellite
    /// override, preference, and probe rows for the removed IDs are deleted
    /// so they cannot resurrect the models.
    pub fn remove_contract_catalog_models(
        &self,
        scope: &ContractScope,
        model_ids: &[String],
        now: DateTime<Utc>,
    ) -> Result<PersistedScopeRow> {
        anyhow::ensure!(
            !model_ids.is_empty(),
            "catalog model remove batch must be nonempty"
        );
        let tx = self.conn.unchecked_transaction()?;
        let current = load_scope_on(&tx, scope)?
            .ok_or_else(|| anyhow::anyhow!("contract scope was not persisted"))?;
        let known: HashSet<&str> = current.catalog_models.iter().map(String::as_str).collect();
        let mut seen = HashSet::new();
        for model_id in model_ids {
            anyhow::ensure!(
                known.contains(model_id.as_str()) && seen.insert(model_id.as_str()),
                "catalog model remove must name distinct models from the saved catalog"
            );
        }
        let remove: HashSet<&str> = model_ids.iter().map(String::as_str).collect();
        let remaining: Vec<String> = current
            .catalog_models
            .iter()
            .filter(|model_id| !remove.contains(model_id.as_str()))
            .cloned()
            .collect();
        upsert_contract_catalog_on(
            &tx,
            scope,
            &remaining,
            current.catalog_refreshed_at,
            &current.catalog_source,
            &current.catalog_source_url,
            now,
        )?;
        for model_id in model_ids {
            purge_removed_catalog_model_on(&tx, scope, model_id)?;
        }
        destination_store::remove_scope_models(self, scope, model_ids)?;
        let row = load_scope_on(&tx, scope)?
            .ok_or_else(|| anyhow::anyhow!("contract scope was not persisted"))?;
        self.refresh_destination_shadow()?;
        tx.commit()?;
        Ok(row)
    }

    /// Replace the saved catalog snapshot without writing protocol overrides.
    /// Existing force_on / force_off rows and preferences are left untouched,
    /// including historical auto-off rows that cannot be distinguished from
    /// an administrator's manual off.
    pub fn refresh_contract_catalog_preserving_settings(
        &self,
        scope: &ContractScope,
        models: &[String],
        refreshed_at: DateTime<Utc>,
        source: &str,
        source_url: &str,
    ) -> Result<PersistedScopeRow> {
        let tx = self.conn.unchecked_transaction()?;
        let first_goat_refresh = scope.id() == COMMAND_CODE_PROVIDER_ID
            && scope.kind_str() == SCOPE_KIND_PROVIDER
            && load_scope_on(&tx, scope)?.is_none_or(|saved| saved.catalog_refreshed_at.is_none());
        preserve_disabled_catalog_models_on(self, scope, refreshed_at)?;
        upsert_contract_catalog_on(
            &tx,
            scope,
            models,
            Some(refreshed_at),
            source,
            source_url,
            refreshed_at,
        )?;
        if first_goat_refresh {
            // The public Provider directory also lists models outside the GOAT
            // plan. Seed only the plan's included cohort on the first refresh;
            // later discoveries keep the normal auto-on behavior. Persist this
            // as ordinary model switches so a later refresh preserves it.
            for model in models {
                if command_code_goat_includes_model(model) {
                    continue;
                }
                for protocol in UpstreamProtocolKind::ALL {
                    let has_saved_switch: bool = tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM provider_contract_model_protocol_overrides
                         WHERE scope_kind = ?1 AND scope_id = ?2
                           AND model_id = ?3 COLLATE NOCASE AND protocol = ?4)",
                        params![scope.kind_str(), scope.id(), model, protocol.as_str()],
                        |row| row.get(0),
                    )?;
                    if has_saved_switch {
                        continue;
                    }
                    set_model_protocol_override_on(
                        &tx,
                        scope,
                        model,
                        protocol,
                        ProtocolOverrideState::ForceOff,
                        refreshed_at,
                    )?;
                }
            }
        }
        let row = load_scope_on(&tx, scope)?
            .ok_or_else(|| anyhow::anyhow!("contract scope was not persisted"))?;
        destination_store::refresh_builtin_catalog(self, scope)?;
        tx.commit()?;
        Ok(row)
    }

    pub fn set_model_protocol_overrides(
        &self,
        scope: &ContractScope,
        rows: &[(String, UpstreamProtocolKind, ProtocolOverrideState)],
        now: DateTime<Utc>,
    ) -> Result<PersistedScopeRow> {
        self.set_model_protocol_settings(scope, rows, &[], now)
    }

    /// `preferences` is merged into the saved preferred protocol choices; an empty
    /// slice leaves existing preferences untouched. Only
    /// `reset_provider_static_model_protocols` clears them.
    pub fn set_model_protocol_settings(
        &self,
        scope: &ContractScope,
        rows: &[(String, UpstreamProtocolKind, ProtocolOverrideState)],
        preferences: &[(String, UpstreamProtocolKind)],
        now: DateTime<Utc>,
    ) -> Result<PersistedScopeRow> {
        self.set_model_protocol_settings_authorized(scope, rows, preferences, now, &[])
    }

    pub fn set_model_protocol_settings_authorized(
        &self,
        scope: &ContractScope,
        rows: &[(String, UpstreamProtocolKind, ProtocolOverrideState)],
        preferences: &[(String, UpstreamProtocolKind)],
        now: DateTime<Utc>,
        authorize_credential_ids: &[String],
    ) -> Result<PersistedScopeRow> {
        anyhow::ensure!(
            !rows.is_empty(),
            "model protocol override batch must be nonempty"
        );
        let tx = self.conn.unchecked_transaction()?;
        ensure_contract_scope_row(&tx, scope, now)?;
        for (model_id, protocol, state) in rows {
            set_model_protocol_override_on(&tx, scope, model_id, *protocol, *state, now)?;
        }
        set_model_protocol_preferences_on(&tx, scope, preferences)?;
        if !authorize_credential_ids.is_empty() {
            let protocols: Vec<_> = rows
                .iter()
                .filter(|(_, _, state)| *state == ProtocolOverrideState::ForceOn)
                .map(|(_, protocol, _)| *protocol)
                .collect();
            identity::authorize_builtin_protocols_on(
                &tx,
                scope,
                &protocols,
                authorize_credential_ids,
            )?;
        }
        bump_scope_revision_on(&tx, scope, now)?;
        let scope = load_scope_on(&tx, scope)?
            .ok_or_else(|| anyhow::anyhow!("contract scope was not persisted"))?;
        self.refresh_destination_shadow()?;
        let affected = rows
            .iter()
            .map(|(model, _, _)| model.clone())
            .collect::<Vec<_>>();
        destination_store::apply_scope_controls(self, &scope.scope, Some(&affected))?;
        tx.commit()?;
        Ok(scope)
    }

    /// Clear mutable protocol judgments and apply an official-docs baseline
    /// for OpenCode Go or Command Code. Documented protocols stay Auto with
    /// Static evidence; missing models default to Chat.
    pub fn reset_provider_docs_model_protocols(
        &self,
        scope: &ContractScope,
        current_models: &[String],
        baseline: &crate::official_protocols::OfficialProtocolBaseline,
        now: DateTime<Utc>,
    ) -> Result<PersistedScopeRow> {
        anyhow::ensure!(
            scope.kind_str() == crate::provider_contracts::SCOPE_KIND_PROVIDER
                && crate::official_protocols::uses_official_docs_protocol_baseline(scope.id()),
            "docs protocol reset is only valid for OpenCode Go, Zen Free, or Command Code"
        );
        let tx = self.conn.unchecked_transaction()?;
        ensure_contract_scope_row(&tx, scope, now)?;
        anyhow::ensure!(
            !matches!(
                baseline,
                crate::official_protocols::OfficialProtocolBaseline::Unavailable
            ),
            "cannot reset protocol configuration without an official document"
        );
        clear_provider_protocol_judgments_on(&tx, scope)?;
        apply_official_protocol_baseline_on(&tx, scope, current_models, baseline, now, true)?;
        bump_scope_revision_on(&tx, scope, now)?;
        let row = load_scope_on(&tx, scope)?
            .ok_or_else(|| anyhow::anyhow!("contract scope was not persisted"))?;
        self.refresh_destination_shadow()?;
        destination_store::apply_scope_controls(self, scope, Some(current_models))?;
        tx.commit()?;
        Ok(row)
    }

    /// Persist official-docs protocols without clearing user overrides.
    pub fn apply_official_protocol_baseline(
        &self,
        scope: &ContractScope,
        current_models: &[String],
        baseline: &crate::official_protocols::OfficialProtocolBaseline,
        now: DateTime<Utc>,
    ) -> Result<PersistedScopeRow> {
        anyhow::ensure!(
            scope.kind_str() == crate::provider_contracts::SCOPE_KIND_PROVIDER
                && crate::official_protocols::uses_official_docs_protocol_baseline(scope.id()),
            "official protocol apply is only valid for OpenCode Go, Zen Free, or Command Code"
        );
        let tx = self.conn.unchecked_transaction()?;
        ensure_contract_scope_row(&tx, scope, now)?;
        apply_official_protocol_baseline_on(&tx, scope, current_models, baseline, now, false)?;
        bump_scope_revision_on(&tx, scope, now)?;
        let row = load_scope_on(&tx, scope)?
            .ok_or_else(|| anyhow::anyhow!("contract scope was not persisted"))?;
        self.refresh_destination_shadow()?;
        destination_store::apply_scope_controls(self, scope, Some(current_models))?;
        tx.commit()?;
        Ok(row)
    }

    /// Clear mutable protocol judgments for a built-in snapshot provider while
    /// preserving its current catalog. Static-supported pairs stay Auto against the
    /// official protocol baseline. Protocols inside the adapter ceiling but
    /// absent from that baseline receive ForceOff; pairs outside the ceiling
    /// are omitted rather than resurrected.
    pub fn reset_provider_static_model_protocols(
        &self,
        scope: &ContractScope,
        current_models: &[String],
        now: DateTime<Utc>,
    ) -> Result<PersistedScopeRow> {
        anyhow::ensure!(
            scope.kind_str() == crate::provider_contracts::SCOPE_KIND_PROVIDER
                && crate::provider_contracts::static_protocol_snapshot_date(scope.id()).is_some(),
            "static protocol reset is only valid for a built-in snapshot provider"
        );
        let tx = self.conn.unchecked_transaction()?;
        ensure_contract_scope_row(&tx, scope, now)?;
        clear_provider_protocol_judgments_on(&tx, scope)?;
        let descriptor = crate::provider_contracts::provider_scope_descriptor(scope.id())
            .expect("validated built-in snapshot provider");
        for model_id in current_models {
            let static_protocols = crate::provider_contracts::static_verified_protocols(
                descriptor.kind,
                model_id,
                &[],
            );
            let ceiling = crate::provider_contracts::safety_ceiling_protocols(
                descriptor.protocol_probe,
                model_id,
            );
            for protocol in [
                UpstreamProtocolKind::ChatCompletions,
                UpstreamProtocolKind::Responses,
                UpstreamProtocolKind::Messages,
            ] {
                if ceiling.contains(&protocol) && !static_protocols.contains(&protocol) {
                    set_model_protocol_override_on(
                        &tx,
                        scope,
                        model_id,
                        protocol,
                        ProtocolOverrideState::ForceOff,
                        now,
                    )?;
                }
            }
        }
        bump_scope_revision_on(&tx, scope, now)?;
        let row = load_scope_on(&tx, scope)?
            .ok_or_else(|| anyhow::anyhow!("contract scope was not persisted"))?;
        self.refresh_destination_shadow()?;
        destination_store::apply_scope_controls(self, scope, Some(current_models))?;
        tx.commit()?;
        Ok(row)
    }

    /// Commit one probe batch — evidence observations plus the binary
    /// overrides each probed protocol implies — in a single transaction with
    /// a single scope-revision bump.
    pub fn commit_model_protocol_probe_results(
        &self,
        scope: &ContractScope,
        observations: &[PersistedModelProtocol],
        overrides: &[(String, UpstreamProtocolKind, ProtocolOverrideState)],
        now: DateTime<Utc>,
    ) -> Result<PersistedScopeRow> {
        anyhow::ensure!(
            !observations.is_empty() || !overrides.is_empty(),
            "probe result batch must be nonempty"
        );
        anyhow::ensure!(
            observations.iter().all(|row| row.scope == *scope),
            "probe observations must belong to the committed contract scope"
        );
        let tx = self.conn.unchecked_transaction()?;
        for row in observations {
            upsert_model_protocol_row_on(&tx, row)?;
        }
        for (model_id, protocol, state) in overrides {
            set_model_protocol_override_on(&tx, scope, model_id, *protocol, *state, now)?;
        }
        bump_scope_revision_on(&tx, scope, now)?;
        let scope = load_scope_on(&tx, scope)?
            .ok_or_else(|| anyhow::anyhow!("contract scope was not persisted"))?;
        self.refresh_destination_shadow()?;
        let affected = overrides
            .iter()
            .map(|(model, _, _)| model.clone())
            .collect::<Vec<_>>();
        if !affected.is_empty() {
            destination_store::apply_scope_controls(self, &scope.scope, Some(&affected))?;
        }
        tx.commit()?;
        Ok(scope)
    }

    pub fn load_model_protocol(
        &self,
        scope: &ContractScope,
        model_id: &str,
        protocol: UpstreamProtocolKind,
    ) -> Result<Option<PersistedModelProtocol>> {
        self.conn
            .query_row(
                "SELECT scope_kind, scope_id, model_id, protocol, source, verified_at,
                        observed_at, last_probe_result, last_probe_at, last_probe_error
                 FROM provider_contract_model_protocols
                 WHERE scope_kind = ?1 AND scope_id = ?2 AND model_id = ?3 AND protocol = ?4",
                params![scope.kind_str(), scope.id(), model_id, protocol.as_str()],
                persist_evidence_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn upsert_model_protocol(&self, row: &PersistedModelProtocol) -> Result<PersistedScopeRow> {
        self.upsert_model_protocols(std::slice::from_ref(row))
    }

    /// Persist a nonempty set of protocol observations and advance the nested
    /// contract-scope revision exactly once, in a single SQLite transaction.
    ///
    /// All rows must share one [`ContractScope`]. Mixed scopes are rejected
    /// before any write so a caller cannot commit a partial batch.
    pub fn upsert_model_protocols(
        &self,
        rows: &[PersistedModelProtocol],
    ) -> Result<PersistedScopeRow> {
        let Some((first, rest)) = rows.split_first() else {
            anyhow::bail!("protocol observation batch must be nonempty");
        };
        if let Some(other) = rest.iter().find(|row| row.scope != first.scope) {
            anyhow::bail!(
                "protocol observations mix contract scopes `{}:{}` and `{}:{}`",
                first.scope.kind_str(),
                first.scope.id(),
                other.scope.kind_str(),
                other.scope.id()
            );
        }
        let tx = self.conn.unchecked_transaction()?;
        for row in rows {
            upsert_model_protocol_row_on(&tx, row)?;
        }
        let now = rows
            .iter()
            .rev()
            .find_map(|row| row.observed_at)
            .unwrap_or_else(Utc::now);
        bump_scope_revision_on(&tx, &first.scope, now)?;
        let scope = load_scope_on(&tx, &first.scope)?
            .ok_or_else(|| anyhow::anyhow!("contract scope was not persisted"))?;
        self.refresh_destination_shadow()?;
        tx.commit()?;
        Ok(scope)
    }
}
