//! Credential-local GOAT plan-window deadlines.
//!
//! The map is one credential's contribution. Ordinary cooldown columns stay
//! ordinary, including when a shared pool copies them. Effective eligibility
//! is the later of the two and is computed at the read boundary.

use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::db::table_has_column;
use crate::models::UsageWindowKind;

const COLUMN: &str = "goat_plan_cooldowns_json";
const PLAN_PREFIX: &str = "You've reached your ";
const PLAN_MIDDLE: &str = " usage limit for your plan. Your limit resets at ";
const PLAN_SUFFIX: &str = ". Please wait for the window to reset or upgrade your plan to continue.";

/// Closed absolute deadlines for the three GOAT plan windows.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GoatPlanCooldowns {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_instant",
        deserialize_with = "deserialize_instant"
    )]
    pub five_hours: Option<DateTime<Utc>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_instant",
        deserialize_with = "deserialize_instant"
    )]
    pub week: Option<DateTime<Utc>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_instant",
        deserialize_with = "deserialize_instant"
    )]
    pub month: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GoatPlanSnapshot {
    pub credential_id: String,
    pub legacy_account_id: String,
    pub destination_id: String,
    pub key_cipher: String,
    pub json: String,
}

impl GoatPlanCooldowns {
    pub(crate) fn is_empty(&self) -> bool {
        self.five_hours.is_none() && self.week.is_none() && self.month.is_none()
    }

    pub(crate) fn deadline(&self, window: UsageWindowKind) -> Option<DateTime<Utc>> {
        match window {
            UsageWindowKind::FiveHours => self.five_hours,
            UsageWindowKind::Week => self.week,
            UsageWindowKind::Month => self.month,
            UsageWindowKind::Free => None,
        }
    }

    /// Keep the later deadline for one window. An earlier or equal instant
    /// does not shorten a deadline already stored.
    pub(crate) fn raise(&mut self, window: UsageWindowKind, reset: DateTime<Utc>) -> bool {
        let slot = match window {
            UsageWindowKind::FiveHours => &mut self.five_hours,
            UsageWindowKind::Week => &mut self.week,
            UsageWindowKind::Month => &mut self.month,
            UsageWindowKind::Free => return false,
        };
        match *slot {
            Some(current) if reset <= current => false,
            _ => {
                *slot = Some(reset);
                true
            }
        }
    }

    pub(crate) fn merge_max(&mut self, other: &Self) {
        for window in [
            UsageWindowKind::FiveHours,
            UsageWindowKind::Week,
            UsageWindowKind::Month,
        ] {
            if let Some(reset) = other.deadline(window) {
                self.raise(window, reset);
            }
        }
    }

    pub(crate) fn latest(&self) -> Option<DateTime<Utc>> {
        [self.five_hours, self.week, self.month]
            .into_iter()
            .flatten()
            .max()
    }
}

/// Exact GOAT plan sentence. The reset must be strictly after `observed_at`.
/// Anything else, including a truncated sentence, is not this signal.
pub(crate) fn declared_plan_window(
    body: &str,
    observed_at: DateTime<Utc>,
) -> Option<(UsageWindowKind, DateTime<Utc>)> {
    let (window, reset) = declared_plan_message(body)?;
    (reset > observed_at).then_some((window, reset))
}

fn declared_plan_message(body: &str) -> Option<(UsageWindowKind, DateTime<Utc>)> {
    let parsed: GoatErrorBody = serde_json::from_str(body).ok()?;
    if parsed.error.code != "RATE_LIMITED" || parsed.error.kind != "rate_limit_error" {
        return None;
    }
    let rest = parsed.error.message.strip_prefix(PLAN_PREFIX)?;
    let (token, rest) = rest.split_once(PLAN_MIDDLE)?;
    let (stamp, tail) = rest.split_once(PLAN_SUFFIX)?;
    if !tail.is_empty() {
        return None;
    }
    let window = match token {
        "5-hour" => UsageWindowKind::FiveHours,
        "weekly" => UsageWindowKind::Week,
        "monthly" => UsageWindowKind::Month,
        _ => return None,
    };
    let reset = DateTime::parse_from_rfc3339(stamp)
        .ok()?
        .with_timezone(&Utc);
    Some((window, reset))
}

