//! Platform parents and links on destinations + credentials (schema v54+).
//!
//! After the leftover `platform_accounts` / `platform_links` drop, parents
//! reconstruct from `destinations` (`legacy_kind=platform_parent`) and the
//! observer credential (`destinations.observer_credential_id`). Links
//! reconstruct from inference credentials whose `destination_id` is that
//! parent. Management ciphertext stays on the observer row; V4 GET stays
//! secret-free.
use super::*;
use crate::platform::{
    PlatformAccount, PlatformGroup, PlatformKind, PlatformLink, PlatformSnapshot,
};
use ocg_domain::credential::{
    identity_id_for_platform_account, observer_credential_id_for_platform_account,
};
use ocg_domain::destination::{
    LegacyDestinationFacts, PlatformKind as DomainPlatformKind, destination_from_legacy,
    destination_id_for_custom_account, destination_id_for_platform_account,
};
use rusqlite::OptionalExtension;

pub(crate) const CREDENTIAL_PURPOSE_INFERENCE: &str = "inference";
pub(crate) const CREDENTIAL_PURPOSE_PLATFORM_OBSERVER: &str = "platform_observer";

// A new association must not reuse a deleted row's refresh identity (ABA).
fn fresh_version() -> u64 {
    (uuid::Uuid::new_v4().as_u128() as u64) & 0x0000_FFFF_FFFF_FFFF
}

pub(super) fn migrate_to_v38(conn: &Connection) -> Result<()> {
    let version = schema_version_on(conn)?;
    if version >= 38 {
        return Ok(());
    }
    anyhow::ensure!(version == 37, "v38 requires schema v37");
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS platform_accounts (
        id TEXT PRIMARY KEY, kind TEXT NOT NULL CHECK(kind IN ('new_api','sub2api')),
        name TEXT NOT NULL, base_url TEXT NOT NULL, credential_cipher TEXT,
        version INTEGER NOT NULL DEFAULT 1, snapshot TEXT);
        CREATE TABLE IF NOT EXISTS platform_links (
        account_id TEXT PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
        platform_account_id TEXT NOT NULL REFERENCES platform_accounts(id) ON DELETE RESTRICT,
        group_json TEXT NOT NULL, version INTEGER NOT NULL DEFAULT 1, snapshot TEXT);
        INSERT OR REPLACE INTO schema_version(version) VALUES(38);",
    )?;
    tx.commit()?;
    Ok(())
}

/// v54: destinations + credentials become the platform parent/link store.
/// Backfill leftover parents/links when they can map, refuse if a leftover
/// row cannot map, then `DROP TABLE platform_links` and
/// `DROP TABLE platform_accounts`.
pub(crate) fn migrate_v54_backfill_and_drop(conn: &Connection) -> Result<()> {
    ensure_v54_columns(conn)?;
    if table_exists(conn, "platform_accounts")? {
        backfill_platform_parents_from_leftover(conn)?;
    }
    if table_exists(conn, "platform_links")? {
        backfill_platform_links_from_leftover(conn)?;
    }
    conn.execute_batch(
        "DROP TABLE IF EXISTS platform_links;
         DROP TABLE IF EXISTS platform_accounts;",
    )?;
    Ok(())
}

pub(crate) fn ensure_v54_columns(conn: &Connection) -> Result<()> {
    if table_exists(conn, "destinations")? {
        if !table_has_column(conn, "destinations", "platform_kind")? {
            conn.execute_batch("ALTER TABLE destinations ADD COLUMN platform_kind TEXT")?;
        }
        if !table_has_column(conn, "destinations", "platform_version")? {
            conn.execute_batch("ALTER TABLE destinations ADD COLUMN platform_version INTEGER")?;
        }
        if !table_has_column(conn, "destinations", "platform_snapshot")? {
            conn.execute_batch("ALTER TABLE destinations ADD COLUMN platform_snapshot TEXT")?;
        }
    }
    if table_exists(conn, "credentials")? {
        if !table_has_column(conn, "credentials", "group_json")? {
            conn.execute_batch("ALTER TABLE credentials ADD COLUMN group_json TEXT")?;
        }
        if !table_has_column(conn, "credentials", "link_version")? {
            conn.execute_batch("ALTER TABLE credentials ADD COLUMN link_version INTEGER")?;
        }
        if !table_has_column(conn, "credentials", "link_snapshot")? {
            conn.execute_batch("ALTER TABLE credentials ADD COLUMN link_snapshot TEXT")?;
        }
        if !table_has_column(conn, "credentials", "credential_purpose")? {
            conn.execute_batch(
                "ALTER TABLE credentials ADD COLUMN credential_purpose TEXT NOT NULL DEFAULT 'inference'",
            )?;
        }
    }
    Ok(())
}

fn backfill_platform_parents_from_leftover(conn: &Connection) -> Result<()> {
    if !table_has_column(conn, "platform_accounts", "base_url")?
        || !table_has_column(conn, "platform_accounts", "kind")?
    {
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM platform_accounts", [], |row| {
            row.get(0)
        })?;
        anyhow::ensure!(
            count == 0,
            "v54 refuses leftover platform parents that cannot map"
        );
        return Ok(());
    }
    let mut stmt = conn.prepare(
        "SELECT id, kind, name, base_url, credential_cipher, version, snapshot
         FROM platform_accounts
         ORDER BY rowid",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, Option<String>>(6)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    for (id, kind, name, base_url, credential_cipher, version, snapshot) in rows {
        let parsed = parse_platform_kind(&kind).ok_or_else(|| {
            anyhow::anyhow!("v54 refuses leftover platform parent `{id}`: unknown kind `{kind}`")
        })?;
        let base = base_url.trim();
        anyhow::ensure!(
            !base.is_empty(),
            "v54 refuses leftover platform parent `{id}`: empty base_url"
        );
        upsert_platform_parent_on(
            conn,
            &id,
            parsed,
            &name,
            base,
            credential_cipher.as_deref(),
            Some(version as u64),
            snapshot.as_deref(),
            false,
        )?;
    }
    Ok(())
}

