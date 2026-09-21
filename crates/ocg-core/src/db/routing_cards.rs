//! Stable presentation groups over the canonical credential routing sequence.
//! Mutators here use the caller's transaction; reads never change routing ranks.

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

pub(crate) const SETTING_KEY: &str = "routing_cards_v1";
const MAX_CARDS: usize = 1000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct RoutingCard {
    pub id: String,
    pub destination_id: String,
    pub credential_ids: Vec<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedLayout {
    version: u32,
    cards: Vec<RoutingCard>,
}

struct CredentialRow {
    id: String,
    destination_id: String,
}

fn resources(conn: &Connection) -> Result<(Vec<String>, Vec<CredentialRow>)> {
    let destinations = conn
        .prepare("SELECT id FROM destinations ORDER BY id")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let credentials = conn
        .prepare(
            "SELECT id, destination_id FROM credentials
         WHERE COALESCE(credential_purpose, 'inference') = 'inference'
         ORDER BY routing_rank, created_at, legacy_account_id, id",
        )?
        .query_map([], |row| {
            Ok(CredentialRow {
                id: row.get(0)?,
                destination_id: row.get(1)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((destinations, credentials))
}

fn read_saved(conn: &Connection) -> Result<Vec<RoutingCard>> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            [SETTING_KEY],
            |row| row.get(0),
        )
        .optional()?;
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let saved: SavedLayout =
        serde_json::from_str(&value).context("invalid saved routing card layout")?;
    ensure!(
        saved.version == 1,
        "unsupported saved routing card layout version"
    );
    validate_structure(&saved.cards)?;
    Ok(saved.cards)
}

fn validate_structure(cards: &[RoutingCard]) -> Result<()> {
    ensure!(
        cards.len() <= MAX_CARDS,
        "at most {MAX_CARDS} routing cards are supported"
    );
    let mut ids = HashSet::new();
    let mut credentials = HashSet::new();
    for card in cards {
        ensure!(
            !card.id.trim().is_empty() && card.id.len() <= 128,
            "invalid routing card id"
        );
        ensure!(ids.insert(&card.id), "duplicate routing card id");
        ensure!(
            !card.destination_id.is_empty(),
            "routing card destination is required"
        );
        for id in &card.credential_ids {
            ensure!(
                credentials.insert(id),
                "a credential appears in more than one routing card"
            );
        }
    }
    Ok(())
}

fn new_id(seed: &str, reserved: &mut HashSet<String>) -> String {
    for suffix in 0_u64.. {
        let id = uuid::Uuid::new_v5(
            &uuid::Uuid::NAMESPACE_URL,
            format!("ocg:routing-card:{seed}:{suffix}").as_bytes(),
        )
        .to_string();
        if reserved.insert(id.clone()) {
            return id;
        }
    }
    unreachable!()
}

/// Reconcile only presentation membership against the live ranked sequence.
/// External reorder can split an old card; its first segment retains its id.
fn normalize(
    destinations: &[String],
    credentials: &[CredentialRow],
    saved: &[RoutingCard],
) -> Result<Vec<RoutingCard>> {
    let destination_ids: HashSet<_> = destinations.iter().map(String::as_str).collect();
    let by_credential: HashMap<_, _> = credentials
        .iter()
        .map(|row| (row.id.as_str(), row))
        .collect();
    let saved: Vec<_> = saved
        .iter()
        .filter(|card| destination_ids.contains(card.destination_id.as_str()))
        .collect();
    let mut owners = HashMap::<&str, &str>::new();
    for card in &saved {
        for id in &card.credential_ids {
            if by_credential
                .get(id.as_str())
                .is_some_and(|row| row.destination_id == card.destination_id)
            {
                owners.insert(id, &card.id);
            }
        }
    }
    let mut reserved: HashSet<_> = saved.iter().map(|card| card.id.clone()).collect();
    let mut used = HashSet::<String>::new();
    let mut result = Vec::<RoutingCard>::new();
    let mut last_owner = None::<String>;
    // A new first Key can reuse a previously empty card for that destination.
    let empty: Vec<_> = saved
        .iter()
        .filter(|card| {
            !card
                .credential_ids
                .iter()
                .any(|id| owners.get(id.as_str()) == Some(&card.id.as_str()))
        })
        .collect();
    for (index, credential) in credentials.iter().enumerate() {
        ensure!(
            destination_ids.contains(credential.destination_id.as_str()),
            "credential destination is missing"
        );
        let owner = if let Some(owner) = owners.get(credential.id.as_str()) {
            (*owner).to_string()
        } else if result
            .last()
            .is_some_and(|card| card.destination_id == credential.destination_id)
        {
            last_owner.clone().expect("a populated result has an owner")
        } else if let Some(owner) = credentials[index + 1..]
            .iter()
            .take_while(|row| row.destination_id == credential.destination_id)
            .find_map(|row| owners.get(row.id.as_str()))
        {
            (*owner).to_string()
        } else if let Some(card) = empty.iter().find(|card| {
            card.destination_id == credential.destination_id && !used.contains(&card.id)
        }) {
            card.id.clone()
        } else {
            new_id(&format!("credential:{}", credential.id), &mut reserved)
        };
        if last_owner.as_ref() == Some(&owner)
            && result
                .last()
                .is_some_and(|card| card.destination_id == credential.destination_id)
        {
            result
                .last_mut()
                .unwrap()
                .credential_ids
                .push(credential.id.clone());
            continue;
        }
        let id = if used.insert(owner.clone()) {
            owner.clone()
        } else {
            new_id(&format!("split:{owner}:{}", credential.id), &mut reserved)
        };
        result.push(RoutingCard {
            id,
            destination_id: credential.destination_id.clone(),
            credential_ids: vec![credential.id.clone()],
        });
        last_owner = Some(owner);
    }
    // Empty cards follow their nearest surviving saved predecessor. A leading
    // empty card stays at the beginning; multiple empties keep their order.
    let mut anchor = None::<String>;
    for card in saved {
        if let Some(existing) = result.iter().find(|row| row.id == card.id) {
            anchor = Some(existing.id.clone());
            continue;
        }
        if card
            .credential_ids
            .iter()
            .any(|id| owners.get(id.as_str()) == Some(&card.id.as_str()))
        {
            continue;
        }
        let position = anchor
            .as_ref()
            .and_then(|id| result.iter().position(|row| &row.id == id))
            .map_or(0, |index| index + 1);
        result.insert(
            position,
            RoutingCard {
                id: card.id.clone(),
                destination_id: card.destination_id.clone(),
                credential_ids: Vec::new(),
            },
        );
        anchor = Some(card.id.clone());
    }
    for destination in destinations {
        if !result
            .iter()
            .any(|card| &card.destination_id == destination)
        {
            result.push(RoutingCard {
                id: new_id(&format!("empty:{destination}"), &mut reserved),
                destination_id: destination.clone(),
                credential_ids: Vec::new(),
            });
        }
    }
    Ok(result)
}

pub(crate) fn load_on(conn: &Connection) -> Result<Vec<RoutingCard>> {
    let (destinations, credentials) = resources(conn)?;
    normalize(&destinations, &credentials, &read_saved(conn)?)
}

fn persist_on(conn: &Connection, cards: &[RoutingCard]) -> Result<()> {
    let value = serde_json::to_string(&SavedLayout {
        version: 1,
        cards: cards.to_vec(),
    })?;
    conn.execute("INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value", params![SETTING_KEY, value])?;
    Ok(())
}

fn sequence_for<'a>(
    cards: &'a [RoutingCard],
    destinations: &[String],
    credentials: &[CredentialRow],
) -> Result<Vec<&'a str>> {
    validate_structure(cards)?;
    let destinations: HashSet<_> = destinations.iter().map(String::as_str).collect();
    let credentials: HashMap<_, _> = credentials
        .iter()
        .map(|row| (row.id.as_str(), row.destination_id.as_str()))
        .collect();
    let mut sequence = Vec::new();
    for card in cards {
        ensure!(
            destinations.contains(card.destination_id.as_str()),
            "routing card destination does not exist"
        );
        for id in &card.credential_ids {
            ensure!(
                credentials.get(id.as_str()) == Some(&card.destination_id.as_str()),
                "routing card credential must belong to its destination"
            );
            sequence.push(id.as_str());
        }
    }
    ensure!(
        sequence.len() == credentials.len(),
        "routing cards must cover every inference credential exactly once"
    );
    Ok(sequence)
}

/// Caller owns the transaction covering this layout and its flattened ranks.
pub(crate) fn save_on(conn: &Connection, cards: &[RoutingCard]) -> Result<()> {
    let (destinations, credentials) = resources(conn)?;
    let sequence = sequence_for(cards, &destinations, &credentials)?;
    for (rank, id) in sequence.iter().enumerate() {
        conn.execute(
            "UPDATE credentials SET routing_rank = ?1 WHERE id = ?2",
            params![rank as i64, id],
        )?;
    }
    persist_on(conn, cards)
}

/// Restore membership after the import transaction has resolved the merged
/// credential sequence. Target order stays authoritative, including rows that
/// are absent from the package. Discontiguous imported cards split as needed.
pub(crate) fn restore_on(conn: &Connection, cards: &[RoutingCard]) -> Result<()> {
    let (destinations, credentials) = resources(conn)?;
    sequence_for(cards, &destinations, &credentials)?;
    let normalized = normalize(&destinations, &credentials, cards)?;
    validate_structure(&normalized)?;
    persist_on(conn, &normalized)
}

/// Used after ordinary creation/deletion/reorder; never changes execution order.
pub(crate) fn reconcile_on(conn: &Connection) -> Result<()> {
    let cards = load_on(conn)?;
    validate_structure(&cards)?;
    persist_on(conn, &cards)
}

#[cfg(test)]
mod tests;