#[derive(Deserialize)]
struct GoatErrorBody {
    error: GoatErrorObject,
}

#[derive(Deserialize)]
struct GoatErrorObject {
    code: String,
    #[serde(rename = "type")]
    kind: String,
    message: String,
}

pub(crate) fn format_instant(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::AutoSi, true)
}

pub(crate) fn overlay_instant(
    ordinary: Option<DateTime<Utc>>,
    local: Option<DateTime<Utc>>,
) -> Option<DateTime<Utc>> {
    match (ordinary, local) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (Some(left), None) => Some(left),
        (None, Some(right)) => Some(right),
        (None, None) => None,
    }
}

/// Effective account deadlines. Ordinary instants stay in their own fields;
/// the local map only lengthens a window or fills an empty one.
pub(crate) fn overlay_account_deadlines(
    until: Option<DateTime<Utc>>,
    five_hours: Option<DateTime<Utc>>,
    week: Option<DateTime<Utc>>,
    month: Option<DateTime<Utc>>,
    map: Option<&GoatPlanCooldowns>,
) -> (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
) {
    let map = map.cloned().unwrap_or_default();
    (
        overlay_wire(until.map(|at| at.to_rfc3339()), map.latest()),
        overlay_wire(five_hours.map(|at| at.to_rfc3339()), map.five_hours),
        overlay_wire(week.map(|at| at.to_rfc3339()), map.week),
        overlay_wire(month.map(|at| at.to_rfc3339()), map.month),
    )
}

/// Keep an ordinary wire timestamp unless the local deadline is later.
/// Unparseable ordinary text stays as stored; it is not replaced by the map.
pub(crate) fn overlay_wire(
    ordinary: Option<String>,
    local: Option<DateTime<Utc>>,
) -> Option<String> {
    let parsed = ordinary
        .as_deref()
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&Utc));
    match ordinary {
        Some(text) => match parsed {
            Some(at) => match overlay_instant(Some(at), local) {
                Some(chosen) if chosen > at => Some(format_instant(chosen)),
                _ => Some(text),
            },
            None => Some(text),
        },
        None => overlay_instant(None, local).map(format_instant),
    }
}

pub(crate) fn record_window_on(
    conn: &Connection,
    credential_id: &str,
    legacy_account_id: &str,
    binding_id: &str,
    credential_version: u64,
    key_cipher: &str,
    window: UsageWindowKind,
    reset: DateTime<Utc>,
) -> anyhow::Result<bool> {
    if !column_ready(conn)? || matches!(window, UsageWindowKind::Free) {
        return Ok(false);
    }
    // Take the write lock before reading. Two connections must not each merge
    // from a stale map and let the earlier deadline replace the later one.
    if conn.is_autocommit() {
        let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
        let updated = record_window_locked(
            &tx,
            credential_id,
            legacy_account_id,
            binding_id,
            credential_version,
            key_cipher,
            window,
            reset,
        )?;
        tx.commit()?;
        Ok(updated)
    } else {
        record_window_locked(
            conn,
            credential_id,
            legacy_account_id,
            binding_id,
            credential_version,
            key_cipher,
            window,
            reset,
        )
    }
}

