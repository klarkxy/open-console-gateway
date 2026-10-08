//! One-shot v66 compatibility facts. Runtime alias generation never reads this module.
use super::*;
use ocg_domain::destination::destination_id_for_builtin;

// Historical defaults only. A later discovery of any of these raw IDs follows
// the current naming policy because schema 67 never replays this migration.
const LEGACY_DEFAULT_NAMES: &[(&str, &str, &str)] = &[
    (KIMI_PROVIDER_ID, "k3", "kimi-k3"),
    (KIMI_PROVIDER_ID, "k3-256k", "kimi-k3-256k"),
    (
        COMMAND_CODE_PROVIDER_ID,
        "nvidia/nemotron-3-ultra-550b-a55b",
        "nemotron-3-ultra",
    ),
];

pub(super) fn migrate_to_v67(conn: &Connection, db_path: &Path, is_fresh: bool) -> Result<()> {
    let version = schema_version_on(conn)?;
    if version >= 67 {
        return Ok(());
    }
    anyhow::ensure!(version == 66, "v67 requires schema v66");
    if !is_fresh {
        create_pre_version_backup(conn, db_path, PRE_V67_BACKUP_FILE_PREFIX, 66)?;
    }
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let locked = schema_version_on(&tx)?;
    if locked >= 67 {
        tx.commit()?;
        return Ok(());
    }
    anyhow::ensure!(
        locked == 66,
        "v67 writer lock observed schema {locked}, expected 66"
    );
    preserve_saved_goat_protocols(&tx)?;
    preserve_legacy_default_names(&tx)?;
    tx.execute_batch("INSERT OR REPLACE INTO schema_version(version) VALUES (67);")?;
    tx.commit()?;
    Ok(())
}

fn preserve_saved_goat_protocols(conn: &Connection) -> Result<()> {
    let destination = destination_id_for_builtin(COMMAND_CODE_PROVIDER_ID);
    for model in destination_store::load_destination_catalog(conn, &destination)? {
        // These are actual saved protocols, not protocols inferred from raw ID
        // membership. Empty/unsupported rows gain no evidence or enablement.
        let constructable =
            ocg_domain::protocol::command_code_constructable_formats(&model.upstream_model);
        for protocol in model.protocols {
            if !constructable.contains(&crate::provider_contracts::protocol_to_api(protocol)) {
                continue;
            }
            conn.execute(
                "INSERT INTO provider_contract_model_protocols
                 (scope_kind, scope_id, model_id, protocol, source)
                 SELECT 'provider', ?1, ?2, ?3, 'preset'
                 WHERE NOT EXISTS (SELECT 1 FROM provider_contract_model_protocols
                     WHERE scope_kind='provider' AND scope_id=?1
                       AND model_id=?2 COLLATE NOCASE AND protocol=?3)",
                params![
                    COMMAND_CODE_PROVIDER_ID,
                    model.upstream_model,
                    protocol.as_str()
                ],
            )?;
            // Keep diagnostic fields while preserving declaration support that
            // the removed runtime whitelist previously supplied.
            conn.execute(
                "UPDATE provider_contract_model_protocols SET source='preset'
                 WHERE scope_kind='provider' AND scope_id=?1
                   AND model_id=?2 COLLATE NOCASE AND protocol=?3 AND source='probe_observed'",
                params![
                    COMMAND_CODE_PROVIDER_ID,
                    model.upstream_model,
                    protocol.as_str()
                ],
            )?;
            if !model.enabled {
                conn.execute(
                    "INSERT OR IGNORE INTO provider_contract_model_protocol_overrides
                     (scope_kind, scope_id, model_id, protocol, state, updated_at)
                     VALUES ('provider', ?1, ?2, ?3, 'force_off', ?4)",
                    params![
                        COMMAND_CODE_PROVIDER_ID,
                        model.upstream_model,
                        protocol.as_str(),
                        Utc::now().to_rfc3339()
                    ],
                )?;
            }
        }
    }
    Ok(())
}

fn shared_canonical_identity(raw: &str, canonical: &str) -> bool {
    raw.rsplit('/')
        .next()
        .is_some_and(|leaf| leaf.eq_ignore_ascii_case(canonical))
}

fn preserve_legacy_default_names(conn: &Connection) -> Result<()> {
    for &(provider, upstream, public) in LEGACY_DEFAULT_NAMES {
        let destination = destination_id_for_builtin(provider);
        let catalog = destination_store::load_destination_catalog(conn, &destination)?;
        let Some(model) = catalog.iter().find(|model| {
            model.upstream_model == upstream && model.public_model == model.upstream_model
        }) else {
            continue;
        };
        let mut stmt = conn.prepare(
            "SELECT m.destination_id, d.legacy_kind, d.legacy_id, m.upstream_model
             FROM destination_models m JOIN destinations d ON d.id=m.destination_id
             WHERE m.public_model_key=?1 OR m.upstream_model=?1 COLLATE NOCASE",
        )?;
        let conflicts = stmt
            .query_map([public.to_ascii_lowercase()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if conflicts
            .iter()
            .any(|(other_destination, kind, provider, raw)| {
                // Same-destination collisions are always ambiguous. Cross-provider
                // already-shared canonical identities remain intentionally shared.
                other_destination == &destination
                    || kind != "builtin"
                    || !matches!(
                        provider.as_str(),
                        OPENCODE_PROVIDER_ID | COMMAND_CODE_PROVIDER_ID | KIMI_PROVIDER_ID
                    )
                    || !shared_canonical_identity(raw, public)
            })
        {
            continue;
        }
        let old = model.public_model.clone();
        conn.execute(
            "UPDATE destination_models SET public_model=?3, public_model_key=?4
             WHERE destination_id=?1 AND public_model_key=?2",
            params![
                destination,
                old.to_ascii_lowercase(),
                public,
                public.to_ascii_lowercase()
            ],
        )?;
        // Scopes referring to the exact raw ID still resolve through raw lookup.
        // Keep them unchanged: changing them to a shared alias could widen scope.
    }
    Ok(())
}

#[cfg(test)]
mod tests;
