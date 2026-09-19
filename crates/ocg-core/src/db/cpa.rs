//! CPA singleton on destinations + observer credential (schema v55+).
//!
//! After the leftover `cpa_integration` drop, the integration reconstructs
//! from the CPA destination (`adapter=cpa` / legacy builtin `cpa`) and the
//! observer credential (`destinations.observer_credential_id`). The
//! management cipher stays on that observer row; the reserved inference
//! credential stays keyless of the management secret. V4 GET stays
//! secret-free. A fresh database without leftover CPA does not invent a
//! destination.

use super::*;
use ocg_domain::credential::{identity_id_for_cpa, observer_credential_id_for_cpa};
use ocg_domain::destination::{
    AdapterKind, Destination, LegacyDestinationFacts, destination_from_legacy,
    destination_id_for_builtin,
};

pub(crate) const CREDENTIAL_PURPOSE_CPA_OBSERVER: &str = "cpa_observer";

#[derive(Debug, Clone)]
pub(crate) struct CpaDestinationExtras {
    pub id: String,
    pub base_url: Option<String>,
    pub observer_credential_id: Option<String>,
}

/// v55: destinations + credentials become the CPA integration store.
/// Backfill leftover `cpa_integration` when that row can map, refuse if it
/// cannot, then `DROP TABLE cpa_integration`. Does not invent a CPA
/// destination when leftover is absent.
pub(crate) fn migrate_v55_backfill_and_drop(conn: &Connection) -> Result<()> {
    if table_exists(conn, "cpa_integration")? {
        backfill_cpa_from_leftover(conn)?;
    }
    conn.execute_batch("DROP TABLE IF EXISTS cpa_integration;")?;
    Ok(())
}

pub(crate) fn destination_id() -> String {
    destination_id_for_builtin(CPA_PROVIDER_ID)
}

pub(crate) fn observer_id() -> String {
    observer_credential_id_for_cpa().to_string()
}

pub(crate) fn destination_present(conn: &Connection) -> Result<bool> {
    if !table_exists(conn, "destinations")? {
        return Ok(false);
    }
    Ok(conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM destinations
             WHERE id = ?1
                OR adapter = 'cpa'
                OR (legacy_kind = 'builtin' AND legacy_id = ?2)
         )",
        params![destination_id(), CPA_PROVIDER_ID],
        |row| row.get::<_, i64>(0),
    )? != 0)
}

pub(crate) fn overlay_projected_destination(
    conn: &Connection,
    destinations: &mut [Destination],
) -> Result<()> {
    let Some(destination) = destinations.iter_mut().find(|destination| {
        destination.adapter == AdapterKind::Cpa || destination.id == destination_id()
    }) else {
        return Ok(());
    };
    destination.observer_credential_id = Some(observer_id());
    destination.capabilities.observer = true;
    let Some(stored) = load_destination_row(conn)? else {
        return Ok(());
    };
    if let Some(base_url) = stored
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        destination.base_url = Some(base_url.to_string());
    }
    if let Some(observer) = stored
        .observer_credential_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        destination.observer_credential_id = Some(observer.to_string());
    }
    Ok(())
}

