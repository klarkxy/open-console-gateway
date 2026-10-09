use super::super::types::LegacyDestinationKindDto;
use super::*;
use std::collections::{BTreeMap, HashSet};

// Project the distinct legacy mapping facts without creating another domain model.
#[allow(clippy::too_many_arguments)]
fn row(
    s: &PageSnapshot,
    key: String,
    provider: &str,
    d: Option<&DestinationDto>,
    public: String,
    upstream: String,
    label: String,
    account: Option<&AccountSummary>,
    routable: bool,
) -> AliasPageRow {
    let metadata = d.and_then(|d| s.metadata.get(&d.id)).and_then(|entries| {
        entries
            .iter()
            .find(|m| m.public_model == public)
            .or_else(|| entries.iter().find(|m| m.upstream_model == upstream))
    });
    let destination_id = d.map(|d| d.id.clone());
    let capability = AliasCapabilitySummary {
        state: if d.is_none() {
            "unavailable"
        } else if s.errors.iter().any(|e| e.resource == "metadata") {
            "error"
        } else if metadata.is_some_and(|m| m.metadata.input_modalities.is_some()) {
            "ready"
        } else {
            "unknown"
        }
        .into(),
        destination_id: destination_id.clone(),
        source: metadata.map(|m| m.source.clone()),
        input_modalities: metadata
            .and_then(|m| m.metadata.input_modalities.clone())
            .unwrap_or_default(),
        output_modalities: metadata
            .and_then(|m| m.metadata.output_modalities.clone())
            .unwrap_or_default(),
    };
    let mut ranks = s
        .credentials
        .iter()
        .filter(|c| {
            if let Some(a) = account {
                c.legacy_account_id == a.id
            } else {
                d.map_or_else(
                    || {
                        s.accounts
                            .iter()
                            .any(|a| a.id == c.legacy_account_id && a.provider_id == provider)
                    },
                    |d| c.destination_id == d.id,
                )
            }
        })
        .filter(|c| {
            c.enabled
                && !s
                    .tags
                    .get(&c.legacy_account_id)
                    .is_some_and(|tags| tags.binding_disabled)
                && s.accounts
                    .iter()
                    .any(|a| a.id == c.legacy_account_id && a.enabled)
                && match &c.scope {
                    ocg_domain::credential::ModelScope::All => true,
                    ocg_domain::credential::ModelScope::Only { models } => {
                        models.iter().any(|m| m.eq_ignore_ascii_case(&public))
                    }
                }
        })
        .map(|c| c.routing_rank)
        .collect::<Vec<_>>();
    ranks.sort_unstable();
    ranks.dedup();
    let platform_label = account
        .and_then(|a| s.links.iter().find(|l| l.account_id == a.id))
        .and_then(|l| s.platforms.iter().find(|p| p.id == l.platform_account_id))
        .map(|p| p.name.clone());
    let target = Some(AliasPageTarget {
        account_id: account.map(|a| a.id.clone()),
        provider_id: Some(provider.into()),
        destination_id: destination_id.clone(),
        model: if d.is_some_and(|d| d.legacy.kind == LegacyDestinationKindDto::Builtin) {
            upstream.clone()
        } else {
            public.clone()
        },
        capabilities: false,
    });
    let capability_target =
        d.filter(|d| !d.capabilities.external_integration)
            .map(|d| AliasPageTarget {
                account_id: None,
                provider_id: Some(provider.into()),
                destination_id: Some(d.id.clone()),
                model: if d.legacy.kind == LegacyDestinationKindDto::Builtin {
                    upstream.clone()
                } else {
                    public.clone()
                },
                capabilities: true,
            });
    AliasPageRow {
        key,
        provider_id: provider.into(),
        destination_id,
        public_model: public,
        upstream_model: upstream,
        provider_plan: label,
        custom_account_id: account.map(|a| a.id.clone()),
        custom_account: account.map(|a| a.name.clone()),
        routable,
        routing_ranks: ranks,
        platform_label,
        capability,
        target,
        capability_target,
    }
}
fn inventory(s: &PageSnapshot) -> Vec<AliasPageRow> {
    let enabled = s
        .accounts
        .iter()
        .filter(|a| a.enabled)
        .map(|a| a.provider_id.as_str())
        .collect::<HashSet<_>>();
    let raw_models = s
        .contracts
        .providers
        .iter()
        .flat_map(|p| p.models.iter().map(|m| m.model_id.as_str()))
        .collect::<HashSet<_>>();
    // A raw spelling can also be an authorized shared Alias. Custom mappings
    // may serve that Alias even when the builtin mapping is disabled. Keep
    // rejecting raw pins whose public Alias has a different spelling.
    let shared_names = s
        .contracts
        .providers
        .iter()
        .flat_map(|p| &p.models)
        .filter(|m| !m.alias.is_empty())
        .map(|m| m.alias.to_ascii_lowercase())
        .collect::<HashSet<_>>();
    let mut rows = Vec::new();
    for p in &s.contracts.providers {
        if !enabled.contains(p.provider_id.as_str()) {
            continue;
        }
        let d = s.destinations.iter().find(|d| {
            d.legacy.kind == LegacyDestinationKindDto::Builtin && d.legacy.id == p.provider_id
        });
        for m in &p.models {
            let public = if !m.alias.is_empty() {
                m.alias.clone()
            } else if p.provider_id == crate::provider::OPENCODE_PROVIDER_ID {
                m.model_id.clone()
            } else {
                continue;
            };
            rows.push(row(
                s,
                format!("provider:{}:{}:{}", p.scope_id, public, m.model_id),
                &p.provider_id,
                d,
                public,
                m.model_id.clone(),
                d.map(|d| d.name.clone())
                    .unwrap_or_else(|| p.provider_id.clone()),
                None,
                m.routable,
            ));
        }
    }
    for a in &s.accounts {
        if a.provider_id != ocg_domain::ids::CUSTOM_PROVIDER_ID || !a.enabled {
            continue;
        }
        let credential = s.credentials.iter().find(|c| c.legacy_account_id == a.id);
        let d = credential.and_then(|c| s.destinations.iter().find(|d| d.id == c.destination_id));
        let scope = s
            .contracts
            .custom_endpoints
            .iter()
            .find(|p| p.scope_id == a.id);
        let mut seen = HashSet::new();
        for m in s.custom_capabilities.get(&a.id).into_iter().flatten() {
            if !seen.insert((m.public_model.to_lowercase(), m.upstream_model.clone())) {
                continue;
            }
            let contract = scope.and_then(|p| {
                p.models.iter().find(|c| {
                    if c.alias.is_empty() {
                        c.model_id.eq_ignore_ascii_case(&m.public_model)
                    } else {
                        c.alias.eq_ignore_ascii_case(&m.public_model)
                    }
                })
            });
            let raw_shadowed = raw_models.contains(m.public_model.as_str())
                && !shared_names.contains(&m.public_model.to_ascii_lowercase());
            let routable = !raw_shadowed
                && a.setup_step == dashboard_v3::AccountSetupStep::Ready
                && a.plan_routable
                && contract.is_some_and(|c| c.routable);
            rows.push(row(
                s,
                format!("custom:{}:{}:{}", a.id, m.public_model, m.upstream_model),
                &a.provider_id,
                d,
                m.public_model.clone(),
                m.upstream_model.clone(),
                "Custom API".into(),
                Some(a),
                routable,
            ));
        }
    }
    for p in &s.definitions {
        if !enabled.contains(p.id.as_str()) {
            continue;
        }
        let d = s
            .destinations
            .iter()
            .find(|d| d.legacy.kind == LegacyDestinationKindDto::Dynamic && d.legacy.id == p.id);
        let Some(d) = d else {
            continue;
        };
        for m in &p.models {
            let contract = d.catalog.iter().find(|c| c.public_model == m.public_model);
            rows.push(row(
                s,
                format!("dynamic:{}:{}:{}", p.id, m.public_model, m.upstream_model),
                &p.id,
                Some(d),
                m.public_model.clone(),
                m.upstream_model.clone(),
                p.name.clone(),
                None,
                d.enabled && contract.is_some_and(|c| providers::http_model(d, c).routable),
            ));
        }
    }
    if enabled.contains(crate::provider::CPA_PROVIDER_ID)
        && let Some(d) = s
            .destinations
            .iter()
            .find(|d| d.legacy.id == crate::provider::CPA_PROVIDER_ID)
    {
        for m in d.catalog.iter().filter(|m| m.enabled) {
            let public = s
                .contracts
                .providers
                .iter()
                .flat_map(|p| p.models.iter())
                .find(|c| {
                    (!c.alias.is_empty() && c.model_id == m.upstream_model)
                        || (!c.alias.is_empty() && c.alias.eq_ignore_ascii_case(&m.upstream_model))
                })
                .map(|c| c.alias.clone())
                .unwrap_or_else(|| m.upstream_model.clone());
            rows.push(row(
                s,
                format!("cpa:{}", m.upstream_model),
                crate::provider::CPA_PROVIDER_ID,
                None,
                public,
                m.upstream_model.clone(),
                "CPA".into(),
                None,
                true,
            ));
        }
    }
    // Existing management Alias inventory shows only routable rows; publication is independent.
    rows.retain(|r| r.routable);
    rows
}
pub(super) fn project(s: &PageSnapshot, q: &PageQuery) -> AliasesPage {
    let rows = inventory(s);
    let total_rows = rows.len();
    let mut upstream = HashMap::<&str, Vec<(&str, String)>>::new();
    for r in &rows {
        upstream
            .entry(&r.upstream_model)
            .or_default()
            .push((&r.provider_id, r.public_model.to_lowercase()));
    }
    let overlaps = rows
        .iter()
        .filter(|r| {
            upstream.get(r.public_model.as_str()).is_some_and(|bucket| {
                bucket.iter().any(|(provider, name)| {
                    *provider != r.provider_id && *name != r.public_model.to_lowercase()
                })
            })
        })
        .map(|r| r.key.clone())
        .collect::<HashSet<_>>();
    let mut groups = BTreeMap::<String, Vec<AliasPageRow>>::new();
    for r in rows {
        groups
            .entry(r.public_model.to_lowercase())
            .or_default()
            .push(r);
    }
    let total_groups = groups.len();
    let needle = q.search();
    let mut matched = Vec::new();
    let mut filtered_rows = 0;
    for (publication_key, mut rows) in groups {
        rows.sort_by_key(|row| row.routing_ranks.first().copied().unwrap_or(u32::MAX));
        let total = rows.len();
        let has_overlap = rows.iter().any(|r| overlaps.contains(&r.key));
        let public = rows[0].public_model.clone();
        let rows = rows
            .into_iter()
            .filter(|r| {
                needle.is_empty()
                    || [
                        r.public_model.as_str(),
                        r.upstream_model.as_str(),
                        r.provider_plan.as_str(),
                        r.provider_id.as_str(),
                        r.custom_account.as_deref().unwrap_or_default(),
                        r.platform_label.as_deref().unwrap_or_default(),
                    ]
                    .iter()
                    .any(|v| v.to_lowercase().contains(&needle))
            })
            .collect::<Vec<_>>();
        if rows.is_empty() {
            continue;
        }
        filtered_rows += rows.len();
        matched.push((publication_key, public, total, has_overlap, rows));
    }
    let filtered_groups = matched.len();
    let (offset, limit) = q.bounds();
    let mut cursor = 0;
    let mut groups = Vec::new();
    for (key, public, total, has_overlap, rows) in matched {
        let matching = rows.len();
        let start = offset.saturating_sub(cursor).min(matching);
        let end = (offset + limit).saturating_sub(cursor).min(matching);
        if end > start {
            groups.push(AliasPageGroup {
                public_model: public,
                publication_key: key.clone(),
                published: !s.unpublished.iter().any(|p| p.trim().to_lowercase() == key),
                total_rows: count(total),
                matching_rows: count(matching),
                has_overlap,
                continued: start > 0 || end < matching,
                rows: rows.into_iter().skip(start).take(end - start).collect(),
            });
        }
        cursor += matching;
    }
    AliasesPage {
        revision: s.revision.clone(),
        read_version: s.read_version.clone(),
        as_of: s.as_of.to_rfc3339(),
        valid_until: Some(s.valid_until.to_rfc3339()),
        total_groups: count(total_groups),
        total_rows: count(total_rows),
        filtered_groups: count(filtered_groups),
        filtered_rows: count(filtered_rows),
        groups,
        offset: count(offset),
        limit: count(limit),
        has_more: offset + limit < filtered_rows,
        errors: s.errors.clone(),
    }
}