fn backfill_platform_links_from_leftover(conn: &Connection) -> Result<()> {
    if !table_has_column(conn, "platform_links", "account_id")?
        || !table_has_column(conn, "platform_links", "platform_account_id")?
        || !table_has_column(conn, "platform_links", "group_json")?
    {
        let count: i64 =
            conn.query_row("SELECT COUNT(*) FROM platform_links", [], |row| row.get(0))?;
        anyhow::ensure!(
            count == 0,
            "v54 refuses leftover platform links that cannot map"
        );
        return Ok(());
    }
    let mut stmt = conn.prepare(
        "SELECT account_id, platform_account_id, group_json, version, snapshot
         FROM platform_links
         ORDER BY rowid",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    for (account_id, parent_id, group_json, version, snapshot) in rows {
        let dest_id = destination_id_for_platform_account(&parent_id);
        anyhow::ensure!(
            destination_exists(conn, &dest_id)?,
            "v54 refuses leftover platform link `{account_id}`: parent `{parent_id}` has no destination"
        );
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM credentials WHERE legacy_account_id = ?1)",
            [&account_id],
            |row| row.get::<_, i64>(0),
        )? != 0;
        anyhow::ensure!(
            exists,
            "v54 refuses leftover platform link `{account_id}`: inference credential is missing"
        );
        serde_json::from_str::<PlatformGroup>(&group_json).map_err(|error| {
            anyhow::anyhow!(
                "v54 refuses leftover platform link `{account_id}`: invalid group_json ({error})"
            )
        })?;
        conn.execute(
            "UPDATE credentials
             SET destination_id = ?2, group_json = ?3, link_version = ?4, link_snapshot = ?5
             WHERE legacy_account_id = ?1",
            params![account_id, dest_id, group_json, version, snapshot,],
        )?;
    }
    Ok(())
}

fn parse_platform_kind(value: &str) -> Option<PlatformKind> {
    match value {
        "new_api" => Some(PlatformKind::NewApi),
        "sub2api" => Some(PlatformKind::Sub2api),
        _ => None,
    }
}

fn kind_sql(kind: PlatformKind) -> &'static str {
    if kind == PlatformKind::NewApi {
        "new_api"
    } else {
        "sub2api"
    }
}

fn kind_from_destination(
    platform_kind: Option<&str>,
    brand_family: Option<&str>,
) -> Option<PlatformKind> {
    if let Some(kind) = platform_kind.and_then(parse_platform_kind) {
        return Some(kind);
    }
    match brand_family {
        Some("New API") => Some(PlatformKind::NewApi),
        Some("Sub2API") => Some(PlatformKind::Sub2api),
        _ => None,
    }
}

fn domain_kind(kind: PlatformKind) -> DomainPlatformKind {
    match kind {
        PlatformKind::NewApi => DomainPlatformKind::NewApi,
        PlatformKind::Sub2api => DomainPlatformKind::Sub2Api,
    }
}

fn destination_exists(conn: &Connection, destination_id: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM destinations WHERE id = ?1)",
        [destination_id],
        |row| row.get::<_, i64>(0),
    )? != 0)
}

fn parse_platform_snapshot(value: Option<String>) -> Result<Option<PlatformSnapshot>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed == "null" {
        return Ok(None);
    }
    Ok(Some(serde_json::from_str(trimmed)?))
}

fn parent_from_destination_row(
    legacy_id: String,
    name: String,
    base_url: Option<String>,
    platform_kind: Option<String>,
    brand_family: Option<String>,
    version: Option<i64>,
    snapshot: Option<String>,
    has_user_credential: bool,
) -> Result<PlatformAccount> {
    let kind = kind_from_destination(platform_kind.as_deref(), brand_family.as_deref())
        .ok_or_else(|| anyhow::anyhow!("platform parent `{legacy_id}` has unknown kind"))?;
    let base_url = base_url.unwrap_or_default();
    anyhow::ensure!(
        !base_url.trim().is_empty(),
        "platform parent `{legacy_id}` has empty base_url"
    );
    Ok(PlatformAccount {
        id: legacy_id,
        kind,
        name,
        base_url,
        has_user_credential,
        version: version.unwrap_or(1) as u64,
        snapshot: parse_platform_snapshot(snapshot)?,
    })
}

fn leftover_parent_from_row(
    id: String,
    kind: String,
    name: String,
    base_url: String,
    has_cipher: bool,
    version: i64,
    snapshot: Option<String>,
) -> Result<PlatformAccount> {
    let kind = parse_platform_kind(&kind)
        .ok_or_else(|| anyhow::anyhow!("platform parent `{id}` has unknown kind `{kind}`"))?;
    Ok(PlatformAccount {
        id,
        kind,
        name,
        base_url,
        has_user_credential: has_cipher,
        version: version as u64,
        snapshot: parse_platform_snapshot(snapshot)?,
    })
}

