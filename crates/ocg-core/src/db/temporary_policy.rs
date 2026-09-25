//! Node-local policy configuration in the existing settings table. Reads are
//! pure; CAS and the transaction belong to the dashboard caller. Each changed
//! rule gets a fresh monotonic version, including remove/re-add cycles.
use crate::temporary_policy::{TemporaryRule, builtin_rule, validate_rules};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub(crate) const SETTING_KEY: &str = "temporary_admission_rules_v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct VersionedRule {
    pub rule: TemporaryRule,
    pub revision: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SavedRules {
    version: u32,
    next_revision: u64,
    pub rules: Vec<VersionedRule>,
}

impl Default for SavedRules {
    fn default() -> Self {
        Self {
            version: 1,
            next_revision: 0,
            rules: vec![VersionedRule {
                rule: builtin_rule(),
                revision: 0,
            }],
        }
    }
}

impl SavedRules {
    pub(crate) fn effective(&self, destination: &str) -> Vec<VersionedRule> {
        let mut rules = BTreeMap::new();
        for saved in &self.rules {
            if saved.rule.destination_id.is_none() {
                rules.insert(saved.rule.id.as_str(), saved);
            }
        }
        for saved in &self.rules {
            if saved.rule.destination_id.as_deref() == Some(destination) {
                rules.insert(saved.rule.id.as_str(), saved);
            }
        }
        rules
            .into_values()
            .filter(|saved| saved.rule.enabled)
            .cloned()
            .collect()
    }

    pub(crate) fn contains_effective(&self, destination: &str, rule: &VersionedRule) -> bool {
        self.effective(destination)
            .iter()
            .any(|current| current.revision == rule.revision && current.rule == rule.rule)
    }
}

pub(crate) fn load_on(conn: &Connection) -> Result<SavedRules> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key=?1",
            [SETTING_KEY],
            |row| row.get(0),
        )
        .optional()?;
    let Some(value) = value else {
        return Ok(SavedRules::default());
    };
    let saved: SavedRules =
        serde_json::from_str(&value).context("invalid saved temporary admission rules")?;
    ensure!(
        saved.version == 1,
        "unsupported temporary admission rules version"
    );
    validate_rules(
        &saved
            .rules
            .iter()
            .map(|item| item.rule.clone())
            .collect::<Vec<_>>(),
    )?;
    ensure!(
        saved
            .rules
            .iter()
            .all(|rule| rule.revision <= saved.next_revision),
        "invalid temporary rule revision"
    );
    Ok(saved)
}

pub(crate) fn save_on(conn: &Connection, rules: &[TemporaryRule]) -> Result<SavedRules> {
    validate_rules(rules)?;
    for rule in rules {
        if let Some(id) = &rule.destination_id {
            ensure!(
                conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM destinations WHERE id=?1)",
                    [id],
                    |row| row.get::<_, bool>(0)
                )?,
                "rule destination no longer exists"
            );
        }
    }
    let previous = load_on(conn)?;
    let mut next = previous.next_revision;
    let mut saved = Vec::with_capacity(rules.len());
    for rule in rules {
        let revision = if let Some(old) = previous.rules.iter().find(|old| old.rule == *rule) {
            old.revision
        } else {
            next = next
                .checked_add(1)
                .context("temporary rule revision exhausted")?;
            next
        };
        saved.push(VersionedRule {
            rule: rule.clone(),
            revision,
        });
    }
    let saved = SavedRules {
        version: 1,
        next_revision: next,
        rules: saved,
    };
    conn.execute("INSERT INTO settings (key,value) VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![SETTING_KEY, serde_json::to_string(&saved)?])?;
    Ok(saved)
}

#[cfg(test)]
mod tests;