fn record_window_locked(
    conn: &Connection,
    credential_id: &str,
    legacy_account_id: &str,
    binding_id: &str,
    credential_version: u64,
    key_cipher: &str,
    window: UsageWindowKind,
    reset: DateTime<Utc>,
) -> anyhow::Result<bool> {
    let row: Option<(String, i64, String, Option<String>)> = conn
        .query_row(
            "SELECT binding_id, credential_version, key_cipher, goat_plan_cooldowns_json
             FROM credentials
             WHERE id = ?1 AND legacy_account_id = ?2",
            params![credential_id, legacy_account_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((binding, version, cipher, json)) = row else {
        return Ok(false);
    };
    if binding != binding_id
        || version <= 0
        || version as u64 != credential_version
        || cipher != key_cipher
    {
        return Ok(false);
    }
    let mut map = parse_stored(json)?.unwrap_or_default();
    map.raise(window, reset);
    store_for_credential(
        conn,
        credential_id,
        legacy_account_id,
        binding_id,
        credential_version,
        key_cipher,
        &map,
    )
}

pub(crate) fn load_for_legacy_on(
    conn: &Connection,
    legacy_account_id: &str,
) -> anyhow::Result<Option<GoatPlanCooldowns>> {
    if !column_ready(conn)? {
        return Ok(None);
    }
    let json: Option<Option<String>> = conn
        .query_row(
            "SELECT goat_plan_cooldowns_json FROM credentials
             WHERE legacy_account_id = ?1
               AND COALESCE(credential_purpose, 'inference') = 'inference'",
            [legacy_account_id],
            |row| row.get(0),
        )
        .optional()?;
    parse_stored(json.flatten())
}

/// Inference maps keyed by the legacy account id. A corrupt stored map is an
/// error. Callers overlay these facts; they do not write the union back.
pub(crate) fn load_by_legacy_on(
    conn: &Connection,
) -> anyhow::Result<std::collections::HashMap<String, GoatPlanCooldowns>> {
    let mut maps = std::collections::HashMap::new();
    if !column_ready(conn)? {
        return Ok(maps);
    }
    let mut statement = conn.prepare(
        "SELECT legacy_account_id, goat_plan_cooldowns_json FROM credentials
         WHERE goat_plan_cooldowns_json IS NOT NULL
           AND COALESCE(credential_purpose, 'inference') = 'inference'",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
    })?;
    for row in rows {
        let (id, json) = row?;
        if let Some(map) = parse_stored(json)? {
            maps.entry(id).or_default().merge_max(&map);
        }
    }
    Ok(maps)
}

pub(crate) fn load_all_on(
    conn: &Connection,
) -> anyhow::Result<std::collections::HashMap<String, GoatPlanCooldowns>> {
    let mut maps = std::collections::HashMap::new();
    if !column_ready(conn)? {
        return Ok(maps);
    }
    let mut statement = conn.prepare(
        "SELECT id, goat_plan_cooldowns_json FROM credentials
         WHERE goat_plan_cooldowns_json IS NOT NULL",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
    })?;
    for row in rows {
        let (id, json) = row?;
        if let Some(map) = parse_stored(json)? {
            maps.insert(id, map);
        }
    }
    Ok(maps)
}

pub(crate) fn clear_for_legacy_on(
    conn: &Connection,
    legacy_account_id: &str,
) -> anyhow::Result<()> {
    if !column_ready(conn)? {
        return Ok(());
    }
    conn.execute(
        "UPDATE credentials SET goat_plan_cooldowns_json = NULL
         WHERE legacy_account_id = ?1",
        [legacy_account_id],
    )?;
    Ok(())
}

/// A replacement ciphertext drops the map. The same stored ciphertext keeps it.
pub(crate) fn clear_if_cipher_changes_on(
    conn: &Connection,
    legacy_account_id: &str,
    new_cipher: &str,
) -> anyhow::Result<()> {
    if !column_ready(conn)? {
        return Ok(());
    }
    conn.execute(
        "UPDATE credentials SET goat_plan_cooldowns_json = NULL
         WHERE legacy_account_id = ?1 AND key_cipher <> ?2",
        params![legacy_account_id, new_cipher],
    )?;
    Ok(())
}

/// `None` means the portable field was absent. An empty map does not clear a
/// same-Key host map. A changed Key drops the old map, then stores a non-empty
/// imported map.
pub(crate) fn apply_import_on(
    conn: &Connection,
    legacy_account_id: &str,
    same_key: bool,
    incoming: Option<&GoatPlanCooldowns>,
) -> anyhow::Result<()> {
    match incoming {
        None if same_key => Ok(()),
        None => clear_for_legacy_on(conn, legacy_account_id),
        Some(map) if map.is_empty() && same_key => Ok(()),
        Some(map) if map.is_empty() => clear_for_legacy_on(conn, legacy_account_id),
        Some(map) if same_key => merge_import_max(conn, legacy_account_id, map),
        Some(map) => store_for_legacy(conn, legacy_account_id, map),
    }
}

