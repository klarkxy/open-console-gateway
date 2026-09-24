//! Direct inference credential reader. No Account reconstruction or fallback.
use crate::destination_projection::DestinationProjection;
use crate::routing_snapshot::ExecutionCredential;
use anyhow::{Context, Result, ensure};

pub(crate) fn load(
    db: &super::Database,
    projection: &DestinationProjection,
) -> Result<Vec<ExecutionCredential>> {
    let mut statement = db.conn.prepare(
        "SELECT id, legacy_account_id, destination_id, provider_id, name, key_cipher,
                enabled, setup_step, auth_error, binding_id, binding_enabled,
                credential_version, authorization_connection_id, quota_recovery_json
         FROM credentials WHERE COALESCE(credential_purpose, 'inference') = 'inference'
         ORDER BY routing_rank, created_at, legacy_account_id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, bool>(6)?,
            row.get::<_, String>(7)?,
            row.get::<_, Option<String>>(8)?,
            row.get::<_, Option<String>>(9)?,
            row.get::<_, Option<bool>>(10)?,
            row.get::<_, Option<i64>>(11)?,
            row.get::<_, String>(12)?,
            row.get::<_, Option<String>>(13)?,
        ))
    })?;
    let mut credentials = Vec::new();
    for row in rows {
        let (
            credential_id,
            id,
            destination_id,
            provider_id,
            name,
            key_cipher,
            enabled,
            step,
            auth_error,
            binding_id,
            binding_enabled,
            version,
            authorization_connection_id,
            quota_recovery_json,
        ) = row?;
        let dto = projection
            .credentials
            .iter()
            .find(|c| c.id == credential_id)
            .context("inference credential missing from persisted projection")?;
        ensure!(
            dto.destination_id == destination_id,
            "credential destination mismatch"
        );
        let version = version.context("missing credential version")?;
        ensure!(version > 0, "invalid credential version");
        let destination = projection
            .destinations
            .iter()
            .find(|d| d.id == destination_id)
            .context("missing execution destination")?;
        let (preset, offering, draft): (Option<String>, Option<String>, bool) = db.conn.query_row(
            "SELECT preset_id, offering, COALESCE(onboarding_draft, 0) FROM destinations WHERE id = ?1", [&destination_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        let official_pricing_kind = if destination.adapter
            == ocg_domain::destination::AdapterKind::Http
            && destination.auth_scheme == ocg_domain::destination::AuthScheme::Bearer
            && offering.as_deref() == Some("api")
        {
            let kind = match preset.as_deref() {
                Some("deepseek") => Some(crate::official_api::OfficialApiKind::Deepseek),
                Some("zhipu") => Some(crate::official_api::OfficialApiKind::Zhipu),
                _ => None,
            };
            kind.filter(|kind| {
                destination
                    .base_url
                    .as_deref()
                    .zip(destination.protocols.first())
                    .is_some_and(|(url, protocol)| {
                        crate::official_api::route_is_official(*kind, url, *protocol)
                    })
            })
        } else {
            None
        };
        let setup: crate::models::AccountSetupStep =
            serde_json::from_value(serde_json::json!(step))
                .context("invalid persisted credential readiness")?;
        credentials.push(ExecutionCredential {
            id,
            credential_id,
            destination_id,
            provider_id,
            official_pricing_kind,
            name,
            key_cipher,
            enabled,
            ready: setup.is_ready() && !draft,
            auth_error,
            cooldowns: dto.cooldowns.clone(),
            binding_id: binding_id.context("missing credential binding")?,
            binding_enabled: binding_enabled.context("missing binding enablement")?,
            credential_version: version as u64,
            authorization_connection_id,
            scope: dto.scope.clone(),
            grants: dto.grants.clone(),
            quota_recovery: quota_recovery_json
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .map(serde_json::from_str)
                .transpose()
                .context("invalid credentials.quota_recovery_json")?,
            quota_probe: false,
        });
    }
    Ok(credentials)
}

/// Explicit Ollama alias pins are the only legacy protocol controls consumed
/// by inference. Evidence and mutable recomputed contracts are not read here.
pub(crate) fn load_ollama_pins(db: &super::Database) -> Result<Vec<String>> {
    let mut statement = db.conn.prepare("SELECT model_id, state FROM provider_contract_model_protocol_overrides WHERE scope_kind = 'provider' AND scope_id = ?1 AND protocol = 'chat_completions' ORDER BY model_id")?;
    let rows = statement.query_map([crate::provider::OLLAMA_PROVIDER_ID], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut pins = Vec::new();
    for row in rows {
        let (model, state) = row?;
        let state = crate::provider_contracts::ProtocolOverrideState::try_from(state.as_str())
            .map_err(anyhow::Error::msg)?;
        if state == crate::provider_contracts::ProtocolOverrideState::ForceOn {
            pins.push(model);
        }
    }
    Ok(pins)
}
