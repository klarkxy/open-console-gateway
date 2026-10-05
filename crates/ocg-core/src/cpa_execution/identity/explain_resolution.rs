//! Non-secret name resolution against current destination catalogs.
//!
//! The read is the caller's transaction. Routeability is not decided here.
//! This module does not admit, decrypt, or read the retired global CPA catalog.

use super::curated_alias;
use crate::cpa_policy::PolicyFault;
use crate::db::native_binding::OWNED_NATIVE_LEGACY_ID;
use ocg_domain::destination::CatalogModel;
use ocg_domain::ids::{looks_raw_shaped, model_ids_match};
use rusqlite::Transaction;
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::cpa_execution) enum ResolutionClass {
    Alias,
    PinnedRaw,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::cpa_execution) struct ResolutionMapping {
    pub destination_id: String,
    pub provider_id: String,
    pub upstream_model: String,
    pub adapter_kind: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::cpa_execution) struct DestinationFace {
    pub id: String,
    pub adapter_kind: String,
    pub base_url: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::cpa_execution) struct CatalogResolution {
    pub known: bool,
    pub kind: Option<ResolutionClass>,
    pub alias: Option<String>,
    pub ambiguous: bool,
    pub mappings: Vec<ResolutionMapping>,
    pub destinations: Vec<DestinationFace>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum HitClass {
    Alias { spelling: String, curated: bool },
    PinnedRaw,
}

struct DestinationRow {
    id: String,
    legacy_kind: String,
    legacy_id: String,
    adapter_kind: String,
    base_url: String,
}

pub(in crate::cpa_execution) fn resolve_current_catalog(
    tx: &Transaction<'_>,
    requested: &str,
) -> Result<CatalogResolution, PolicyFault> {
    let rows = destination_rows(tx)?;
    let mut destinations = Vec::with_capacity(rows.len());
    let mut hits: Vec<(HitClass, ResolutionMapping)> = Vec::new();
    for row in &rows {
        destinations.push(DestinationFace {
            id: row.id.clone(),
            adapter_kind: row.adapter_kind.clone(),
            base_url: row.base_url.clone(),
        });
        let catalog = crate::db::destination_store::load_destination_catalog(tx, &row.id)
            .map_err(|_| PolicyFault::Unavailable)?;
        let providers = provider_ids(tx, row)?;
        for model in &catalog {
            for provider_id in &providers {
                let Some(class) = classify_hit(provider_id, model, requested) else {
                    continue;
                };
                hits.push((
                    class,
                    ResolutionMapping {
                        destination_id: row.id.clone(),
                        provider_id: provider_id.clone(),
                        upstream_model: model.upstream_model.clone(),
                        adapter_kind: row.adapter_kind.clone(),
                    },
                ));
            }
        }
    }
    let hits = select_hits(requested, hits);
    let (kind, alias, ambiguous) = collapse(&hits);
    let mappings: Vec<ResolutionMapping> = hits.into_iter().map(|(_, mapping)| mapping).collect();
    Ok(CatalogResolution {
        known: !mappings.is_empty(),
        kind,
        alias,
        ambiguous,
        mappings,
        destinations,
    })
}

fn destination_rows(tx: &Transaction<'_>) -> Result<Vec<DestinationRow>, PolicyFault> {
    let mut statement = tx
        .prepare(
            "SELECT id, legacy_kind, legacy_id, adapter, COALESCE(base_url, '')
             FROM destinations
             ORDER BY id",
        )
        .map_err(|_| PolicyFault::Unavailable)?;
    let rows = statement
        .query_map([], |row| {
            Ok(DestinationRow {
                id: row.get(0)?,
                legacy_kind: row.get(1)?,
                legacy_id: row.get(2)?,
                adapter_kind: row.get(3)?,
                base_url: row.get(4)?,
            })
        })
        .map_err(|_| PolicyFault::Unavailable)?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|_| PolicyFault::Unavailable)
}