pub(crate) fn snapshot_on(conn: &Connection) -> anyhow::Result<Vec<GoatPlanSnapshot>> {
    if !column_ready(conn)? {
        return Ok(Vec::new());
    }
    let mut statement = conn.prepare(
        "SELECT id, legacy_account_id, destination_id, key_cipher, goat_plan_cooldowns_json
         FROM credentials
         WHERE goat_plan_cooldowns_json IS NOT NULL",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(GoatPlanSnapshot {
            credential_id: row.get(0)?,
            legacy_account_id: row.get(1)?,
            destination_id: row.get(2)?,
            key_cipher: row.get(3)?,
            json: row.get(4)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

pub(crate) fn restore_on(conn: &Connection, rows: &[GoatPlanSnapshot]) -> anyhow::Result<()> {
    if rows.is_empty() || !column_ready(conn)? {
        return Ok(());
    }
    for row in rows {
        // A routing-card move may change destination_id. The saved Key is the
        // credential id plus ciphertext; a different Key must not inherit this map.
        conn.execute(
            "UPDATE credentials SET goat_plan_cooldowns_json = ?4
             WHERE id = ?1
               AND legacy_account_id = ?2
               AND key_cipher = ?3",
            params![
                row.credential_id,
                row.legacy_account_id,
                row.key_cipher,
                row.json,
            ],
        )?;
    }
    Ok(())
}

fn merge_import_max(
    conn: &Connection,
    legacy_account_id: &str,
    incoming: &GoatPlanCooldowns,
) -> anyhow::Result<()> {
    let merge = |conn: &Connection| -> anyhow::Result<()> {
        let mut merged = load_for_legacy_on(conn, legacy_account_id)?.unwrap_or_default();
        merged.merge_max(incoming);
        store_for_legacy(conn, legacy_account_id, &merged)
    };
    if conn.is_autocommit() {
        let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
        merge(&tx)?;
        tx.commit()?;
        Ok(())
    } else {
        merge(conn)
    }
}

fn store_for_legacy(
    conn: &Connection,
    legacy_account_id: &str,
    map: &GoatPlanCooldowns,
) -> anyhow::Result<()> {
    if !column_ready(conn)? {
        return Ok(());
    }
    conn.execute(
        "UPDATE credentials SET goat_plan_cooldowns_json = ?2
         WHERE legacy_account_id = ?1
           AND COALESCE(credential_purpose, 'inference') = 'inference'",
        params![legacy_account_id, stored_json(map)?],
    )?;
    Ok(())
}

fn store_for_credential(
    conn: &Connection,
    credential_id: &str,
    legacy_account_id: &str,
    binding_id: &str,
    credential_version: u64,
    key_cipher: &str,
    map: &GoatPlanCooldowns,
) -> anyhow::Result<bool> {
    let updated = conn.execute(
        "UPDATE credentials SET goat_plan_cooldowns_json = ?6
         WHERE id = ?1
           AND legacy_account_id = ?2
           AND binding_id = ?3
           AND credential_version = ?4
           AND key_cipher = ?5",
        params![
            credential_id,
            legacy_account_id,
            binding_id,
            credential_version as i64,
            key_cipher,
            stored_json(map)?,
        ],
    )?;
    Ok(updated == 1)
}

fn stored_json(map: &GoatPlanCooldowns) -> anyhow::Result<Option<String>> {
    if map.is_empty() {
        Ok(None)
    } else {
        Ok(Some(serde_json::to_string(map)?))
    }
}

fn parse_stored(json: Option<String>) -> anyhow::Result<Option<GoatPlanCooldowns>> {
    let Some(json) = json.filter(|value| !value.trim().is_empty()) else {
        return Ok(None);
    };
    let map: GoatPlanCooldowns = serde_json::from_str(&json).map_err(|error| {
        anyhow::anyhow!("invalid credentials.goat_plan_cooldowns_json: {error}")
    })?;
    Ok((!map.is_empty()).then_some(map))
}

fn column_ready(conn: &Connection) -> anyhow::Result<bool> {
    table_has_column(conn, "credentials", COLUMN)
}

fn serialize_instant<S>(value: &Option<DateTime<Utc>>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match value {
        Some(at) => serializer.serialize_some(&format_instant(*at)),
        None => serializer.serialize_none(),
    }
}

fn deserialize_instant<'de, D>(deserializer: D) -> Result<Option<DateTime<Utc>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<String>::deserialize(deserializer)?;
    value
        .map(|text| {
            DateTime::parse_from_rfc3339(&text)
                .map(|at| at.with_timezone(&Utc))
                .map_err(serde::de::Error::custom)
        })
        .transpose()
}

#[cfg(test)]
mod tests;