pub(crate) fn snapshot_destination_extras(conn: &Connection) -> Result<Vec<CpaDestinationExtras>> {
    if !table_exists(conn, "destinations")? {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT id, base_url, observer_credential_id
         FROM destinations
         WHERE id = ?1
            OR adapter = 'cpa'
            OR (legacy_kind = 'builtin' AND legacy_id = ?2)",
    )?;
    let rows = stmt
        .query_map(params![destination_id(), CPA_PROVIDER_ID], |row| {
            Ok(CpaDestinationExtras {
                id: row.get(0)?,
                base_url: row.get(1)?,
                observer_credential_id: row.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub(crate) fn restore_destination_extras(
    conn: &Connection,
    extras: &[CpaDestinationExtras],
) -> Result<()> {
    if extras.is_empty() || !table_exists(conn, "destinations")? {
        return Ok(());
    }
    for extra in extras {
        conn.execute(
            "UPDATE destinations
             SET base_url = COALESCE(?2, base_url),
                 observer_credential_id = COALESCE(?3, observer_credential_id)
             WHERE id = ?1",
            params![extra.id, extra.base_url, extra.observer_credential_id],
        )?;
    }
    Ok(())
}

pub(crate) fn integration_on(conn: &Connection) -> Result<Option<CpaIntegrationRecord>> {
    let Some(stored) = load_destination_row(conn)? else {
        return Ok(None);
    };
    let observer = stored
        .observer_credential_id
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(observer_id);
    let management_key_cipher = observer_key_cipher_on(conn, &observer)?.unwrap_or_default();
    Ok(Some(CpaIntegrationRecord {
        account_id: CPA_ACCOUNT_ID.to_string(),
        base_url: stored.base_url.unwrap_or_default(),
        management_key_cipher,
    }))
}

pub(crate) fn upsert_destination_and_observer_on(
    conn: &Connection,
    base_url: &str,
    management_key_cipher: &str,
) -> Result<()> {
    let mut destination = destination_from_legacy(&LegacyDestinationFacts::Cpa)
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    destination.observer_credential_id = Some(observer_id());
    destination.capabilities.observer = true;
    let trimmed = base_url.trim();
    if !trimmed.is_empty() {
        destination.base_url = Some(trimmed.to_string());
    } else {
        destination.base_url = None;
    }
    let dest_id = destination.id.clone();
    let observer = observer_id();
    if destination_exists(conn, &dest_id)? {
        conn.execute(
            "UPDATE destinations
             SET name = ?2, base_url = ?3, brand_family = ?4,
                 protocols_json = ?5, auth_scheme = ?6, adapter = ?7,
                 capabilities_json = ?8, max_credentials = ?9, enabled = ?10,
                 observer_credential_id = ?11
             WHERE id = ?1",
            params![
                dest_id,
                destination.name,
                destination.base_url,
                destination.brand_family,
                serde_json::to_string(&destination.protocols)?,
                destination.auth_scheme.as_str(),
                destination.adapter.as_str(),
                serde_json::to_string(&destination.capabilities)?,
                destination.max_credentials.map(i64::from),
                i64::from(destination.enabled),
                observer,
            ],
        )?;
    } else {
        conn.execute(
            "INSERT INTO destinations (
                id, legacy_kind, legacy_id, adapter, name, brand_family, base_url,
                protocols_json, auth_scheme, capabilities_json, plan_json,
                max_credentials, observer_credential_id, enabled
             ) VALUES (?1, 'builtin', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                dest_id,
                CPA_PROVIDER_ID,
                destination.adapter.as_str(),
                destination.name,
                destination.brand_family,
                destination.base_url,
                serde_json::to_string(&destination.protocols)?,
                destination.auth_scheme.as_str(),
                serde_json::to_string(&destination.capabilities)?,
                destination
                    .plan
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?,
                destination.max_credentials.map(i64::from),
                observer,
                i64::from(destination.enabled),
            ],
        )?;
    }
    upsert_observer_on(conn, &dest_id, &destination.name, management_key_cipher)?;
    Ok(())
}

pub(crate) fn delete_destination_and_observer_on(conn: &Connection) -> Result<()> {
    let dest_id = destination_id();
    let observer = observer_id();
    conn.execute(
        "DELETE FROM destination_models WHERE destination_id = ?1",
        [&dest_id],
    )?;
    conn.execute("DELETE FROM destinations WHERE id = ?1", [&dest_id])?;
    conn.execute(
        "DELETE FROM credential_grants WHERE credential_id = ?1",
        [&observer],
    )?;
    conn.execute("DELETE FROM credentials WHERE id = ?1", [&observer])?;
    Ok(())
}

fn backfill_cpa_from_leftover(conn: &Connection) -> Result<()> {
    if !table_has_column(conn, "cpa_integration", "account_id")?
        || !table_has_column(conn, "cpa_integration", "base_url")?
        || !table_has_column(conn, "cpa_integration", "management_key_cipher")?
    {
        let count: i64 =
            conn.query_row("SELECT COUNT(*) FROM cpa_integration", [], |row| row.get(0))?;
        anyhow::ensure!(
            count == 0,
            "v55 refuses leftover cpa_integration that cannot map"
        );
        return Ok(());
    }
    let leftover = conn
        .query_row(
            "SELECT account_id, base_url, management_key_cipher
               FROM cpa_integration WHERE id = 'cpa'",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((account_id, base_url, management_key_cipher)) = leftover else {
        return Ok(());
    };
    anyhow::ensure!(
        !account_id.trim().is_empty(),
        "v55 refuses leftover cpa_integration: empty account_id"
    );
    anyhow::ensure!(
        !management_key_cipher.trim().is_empty(),
        "v55 refuses leftover cpa_integration: empty management_key_cipher"
    );
    upsert_destination_and_observer_on(conn, &base_url, management_key_cipher.trim())?;
    Ok(())
}

fn load_destination_row(conn: &Connection) -> Result<Option<CpaDestinationExtras>> {
    if !table_exists(conn, "destinations")? {
        return Ok(None);
    }
    conn.query_row(
        "SELECT id, base_url, observer_credential_id
           FROM destinations
          WHERE id = ?1
             OR adapter = 'cpa'
             OR (legacy_kind = 'builtin' AND legacy_id = ?2)
          LIMIT 1",
        params![destination_id(), CPA_PROVIDER_ID],
        |row| {
            Ok(CpaDestinationExtras {
                id: row.get(0)?,
                base_url: row.get(1)?,
                observer_credential_id: row.get(2)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

fn destination_exists(conn: &Connection, destination_id: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM destinations WHERE id = ?1)",
        [destination_id],
        |row| row.get::<_, i64>(0),
    )? != 0)
}

fn observer_key_cipher_on(conn: &Connection, observer_id: &str) -> Result<Option<String>> {
    if !table_exists(conn, "credentials")? {
        return Ok(None);
    }
    let value: Option<String> = conn
        .query_row(
            "SELECT key_cipher FROM credentials WHERE id = ?1",
            [observer_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(value.filter(|cipher| !cipher.is_empty()))
}

fn upsert_observer_on(
    conn: &Connection,
    dest_id: &str,
    name: &str,
    management_key_cipher: &str,
) -> Result<()> {
    let observer = observer_id();
    let identity_id = identity_id_for_cpa().to_string();
    let scope_json = serde_json::to_string(&ocg_domain::credential::ModelScope::All)?;
    let existing: Option<String> = conn
        .query_row(
            "SELECT key_cipher FROM credentials WHERE id = ?1",
            [&observer],
            |row| row.get(0),
        )
        .optional()?;
    let cipher = if management_key_cipher.is_empty() {
        existing.clone().unwrap_or_default()
    } else {
        management_key_cipher.to_string()
    };
    let has_secret = !cipher.is_empty();
    if existing.is_some() {
        conn.execute(
            "UPDATE credentials
             SET destination_id = ?2, name = ?3, has_secret = ?4,
                 credential_purpose = ?5, identity_id = COALESCE(identity_id, ?6),
                 key_cipher = ?7
             WHERE id = ?1",
            params![
                observer,
                dest_id,
                name,
                i64::from(has_secret),
                CREDENTIAL_PURPOSE_CPA_OBSERVER,
                identity_id,
                cipher,
            ],
        )?;
        return Ok(());
    }
    conn.execute(
        "INSERT INTO credentials (
            id, legacy_account_id, destination_id, name, notes, has_secret,
            enabled, routing_rank, scope_json, auth_state, last_error,
            cooldown_generic_until, cooldown_5h_until, cooldown_week_until,
            cooldown_month_until, cooldown_free_until, quota_pool_id,
            onboarding_json, purchase_date, key_cipher, credential_purpose,
            identity_id, provider_id
         ) VALUES (
            ?1, ?2, ?3, ?4, NULL, ?5, 0, -1, ?6, 'unknown', NULL,
            NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, ?7, ?8, ?9, ?10
         )",
        params![
            observer,
            observer,
            dest_id,
            name,
            i64::from(has_secret),
            scope_json,
            cipher,
            CREDENTIAL_PURPOSE_CPA_OBSERVER,
            identity_id,
            CPA_PROVIDER_ID,
        ],
    )?;
    Ok(())
}