fn provider_ids(tx: &Transaction<'_>, row: &DestinationRow) -> Result<Vec<String>, PolicyFault> {
    let mut ids = Vec::new();
    // The owned-native destination marker is not a provider. Native credentials
    // carry the canonical `cpa` provider id.
    if row.legacy_id != OWNED_NATIVE_LEGACY_ID
        && matches!(row.legacy_kind.as_str(), "builtin" | "dynamic")
        && !row.legacy_id.is_empty()
    {
        ids.push(row.legacy_id.clone());
    }
    let mut statement = tx
        .prepare(
            "SELECT COALESCE(provider_id, '')
             FROM credentials
             WHERE destination_id = ?1
             ORDER BY id",
        )
        .map_err(|_| PolicyFault::Unavailable)?;
    let found = statement
        .query_map([&row.id], |row| row.get::<_, String>(0))
        .map_err(|_| PolicyFault::Unavailable)?;
    for provider in found {
        let provider = provider.map_err(|_| PolicyFault::Unavailable)?;
        let provider = provider.trim();
        if !provider.is_empty() && !ids.iter().any(|saved| saved == provider) {
            ids.push(provider.to_string());
        }
    }
    if ids.is_empty() {
        ids.push(String::new());
    }
    Ok(ids)
}

/// Exact raw spelling and curated-alias case rules.
///
/// A raw-shaped request matches only the stored upstream bytes. Case folding
/// belongs to a curated alias, not to a raw pin.
fn classify_hit(provider_id: &str, model: &CatalogModel, requested: &str) -> Option<HitClass> {
    let trimmed = requested.trim();
    if trimmed.is_empty() {
        return None;
    }
    let alias = curated_alias(provider_id, &model.upstream_model);
    let exact_upstream = model.upstream_model == trimmed;
    if looks_raw_shaped(trimmed) {
        return exact_upstream.then_some(HitClass::PinnedRaw);
    }
    if !alias.is_empty() && alias == trimmed {
        return Some(HitClass::Alias {
            spelling: alias,
            curated: true,
        });
    }
    if !alias.is_empty() && model_ids_match(trimmed, &alias) && !exact_upstream {
        return Some(HitClass::Alias {
            spelling: alias,
            curated: true,
        });
    }
    if exact_upstream {
        return Some(HitClass::PinnedRaw);
    }
    if !looks_raw_shaped(&model.public_model)
        && model.public_model != model.upstream_model
        && model_ids_match(trimmed, &model.public_model)
    {
        return Some(HitClass::Alias {
            spelling: model.public_model.clone(),
            curated: false,
        });
    }
    None
}

/// Exact raw spelling wins over a different alias before identities collapse.
/// The request that is itself the canonical alias stays on that alias.
fn select_hits(
    requested: &str,
    hits: Vec<(HitClass, ResolutionMapping)>,
) -> Vec<(HitClass, ResolutionMapping)> {
    let trimmed = requested.trim();
    if looks_raw_shaped(trimmed) {
        return hits;
    }
    let curated_request = hits.iter().any(|(class, _)| {
        matches!(
            class,
            HitClass::Alias {
                curated: true,
                spelling,
            } if spelling == trimmed
        )
    });
    if curated_request {
        return hits
            .into_iter()
            .filter(|(class, _)| {
                matches!(class, HitClass::Alias { spelling, .. } if spelling == trimmed)
            })
            .collect();
    }
    let exact_raw = hits.iter().any(|(class, mapping)| {
        matches!(class, HitClass::PinnedRaw) && mapping.upstream_model == trimmed
    });
    if exact_raw {
        return hits
            .into_iter()
            .filter(|(class, mapping)| {
                matches!(class, HitClass::PinnedRaw) && mapping.upstream_model == trimmed
            })
            .collect();
    }
    hits
}

fn collapse(
    hits: &[(HitClass, ResolutionMapping)],
) -> (Option<ResolutionClass>, Option<String>, bool) {
    if hits.is_empty() {
        return (None, None, false);
    }
    let mut alias = None;
    let mut saw_alias = false;
    let mut saw_pin = false;
    let mut pins = BTreeSet::new();
    for (class, mapping) in hits {
        match class {
            HitClass::Alias { spelling, .. } => {
                saw_alias = true;
                match &alias {
                    None => alias = Some(spelling.clone()),
                    Some(saved) if saved == spelling => {}
                    Some(_) => return (None, None, true),
                }
            }
            HitClass::PinnedRaw => {
                saw_pin = true;
                pins.insert((mapping.provider_id.clone(), mapping.upstream_model.clone()));
            }
        }
    }
    if saw_alias && saw_pin {
        return (None, None, true);
    }
    if saw_alias {
        return (Some(ResolutionClass::Alias), alias, false);
    }
    if pins.len() > 1 {
        return (None, None, true);
    }
    (Some(ResolutionClass::PinnedRaw), None, false)
}

#[cfg(test)]
mod tests;