fn leftover_platform_accounts_on(conn: &Connection) -> Result<Vec<PlatformAccount>> {
    if !table_exists(conn, "platform_accounts")? {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT id, kind, name, base_url, credential_cipher IS NOT NULL, version, snapshot
         FROM platform_accounts ORDER BY rowid",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, bool>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, Option<String>>(6)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    rows.into_iter()
        .map(
            |(id, kind, name, base_url, has_cipher, version, snapshot)| {
                leftover_parent_from_row(id, kind, name, base_url, has_cipher, version, snapshot)
            },
        )
        .collect()
}

fn leftover_platform_account_on(conn: &Connection, id: &str) -> Result<Option<PlatformAccount>> {
    if !table_exists(conn, "platform_accounts")? {
        return Ok(None);
    }
    let row: Option<(String, String, String, String, bool, i64, Option<String>)> = conn
        .query_row(
            "SELECT id, kind, name, base_url, credential_cipher IS NOT NULL, version, snapshot
             FROM platform_accounts WHERE id = ?1",
            [id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .optional()?;
    row.map(
        |(id, kind, name, base_url, has_cipher, version, snapshot)| {
            leftover_parent_from_row(id, kind, name, base_url, has_cipher, version, snapshot)
        },
    )
    .transpose()
}

fn leftover_platform_links_on(conn: &Connection) -> Result<Vec<PlatformLink>> {
    if !table_exists(conn, "platform_links")? {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT account_id, platform_account_id, group_json, snapshot
         FROM platform_links ORDER BY rowid",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    rows.into_iter()
        .map(|(account_id, platform_account_id, group, snapshot)| {
            Ok(PlatformLink {
                account_id,
                platform_account_id,
                group: serde_json::from_str(&group)?,
                snapshot: parse_platform_snapshot(snapshot)?,
            })
        })
        .collect()
}

fn list_platform_accounts_on(conn: &Connection) -> Result<Vec<PlatformAccount>> {
    if table_exists(conn, "destinations")?
        && table_has_column(conn, "destinations", "platform_kind")?
    {
        let mut stmt = conn.prepare(
            "SELECT d.legacy_id, d.name, d.base_url, d.platform_kind, d.brand_family,
                    d.platform_version, d.platform_snapshot,
                    COALESCE(o.key_cipher, '') != ''
             FROM destinations d
             LEFT JOIN credentials o ON o.id = d.observer_credential_id
             WHERE d.legacy_kind = 'platform_parent'
             ORDER BY d.rowid",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, bool>(7)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);
        let mapped = rows
            .into_iter()
            .map(
                |(
                    id,
                    name,
                    base_url,
                    platform_kind,
                    brand_family,
                    version,
                    snapshot,
                    has_cipher,
                )| {
                    parent_from_destination_row(
                        id,
                        name,
                        base_url,
                        platform_kind,
                        brand_family,
                        version,
                        snapshot,
                        has_cipher,
                    )
                },
            )
            .collect::<Result<Vec<_>>>()?;
        if !mapped.is_empty() || !table_exists(conn, "platform_accounts")? {
            return Ok(mapped);
        }
    }
    leftover_platform_accounts_on(conn)
}

fn platform_account_on(conn: &Connection, id: &str) -> Result<Option<PlatformAccount>> {
    if table_exists(conn, "destinations")?
        && table_has_column(conn, "destinations", "platform_kind")?
    {
        let row: Option<(
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<i64>,
            Option<String>,
            bool,
        )> = conn
            .query_row(
                "SELECT d.legacy_id, d.name, d.base_url, d.platform_kind, d.brand_family,
                        d.platform_version, d.platform_snapshot,
                        COALESCE(o.key_cipher, '') != ''
                 FROM destinations d
                 LEFT JOIN credentials o ON o.id = d.observer_credential_id
                 WHERE d.legacy_kind = 'platform_parent' AND d.legacy_id = ?1",
                [id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                    ))
                },
            )
            .optional()?;
        if let Some((
            id,
            name,
            base_url,
            platform_kind,
            brand_family,
            version,
            snapshot,
            has_cipher,
        )) = row
        {
            return Ok(Some(parent_from_destination_row(
                id,
                name,
                base_url,
                platform_kind,
                brand_family,
                version,
                snapshot,
                has_cipher,
            )?));
        }
        if !table_exists(conn, "platform_accounts")? {
            return Ok(None);
        }
    }
    leftover_platform_account_on(conn, id)
}

fn list_platform_links_on(conn: &Connection) -> Result<Vec<PlatformLink>> {
    if table_exists(conn, "credentials")?
        && table_exists(conn, "destinations")?
        && table_has_column(conn, "credentials", "group_json")?
    {
        let purpose_filter = if table_has_column(conn, "credentials", "credential_purpose")? {
            "AND COALESCE(c.credential_purpose, 'inference') = 'inference'"
        } else {
            ""
        };
        let mut stmt = conn.prepare(&format!(
            "SELECT c.legacy_account_id, d.legacy_id, c.group_json, c.link_snapshot
             FROM credentials c
             JOIN destinations d ON d.id = c.destination_id
             WHERE d.legacy_kind = 'platform_parent'
               AND c.group_json IS NOT NULL
               {purpose_filter}
             ORDER BY c.rowid"
        ))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);
        let mapped = rows
            .into_iter()
            .map(|(account_id, platform_account_id, group_json, snapshot)| {
                let group = match group_json {
                    Some(value) => serde_json::from_str(&value)?,
                    None => PlatformGroup::default(),
                };
                Ok(PlatformLink {
                    account_id,
                    platform_account_id,
                    group,
                    snapshot: parse_platform_snapshot(snapshot)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        if !mapped.is_empty() || !table_exists(conn, "platform_links")? {
            return Ok(mapped);
        }
    }
    leftover_platform_links_on(conn)
}

fn observer_key_cipher_on(conn: &Connection, parent_id: &str) -> Result<Option<String>> {
    let observer_id = observer_credential_id_for_platform_account(parent_id).to_string();
    let value: Option<String> = conn
        .query_row(
            "SELECT key_cipher FROM credentials WHERE id = ?1",
            [&observer_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(value.filter(|cipher| !cipher.is_empty()))
}

fn upsert_platform_parent_on(
    conn: &Connection,
    id: &str,
    kind: PlatformKind,
    name: &str,
    base_url: &str,
    credential_cipher: Option<&str>,
    version: Option<u64>,
    snapshot: Option<&str>,
    bump_if_exists: bool,
) -> Result<()> {
    let destination = destination_from_legacy(&LegacyDestinationFacts::PlatformParent {
        id: id.to_string(),
        kind: domain_kind(kind),
        name: name.to_string(),
        base_url: base_url.to_string(),
        has_user_credential: credential_cipher.is_some_and(|value| !value.is_empty()),
    })
    .map_err(|error| anyhow::anyhow!("{error}"))?;
    let dest_id = destination.id.clone();
    let observer_id = observer_credential_id_for_platform_account(id).to_string();
    let version = if destination_exists(conn, &dest_id)? {
        if bump_if_exists {
            let current: i64 = conn.query_row(
                "SELECT COALESCE(platform_version, 1) FROM destinations WHERE id = ?1",
                [&dest_id],
                |row| row.get(0),
            )?;
            current as u64 + 1
        } else {
            version.unwrap_or(1)
        }
    } else {
        version.unwrap_or_else(fresh_version)
    };
    if destination_exists(conn, &dest_id)? {
        conn.execute(
            "UPDATE destinations
             SET name = ?2, base_url = ?3, brand_family = ?4, platform_kind = ?5,
                 protocols_json = ?6, auth_scheme = ?7, adapter = ?8,
                 model_resolution = 'public_only', capabilities_json = ?9,
                 max_credentials = ?10, enabled = ?11,
                 observer_credential_id = ?12, platform_version = ?13,
                 platform_snapshot = CASE WHEN ?14 THEN ?15 ELSE platform_snapshot END
             WHERE id = ?1",
            params![
                dest_id,
                destination.name,
                destination.base_url,
                destination.brand_family,
                kind_sql(kind),
                serde_json::to_string(&destination.protocols)?,
                destination.auth_scheme.as_str(),
                destination.adapter.as_str(),
                serde_json::to_string(&destination.capabilities)?,
                destination.max_credentials.map(i64::from),
                i64::from(destination.enabled),
                observer_id,
                version as i64,
                snapshot.is_some() || !bump_if_exists,
                snapshot,
            ],
        )?;
    } else {
        conn.execute(
            "INSERT INTO destinations (
                id, legacy_kind, legacy_id, adapter, name, brand_family, base_url,
                protocols_json, auth_scheme, model_resolution, capabilities_json, plan_json,
                max_credentials, observer_credential_id, enabled,
                platform_kind, platform_version, platform_snapshot
             ) VALUES (?1, 'platform_parent', ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'public_only', ?9, NULL, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                dest_id,
                id,
                destination.adapter.as_str(),
                destination.name,
                destination.brand_family,
                destination.base_url,
                serde_json::to_string(&destination.protocols)?,
                destination.auth_scheme.as_str(),
                serde_json::to_string(&destination.capabilities)?,
                destination.max_credentials.map(i64::from),
                observer_id,
                i64::from(destination.enabled),
                kind_sql(kind),
                version as i64,
                snapshot,
            ],
        )?;
    }
    upsert_observer_credential_on(
        conn,
        id,
        &dest_id,
        name,
        credential_cipher,
        !bump_if_exists || credential_cipher.is_some(),
    )?;
    Ok(())
}

fn upsert_observer_credential_on(
    conn: &Connection,
    parent_id: &str,
    dest_id: &str,
    name: &str,
    credential_cipher: Option<&str>,
    write_cipher: bool,
) -> Result<()> {
    let observer_id = observer_credential_id_for_platform_account(parent_id).to_string();
    let identity_id = identity_id_for_platform_account(parent_id).to_string();
    let scope_json = serde_json::to_string(&ocg_domain::credential::ModelScope::All)?;
    let existing: Option<String> = conn
        .query_row(
            "SELECT key_cipher FROM credentials WHERE id = ?1",
            [&observer_id],
            |row| row.get(0),
        )
        .optional()?;
    let cipher = if write_cipher {
        credential_cipher.unwrap_or("").to_string()
    } else {
        existing.clone().unwrap_or_default()
    };
    let has_secret = !cipher.is_empty();
    if existing.is_some() {
        conn.execute(
            "UPDATE credentials
             SET destination_id = ?2, name = ?3, has_secret = ?4,
                 credential_purpose = ?5, identity_id = COALESCE(identity_id, ?6),
                 key_cipher = CASE WHEN ?7 THEN ?8 ELSE key_cipher END
             WHERE id = ?1",
            params![
                observer_id,
                dest_id,
                name,
                i64::from(
                    has_secret
                        || (!write_cipher && existing.as_deref().is_some_and(|v| !v.is_empty()))
                ),
                CREDENTIAL_PURPOSE_PLATFORM_OBSERVER,
                identity_id,
                write_cipher,
                cipher,
            ],
        )?;
        if write_cipher {
            conn.execute(
                "UPDATE credentials SET has_secret = ?2, key_cipher = ?3 WHERE id = ?1",
                params![observer_id, i64::from(has_secret), cipher],
            )?;
        }
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
            NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, ?7, ?8, ?9, NULL
         )",
        params![
            observer_id,
            observer_id,
            dest_id,
            name,
            i64::from(has_secret),
            scope_json,
            cipher,
            CREDENTIAL_PURPOSE_PLATFORM_OBSERVER,
            identity_id,
        ],
    )?;
    Ok(())
}

fn delete_observer_credential_on(conn: &Connection, parent_id: &str) -> Result<()> {
    let observer_id = observer_credential_id_for_platform_account(parent_id).to_string();
    conn.execute(
        "DELETE FROM credential_grants WHERE credential_id = ?1",
        [&observer_id],
    )?;
    conn.execute("DELETE FROM credentials WHERE id = ?1", [&observer_id])?;
    Ok(())
}

fn linked_account_ids_for_parent(conn: &Connection, parent_id: &str) -> Result<Vec<String>> {
    if table_exists(conn, "credentials")? && table_has_column(conn, "credentials", "group_json")? {
        let dest_id = destination_id_for_platform_account(parent_id);
        let purpose_filter = if table_has_column(conn, "credentials", "credential_purpose")? {
            "AND COALESCE(credential_purpose, 'inference') = 'inference'"
        } else {
            ""
        };
        let mut stmt = conn.prepare(&format!(
            "SELECT legacy_account_id FROM credentials
             WHERE destination_id = ?1
               AND group_json IS NOT NULL
               {purpose_filter}"
        ))?;
        let rows = stmt.query_map([&dest_id], |row| row.get(0))?;
        let mapped = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        if !mapped.is_empty() || !table_exists(conn, "platform_links")? {
            return Ok(mapped);
        }
    }
    if table_exists(conn, "platform_links")? {
        let mut stmt =
            conn.prepare("SELECT account_id FROM platform_links WHERE platform_account_id = ?1")?;
        return stmt
            .query_map([parent_id], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into);
    }
    Ok(Vec::new())
}

fn clear_link_snapshots_for_parent(conn: &Connection, parent_id: &str) -> Result<()> {
    let dest_id = destination_id_for_platform_account(parent_id);
    conn.execute(
        "UPDATE credentials
         SET link_snapshot = NULL, link_version = COALESCE(link_version, 0) + 1
         WHERE destination_id = ?1
           AND COALESCE(credential_purpose, 'inference') = 'inference'",
        [&dest_id],
    )?;
    Ok(())
}

fn link_version_on(conn: &Connection, account_id: &str, parent_id: &str) -> Result<i64> {
    let dest_id = destination_id_for_platform_account(parent_id);
    Ok(conn.query_row(
        "SELECT COALESCE(link_version, 0) FROM credentials
         WHERE legacy_account_id = ?1 AND destination_id = ?2
           AND COALESCE(credential_purpose, 'inference') = 'inference'",
        params![account_id, dest_id],
        |row| row.get(0),
    )?)
}

pub(crate) fn apply_platform_link_on(
    conn: &Connection,
    account_id: &str,
    parent_id: &str,
    group: &PlatformGroup,
) -> Result<()> {
    let account = account_store::get_account_on(conn, account_id)?.context("account not found")?;
    anyhow::ensure!(
        is_custom_api(&account.provider_id),
        "only Custom API Keys can be linked"
    );
    let parent = platform_account_on(conn, parent_id)?.context("platform account not found")?;
    let _custom = custom_store::account_custom_config_on(conn, account_id)?
        .context("Custom configuration missing")?;
    let mut group = group.clone();
    group.verified = false;
    group.subscription_type = None;
    anyhow::ensure!(
        group
            .id
            .as_ref()
            .is_none_or(|v| v.len() <= 200 && !v.chars().any(char::is_control))
            && group
                .platform
                .as_ref()
                .is_none_or(|v| v.len() <= 64 && !v.chars().any(char::is_control))
            && group.auto_groups.len() <= 50
            && group
                .auto_groups
                .iter()
                .all(|v| v.len() <= 200 && !v.chars().any(char::is_control)),
        "invalid group identity"
    );
    // Read and narrow from the source Custom destination before moving this
    // one credential. A shared connection is destination-owned: linking one
    // Key must never rewrite or delete the siblings' transport.
    custom_store::merge_custom_models_onto_platform_parent(conn, account_id, parent_id)?;
    custom_store::narrow_credential_scope_from_custom_destination(conn, account_id)?;
    set_link_on(conn, account_id, parent_id, &group, None)?;
    identity::update_account_identity_declaration(
        conn,
        account_id,
        Some(&ocg_domain::credential::DeclaredPlatformRelation {
            platform_account_id: parent_id.to_string(),
            group: identity::platform_group_label(&group),
            parent_base_url: parent.base_url,
        }),
        Utc::now(),
    )?;
    Ok(())
}

fn set_link_on(
    conn: &Connection,
    account_id: &str,
    parent_id: &str,
    group: &PlatformGroup,
    version: Option<u64>,
) -> Result<()> {
    let dest_id = destination_id_for_platform_account(parent_id);
    let version = version.unwrap_or_else(fresh_version);
    conn.execute(
        "UPDATE credentials
         SET destination_id = ?2, group_json = ?3, link_version = ?4, link_snapshot = NULL
         WHERE legacy_account_id = ?1",
        params![
            account_id,
            dest_id,
            serde_json::to_string(group)?,
            version as i64
        ],
    )?;
    Ok(())
}

fn clear_link_on(conn: &Connection, account_id: &str) -> Result<Option<String>> {
    let parent_id = custom_store::platform_parent_id(conn, account_id)?;
    conn.execute(
        "UPDATE credentials
         SET group_json = NULL, link_version = NULL, link_snapshot = NULL
         WHERE legacy_account_id = ?1
           AND COALESCE(credential_purpose, 'inference') = 'inference'",
        [account_id],
    )?;
    Ok(parent_id)
}

pub(crate) fn platform_parent_base_url(
    conn: &Connection,
    parent_id: &str,
) -> Result<Option<String>> {
    if table_exists(conn, "destinations")? {
        if let Some(base_url) = conn
            .query_row(
                "SELECT base_url FROM destinations
                 WHERE legacy_kind = 'platform_parent' AND legacy_id = ?1",
                [parent_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten()
        {
            return Ok(Some(base_url));
        }
    }
    if table_exists(conn, "platform_accounts")? {
        return Ok(conn
            .query_row(
                "SELECT base_url FROM platform_accounts WHERE id = ?1",
                [parent_id],
                |row| row.get(0),
            )
            .optional()?);
    }
    Ok(None)
}

pub(crate) struct PlatformParentIdentityRow {
    pub platform_id: String,
    pub name: String,
    pub base_url: String,
    pub has_credential: bool,
}

pub(crate) fn list_platform_parent_identity_rows(
    conn: &Connection,
) -> Result<Vec<PlatformParentIdentityRow>> {
    if table_exists(conn, "destinations")? {
        let mut stmt = conn.prepare(
            "SELECT d.legacy_id, d.name, COALESCE(d.base_url, ''),
                    COALESCE(o.key_cipher, '') != ''
             FROM destinations d
             LEFT JOIN credentials o ON o.id = d.observer_credential_id
             WHERE d.legacy_kind = 'platform_parent'
             ORDER BY d.rowid",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(PlatformParentIdentityRow {
                    platform_id: row.get(0)?,
                    name: row.get(1)?,
                    base_url: row.get(2)?,
                    has_credential: row.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if !rows.is_empty() || !table_exists(conn, "platform_accounts")? {
            return Ok(rows);
        }
    }
    if table_exists(conn, "platform_accounts")? {
        let mut stmt = conn.prepare(
            "SELECT id, name, base_url, credential_cipher IS NOT NULL
             FROM platform_accounts ORDER BY rowid",
        )?;
        return stmt
            .query_map([], |row| {
                Ok(PlatformParentIdentityRow {
                    platform_id: row.get(0)?,
                    name: row.get(1)?,
                    base_url: row.get(2)?,
                    has_credential: row.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into);
    }
    Ok(Vec::new())
}

pub(crate) fn list_declared_platform_relations(
    conn: &Connection,
) -> Result<Vec<(String, String, String, String)>> {
    if table_exists(conn, "credentials")?
        && table_exists(conn, "destinations")?
        && table_has_column(conn, "credentials", "group_json")?
    {
        let purpose_filter = if table_has_column(conn, "credentials", "credential_purpose")? {
            "AND COALESCE(c.credential_purpose, 'inference') = 'inference'"
        } else {
            ""
        };
        let group_expr = if table_has_column(conn, "credentials", "group_json")? {
            "COALESCE(c.group_json, '{}')"
        } else {
            "'{}'"
        };
        let group_filter = if table_has_column(conn, "credentials", "group_json")? {
            "AND c.group_json IS NOT NULL"
        } else {
            ""
        };
        let mut stmt = conn.prepare(&format!(
            "SELECT c.legacy_account_id, d.legacy_id, {group_expr},
                    COALESCE(d.base_url, '')
             FROM credentials c
             JOIN destinations d ON d.id = c.destination_id
             WHERE d.legacy_kind = 'platform_parent'
               {purpose_filter}
               {group_filter}"
        ))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if !rows.is_empty() || !table_exists(conn, "platform_links")? {
            return Ok(rows);
        }
    }
    if table_exists(conn, "platform_links")? && table_exists(conn, "platform_accounts")? {
        let mut stmt = conn.prepare(
            "SELECT l.account_id, l.platform_account_id, l.group_json, p.base_url
             FROM platform_links l
             JOIN platform_accounts p ON p.id = l.platform_account_id",
        )?;
        return stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into);
    }
    Ok(Vec::new())
}

#[derive(Debug, Clone)]
pub(crate) struct DestinationPlatformExtras {
    pub id: String,
    pub platform_kind: Option<String>,
    pub platform_version: Option<i64>,
    pub platform_snapshot: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ObserverCredentialRow {
    pub id: String,
    pub destination_id: String,
    pub name: String,
    pub key_cipher: String,
    pub has_secret: bool,
    pub identity_id: Option<String>,
    pub purpose: String,
}

pub(crate) fn snapshot_destination_platform_extras(
    conn: &Connection,
) -> Result<Vec<DestinationPlatformExtras>> {
    if !table_exists(conn, "destinations")?
        || !table_has_column(conn, "destinations", "platform_version")?
    {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT id, platform_kind, platform_version, platform_snapshot
         FROM destinations
         WHERE legacy_kind = 'platform_parent'",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok(DestinationPlatformExtras {
                id: row.get(0)?,
                platform_kind: row.get(1)?,
                platform_version: row.get(2)?,
                platform_snapshot: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub(crate) fn restore_destination_platform_extras(
    conn: &Connection,
    extras: &[DestinationPlatformExtras],
) -> Result<()> {
    if extras.is_empty() || !table_has_column(conn, "destinations", "platform_version")? {
        return Ok(());
    }
    for extra in extras {
        conn.execute(
            "UPDATE destinations
             SET platform_kind = COALESCE(?2, platform_kind),
                 platform_version = COALESCE(?3, platform_version),
                 platform_snapshot = CASE WHEN ?4 IS NOT NULL THEN ?4 ELSE platform_snapshot END
             WHERE id = ?1",
            params![
                extra.id,
                extra.platform_kind,
                extra.platform_version,
                extra.platform_snapshot,
            ],
        )?;
    }
    Ok(())
}

pub(crate) fn snapshot_observer_credentials(
    conn: &Connection,
) -> Result<Vec<ObserverCredentialRow>> {
    if !table_exists(conn, "credentials")?
        || !table_has_column(conn, "credentials", "credential_purpose")?
    {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT id, destination_id, name, COALESCE(key_cipher, ''), has_secret, identity_id,
                COALESCE(credential_purpose, ?1)
         FROM credentials
         WHERE credential_purpose IN (?1, ?2)
            OR id IN (
                SELECT observer_credential_id FROM destinations
                 WHERE observer_credential_id IS NOT NULL
            )",
    )?;
    let rows = stmt
        .query_map(
            [
                CREDENTIAL_PURPOSE_PLATFORM_OBSERVER,
                crate::db::cpa::CREDENTIAL_PURPOSE_CPA_OBSERVER,
            ],
            |row| {
                Ok(ObserverCredentialRow {
                    id: row.get(0)?,
                    destination_id: row.get(1)?,
                    name: row.get(2)?,
                    key_cipher: row.get(3)?,
                    has_secret: row.get::<_, i64>(4)? != 0,
                    identity_id: row.get(5)?,
                    purpose: row.get(6)?,
                })
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub(crate) fn restore_observer_credentials(
    conn: &Connection,
    rows: &[ObserverCredentialRow],
) -> Result<()> {
    if rows.is_empty() || !table_has_column(conn, "credentials", "credential_purpose")? {
        return Ok(());
    }
    let scope_json = serde_json::to_string(&ocg_domain::credential::ModelScope::All)?;
    for row in rows {
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM credentials WHERE id = ?1)",
            [&row.id],
            |r| r.get::<_, i64>(0),
        )? != 0;
        if exists {
            conn.execute(
                "UPDATE credentials
                 SET destination_id = ?2, name = ?3, key_cipher = ?4, has_secret = ?5,
                     credential_purpose = ?6, identity_id = COALESCE(?7, identity_id)
                 WHERE id = ?1",
                params![
                    row.id,
                    row.destination_id,
                    row.name,
                    row.key_cipher,
                    i64::from(row.has_secret),
                    row.purpose,
                    row.identity_id,
                ],
            )?;
            continue;
        }
        conn.execute(
            "INSERT INTO credentials (
                id, legacy_account_id, destination_id, name, notes, has_secret,
                enabled, routing_rank, scope_json, auth_state, key_cipher,
                credential_purpose, identity_id, provider_id
             ) VALUES (
                ?1, ?2, ?3, ?4, NULL, ?5, 0, -1, ?6, 'unknown', ?7, ?8, ?9, NULL
             )",
            params![
                row.id,
                row.id,
                row.destination_id,
                row.name,
                i64::from(row.has_secret),
                scope_json,
                row.key_cipher,
                row.purpose,
                row.identity_id,
            ],
        )?;
    }
    Ok(())
}

pub(super) fn merge_platforms_on(
    conn: &Connection,
    parents: &[crate::platform::PortablePlatformAccount],
    links: &[crate::platform::PortablePlatformLink],
    imported_account_ids: &HashSet<String>,
) -> Result<()> {
    for parent in parents {
        let kind = kind_sql(parent.kind);
        if let Some(existing) = platform_account_on(conn, &parent.id)? {
            anyhow::ensure!(
                kind_sql(existing.kind) == kind && existing.base_url == parent.base_url,
                "imported platform identity conflicts with immutable origin"
            );
        }
        upsert_platform_parent_on(
            conn,
            &parent.id,
            parent.kind,
            &parent.name,
            &parent.base_url,
            None,
            Some(fresh_version()),
            Some(""),
            false,
        )?;
        conn.execute(
            "UPDATE destinations SET platform_snapshot = NULL
             WHERE legacy_kind = 'platform_parent' AND legacy_id = ?1",
            [&parent.id],
        )?;
        identity::persist_platform_identity(
            conn,
            &parent.id,
            &parent.name,
            &parent.base_url,
            Utc::now(),
        )?;
    }
    let import_links: Vec<(String, String)> = links
        .iter()
        .map(|link| (link.account_id.clone(), link.platform_account_id.clone()))
        .collect();
    for link in links {
        let mut group = link.group.clone();
        group.verified = false;
        set_link_on(
            conn,
            &link.account_id,
            &link.platform_account_id,
            &group,
            Some(fresh_version()),
        )?;
        if let Some(base_url) = platform_parent_base_url(conn, &link.platform_account_id)? {
            identity::update_account_identity_declaration(
                conn,
                &link.account_id,
                Some(&ocg_domain::credential::DeclaredPlatformRelation {
                    platform_account_id: link.platform_account_id.clone(),
                    group: identity::platform_group_label(&group),
                    parent_base_url: base_url,
                }),
                Utc::now(),
            )?;
        }
    }
    custom_store::merge_linked_custom_models_for_import(conn, &import_links)?;
    let source = crate::db::account_store::account_row_source(conn)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT c.{id}, d.base_url, a.provider_id
         FROM credentials c
         JOIN destinations d ON d.id = c.destination_id
         JOIN {table} a ON a.{id} = c.legacy_account_id
         WHERE d.legacy_kind = 'platform_parent'
           AND COALESCE(c.credential_purpose, 'inference') = 'inference'",
        table = source.table,
        id = source.id_col,
    ))?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    for (id, base, provider) in rows {
        if !imported_account_ids.contains(&id.to_ascii_lowercase()) {
            continue;
        }
        let provider = provider.unwrap_or_default();
        anyhow::ensure!(
            is_custom_api(&provider),
            "linked account must remain Custom API"
        );
        let Some(base) = base else {
            continue;
        };
        let _endpoint = crate::platform::hosted_endpoint(&base)?;
        conn.execute(
            "UPDATE credentials
             SET link_snapshot = NULL, link_version = COALESCE(link_version, 0) + 1
             WHERE legacy_account_id = ?1",
            [id],
        )?;
    }
    Ok(())
}

pub(crate) fn clear_link_snapshot_for_account(conn: &Connection, account_id: &str) -> Result<()> {
    conn.execute(
        "UPDATE credentials
         SET link_snapshot = NULL, link_version = COALESCE(link_version, 0) + 1
         WHERE legacy_account_id = ?1
           AND COALESCE(credential_purpose, 'inference') = 'inference'
           AND group_json IS NOT NULL",
        [account_id],
    )?;
    Ok(())
}

pub(crate) fn unlink_imported_accounts(conn: &Connection, account_ids: &[String]) -> Result<()> {
    for account_id in account_ids {
        let linked = custom_store::platform_parent_id(conn, account_id)?.is_some();
        if linked {
            // `group_json` alone does not unlink: readers treat a platform
            // parent `destination_id` as linked even when group evidence is gone.
            let custom_dest = destination_id_for_custom_account(account_id);
            conn.execute(
                "UPDATE credentials
                 SET group_json = NULL, link_version = NULL, link_snapshot = NULL,
                     destination_id = ?2
                 WHERE legacy_account_id = ?1
                   AND COALESCE(credential_purpose, 'inference') = 'inference'",
                params![account_id, custom_dest],
            )?;
        } else {
            conn.execute(
                "UPDATE credentials
                 SET group_json = NULL, link_version = NULL, link_snapshot = NULL
                 WHERE legacy_account_id = ?1
                   AND COALESCE(credential_purpose, 'inference') = 'inference'",
                [account_id],
            )?;
        }
    }
    Ok(())
}

impl Database {
    pub(crate) fn platform_hosted_endpoint(&self, id: &str) -> Result<Option<String>> {
        let parent_id = custom_store::platform_parent_id(&self.conn, id)?;
        let Some(parent_id) = parent_id else {
            return Ok(None);
        };
        let base = platform_parent_base_url(&self.conn, &parent_id)?;
        base.map(|b| crate::platform::hosted_endpoint(&b))
            .transpose()
    }

    pub fn list_platform_accounts(&self) -> Result<Vec<PlatformAccount>> {
        list_platform_accounts_on(&self.conn)
    }

    pub fn platform_account(&self, id: &str) -> Result<Option<PlatformAccount>> {
        platform_account_on(&self.conn, id)
    }

    pub(crate) fn platform_credential_cipher(&self, id: &str) -> Result<Option<String>> {
        observer_key_cipher_on(&self.conn, id)
    }

    pub(crate) fn create_platform_account(
        &self,
        id: &str,
        kind: PlatformKind,
        name: &str,
        base_url: &str,
        credential_cipher: Option<&str>,
    ) -> Result<()> {
        let base_url = crate::platform::validate_platform_base_url(base_url)?;
        anyhow::ensure!(
            !name.trim().is_empty() && name.len() <= 200,
            "invalid platform name"
        );
        let tx = self.conn.unchecked_transaction()?;
        upsert_platform_parent_on(
            &tx,
            id,
            kind,
            name.trim(),
            &base_url,
            credential_cipher,
            None,
            None,
            false,
        )?;
        identity::persist_platform_identity(&tx, id, name.trim(), &base_url, Utc::now())?;
        tx.commit()?;
        Ok(())
    }

    /// None preserves a credential; Some(None) explicitly clears it.
    pub(crate) fn update_platform_account(
        &self,
        id: &str,
        name: &str,
        credential: Option<Option<&str>>,
    ) -> Result<()> {
        anyhow::ensure!(
            !name.trim().is_empty() && name.len() <= 200,
            "invalid platform name"
        );
        anyhow::ensure!(
            self.platform_account(id)?.is_some(),
            "platform account not found"
        );
        let tx = self.conn.unchecked_transaction()?;
        let dest_id = destination_id_for_platform_account(id);
        let count = tx.execute(
            "UPDATE destinations
             SET name = ?2, platform_version = COALESCE(platform_version, 1) + 1
             WHERE id = ?1",
            params![dest_id, name.trim()],
        )?;
        anyhow::ensure!(count == 1, "platform account not found");
        let base_url = platform_parent_base_url(&tx, id)?.unwrap_or_default();
        identity::persist_platform_identity(&tx, id, name.trim(), &base_url, Utc::now())?;
        upsert_observer_credential_on(
            &tx,
            id,
            &dest_id,
            name.trim(),
            match credential {
                Some(value) => value,
                None => None,
            },
            credential.is_some(),
        )?;
        if credential.is_some() {
            tx.execute(
                "UPDATE destinations SET platform_snapshot = NULL WHERE id = ?1",
                [&dest_id],
            )?;
            clear_link_snapshots_for_parent(&tx, id)?;
        }
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn delete_platform_account(&self, id: &str) -> Result<()> {
        anyhow::ensure!(
            linked_account_ids_for_parent(&self.conn, id)?.is_empty(),
            "unlink Keys before deleting the platform account"
        );
        let dest_id = destination_id_for_platform_account(id);
        let tx = self.conn.unchecked_transaction()?;
        anyhow::ensure!(
            destination_exists(&tx, &dest_id)?,
            "platform account not found"
        );
        delete_observer_credential_on(&tx, id)?;
        tx.execute(
            "DELETE FROM destination_models WHERE destination_id = ?1",
            [&dest_id],
        )?;
        anyhow::ensure!(
            tx.execute("DELETE FROM destinations WHERE id = ?1", [&dest_id])? == 1,
            "platform account not found"
        );
        identity::delete_platform_identity(&tx, id)?;
        tx.commit()?;
        Ok(())
    }

    pub fn list_platform_links(&self) -> Result<Vec<PlatformLink>> {
        list_platform_links_on(&self.conn)
    }

    pub(crate) fn link_platform_account(
        &self,
        account_id: &str,
        parent_id: &str,
        group: &PlatformGroup,
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        apply_platform_link_on(&tx, account_id, parent_id, group)?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn unlink_platform_account(&self, account_id: &str) -> Result<()> {
        let parent_id = custom_store::platform_parent_id(&self.conn, account_id)?;
        let tx = self.conn.unchecked_transaction()?;
        clear_link_on(&tx, account_id)?;
        identity::update_account_identity_declaration(&tx, account_id, None, Utc::now())?;
        if let Some(parent_id) = parent_id {
            custom_store::persist_custom_destination_after_unlink(&tx, account_id, &parent_id)?;
        }
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn platform_refresh_token(
        &self,
        parent_id: &str,
        account_id: Option<&str>,
    ) -> Result<String> {
        let parent = self
            .platform_account(parent_id)?
            .context("platform account not found")?;
        let mut token = format!("{}:{}", parent.id, parent.version);
        if let Some(id) = account_id {
            let version = link_version_on(&self.conn, id, parent_id)?;
            let account = self.get_account(id)?.context("account not found")?;
            // Private comparison token; never returned or logged.
            token.push_str(&format!(":{id}:{version}:{}", account.key_cipher));
        }
        Ok(token)
    }

    pub(crate) fn save_platform_refresh(
        &self,
        parent_id: &str,
        account_id: Option<&str>,
        token: &str,
        snapshot: &PlatformSnapshot,
    ) -> Result<bool> {
        if self
            .platform_refresh_token(parent_id, account_id)
            .ok()
            .as_deref()
            != Some(token)
        {
            return Ok(false);
        }
        let previous = if let Some(id) = account_id {
            self.list_platform_links()?
                .into_iter()
                .find(|l| l.account_id == id)
                .and_then(|l| l.snapshot)
        } else {
            self.platform_account(parent_id)?.and_then(|p| p.snapshot)
        };
        let mut saved = snapshot.clone();
        if snapshot.stale
            && let Some(mut old) = previous
        {
            old.stale = true;
            old.errors = snapshot.errors.clone();
            saved = old;
        }
        let json = serde_json::to_string(&saved)?;
        if let Some(id) = account_id {
            self.conn.execute(
                "UPDATE credentials
                 SET link_snapshot = ?2, link_version = COALESCE(link_version, 0) + 1
                 WHERE legacy_account_id = ?1",
                params![id, json],
            )?;
        } else {
            let dest_id = destination_id_for_platform_account(parent_id);
            self.conn.execute(
                "UPDATE destinations
                 SET platform_snapshot = ?2,
                     platform_version = COALESCE(platform_version, 1) + 1
                 WHERE id = ?1",
                params![dest_id, json],
            )?;
        }
        Ok(true)
    }
}
