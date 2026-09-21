//! Destination/credential projection for leftover-table migration and V4–V6
//! backup conversion.
//!
//! Runtime reads serve persisted `destinations` / `credentials` via
//! [`read_v4_projection`] and [`load_persisted`]. Runtime writers persist
//! those tables incrementally; they must not rebuild them from [`project`].
//!
//! [`project`] still maps reconstructed account/platform/dynamic/identity
//! facts when leftover tables exist or a V4–V6 package needs converting.
//! [`replace_persisted`] / [`replace_persisted_on`] remain the one-shot
//! backfill used while `accounts` (or other leftover tables) can still
//! reconstruct the store. [`refresh_destination_shadow`] now only syncs
//! builtin `destination_models` from persisted contracts.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use ocg_domain::destination::{
    AdapterKind, CatalogModel, Credential, Destination, LegacyCredentialFacts,
    LegacyDestinationFacts, LegacyDestinationRef, LegacyIdentityFacts, LegacyPlatformLink,
    MappingError, PlatformKind, credential_from_legacy, destination_from_legacy,
    destination_id_for_builtin, destination_id_for_platform_account,
};
use ocg_domain::ids::{
    CPA_ACCOUNT_ID, CPA_PROVIDER_ID, CUSTOM_PROVIDER_ID, OPENCODE_ZEN_FREE_PROVIDER_ID,
};

use crate::db::Database;
use crate::db::identity::IdentityAccountRecord;
use crate::models::{Account, AccountModelCapability};
use crate::platform::{PlatformAccount, PlatformKind as StoredPlatformKind};
use crate::provider::{BUILTIN_PROVIDERS, ConnectionVerificationStatus, UpstreamProtocolKind};
use crate::provider_contracts::{EffectiveScopeContract, build_effective_contracts};

mod store;

/// Total mapped destination + credential set. Catalogs are joined after mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DestinationProjection {
    pub destinations: Vec<Destination>,
    pub credentials: Vec<Credential>,
}

/// One legacy row the mapper refused. The projection is total or it refuses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionRefusal {
    pub row: RefusedRow,
    pub error: MappingError,
}

/// Persisted row identity for a mapping refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefusedRow {
    Account { id: String, provider_id: String },
    DynamicProvider { id: String },
    PlatformParent { id: String },
}

/// Total or refuse: `Ok(Ok(_))` only when every row mapped.
///
/// Reads the same SQLite handles V4 connections/identities use (`list_accounts`,
/// `list_identity_model`, `list_control_plane_dynamic_providers` reconstructed
/// from destinations + `destination_models`, platform parents/links, persisted
/// provider contracts). Callers that already hold `CoreState` pass
/// `&*state.db.lock()`.
pub fn project(
    db: &Database,
) -> anyhow::Result<Result<DestinationProjection, Vec<ProjectionRefusal>>> {
    let accounts = db.list_accounts()?;
    let identity_snapshot = db.list_identity_model()?;
    let identity_by_account: HashMap<&str, &IdentityAccountRecord> = identity_snapshot
        .accounts
        .iter()
        .map(|record| (record.account.id.as_str(), record))
        .collect();
    let platform_links = db.list_platform_links()?;
    let link_by_account: HashMap<&str, String> = platform_links
        .iter()
        .map(|link| (link.account_id.as_str(), link.platform_account_id.clone()))
        .collect();
    let platforms = db.list_platform_accounts()?;
    let dynamic_providers = db.list_control_plane_dynamic_providers()?;

    let mut destinations = Vec::new();
    let mut refusals = Vec::new();

    let cpa_destination_present = crate::db::cpa::destination_present(&db.conn)?;
    for facts in builtin_destination_facts(&accounts, cpa_destination_present) {
        push_destination(&mut destinations, &mut refusals, facts);
    }
    for runtime in &dynamic_providers {
        push_destination(
            &mut destinations,
            &mut refusals,
            PendingDestination {
                row: RefusedRow::DynamicProvider {
                    id: runtime.id.clone(),
                },
                facts: LegacyDestinationFacts::Dynamic {
                    definition: runtime.definition(),
                },
            },
        );
    }
    for account in accounts
        .iter()
        .filter(|account| account.provider_id == CUSTOM_PROVIDER_ID)
        .filter(|account| !link_by_account.contains_key(account.id.as_str()))
    {
        let custom = custom_account_facts(db, &account.id)?;
        push_destination(
            &mut destinations,
            &mut refusals,
            PendingDestination {
                row: RefusedRow::Account {
                    id: account.id.clone(),
                    provider_id: account.provider_id.clone(),
                },
                facts: LegacyDestinationFacts::CustomAccount {
                    account_id: account.id.clone(),
                    name: account.name.clone(),
                    endpoint_url: custom.endpoint_url,
                    protocol: custom.protocol,
                    model_capabilities: custom.model_capabilities,
                },
            },
        );
    }
    for parent in &platforms {
        push_destination(
            &mut destinations,
            &mut refusals,
            PendingDestination {
                row: RefusedRow::PlatformParent {
                    id: parent.id.clone(),
                },
                facts: platform_parent_facts(parent),
            },
        );
    }
    crate::db::cpa::overlay_projected_destination(&db.conn, &mut destinations)?;

    let mut credentials = Vec::new();
    for (order_index, account) in accounts.iter().enumerate() {
        let facts = credential_facts(
            account,
            order_index as u32,
            identity_by_account.get(account.id.as_str()).copied(),
            link_by_account.get(account.id.as_str()).cloned(),
        );
        push_credential(&mut credentials, &mut refusals, &destinations, facts);
    }
    if !refusals.is_empty() {
        return Ok(Err(refusals));
    }

    join_persisted_catalogs(db, &mut destinations, &accounts, &link_by_account);
    Ok(Ok(DestinationProjection {
        destinations,
        credentials,
    }))
}

/// Rebuild the v50 shadow from [`project`] in one SQLite transaction.
///
/// After v52 the open path must not empty a populated destinations/credentials
/// store just because `accounts` is gone. After v54 the same is true for
/// platform leftover tables: `project()` reconstructs parents/links from
/// destinations + credentials. After v55 the same is true for CPA leftover:
/// `project()` reconstructs the integration from the CPA destination +
/// observer credential. After v56 the same is true for leftover dynamic
/// provider tables: `project()` reconstructs user-defined Providers from
/// destinations + `destination_models`. After v57 leftover identity satellites
/// are gone: identities reconstruct from credentials (and destinations for
/// platform parents). Skip when `accounts` is missing or
/// the shadow already has rows; only seed when the shadow is empty and leftover
/// legacy tables can still project. Mapping refusals return `Ok(Err(refusals))`
/// without deleting credential-backed account rows after v52. I/O or SQL errors
/// fail the outer `Result`.
pub fn replace_persisted(db: &Database) -> anyhow::Result<Result<(), Vec<ProjectionRefusal>>> {
    let tx = db.conn.unchecked_transaction()?;
    let result = replace_persisted_on(db)?;
    tx.commit()?;
    Ok(result)
}

/// After v52 the Host open path must not empty a populated destinations/credentials
/// store just because `accounts` is gone. Skip when that table is missing or the
/// shadow already has rows; only seed when the shadow is empty and leftover
/// legacy tables can still project.
pub(crate) fn should_skip_persist_on_open(db: &Database) -> anyhow::Result<bool> {
    let accounts_missing = !crate::db::table_exists(&db.conn, "accounts")?;
    let populated = store::load_all(db)
        .ok()
        .is_some_and(|stored| shadow_is_populated(&stored));
    Ok(accounts_missing || populated)
}

/// One-shot leftover-table backfill. Runtime writers must not call this.
pub fn replace_persisted_on(db: &Database) -> anyhow::Result<Result<(), Vec<ProjectionRefusal>>> {
    match project(db)? {
        Err(refusals) => {
            // After v52 credentials *are* the account-row store. Emptying them
            // on a mapping refusal would delete live Keys. Only wipe the
            // shadow while `accounts` can still reconstruct it.
            if crate::db::table_exists(&db.conn, "accounts")? {
                store::empty_all_on(&db.conn)?;
            }
            Ok(Err(refusals))
        }
        Ok(projection) => {
            store::replace_all_on(&db.conn, &projection)?;
            Ok(Ok(()))
        }
    }
}

/// Keep builtin `destination_models` aligned with persisted contracts.
///
/// Runtime writers must not rebuild destinations/credentials from
/// [`project`]. Leftover-table backfill still uses [`replace_persisted_on`].
pub fn refresh_destination_shadow(db: &Database) -> anyhow::Result<()> {
    if db.schema_version()? >= 59 {
        crate::db::destination_store::seed_missing_builtin_catalogs(db)
    } else {
        crate::db::destination_store::sync_builtin_catalogs(db)
    }
}

/// Reconstruct destinations and credentials from the v50 shadow tables.
pub fn load_persisted(db: &Database) -> anyhow::Result<DestinationProjection> {
    store::load_all(db)
}

/// Field equality between a live [`project`] snapshot and a v50 shadow.
pub fn shadow_compare(live: &DestinationProjection, stored: &DestinationProjection) -> bool {
    live == stored
}

/// Read the v50 shadow and return it only when it equals `live`.
///
/// Load errors, an empty/divergent snapshot, and any other mismatch yield
/// `None` so callers can serve `live` without failing the request.
pub fn read_shadow_if_matches(
    db: &Database,
    live: &DestinationProjection,
) -> Option<DestinationProjection> {
    let stored = load_persisted(db).ok()?;
    shadow_compare(live, &stored).then_some(stored)
}

/// True when the shadow holds at least one destination or credential.
///
/// Fresh and refusal-emptied stores are both empty; a total persist always
/// writes Zen (and usually more), so emptiness is the fallback signal.
pub fn shadow_is_populated(stored: &DestinationProjection) -> bool {
    !stored.destinations.is_empty() || !stored.credentials.is_empty()
}

/// V4 reads the authoritative store, including a legitimately empty store.
/// Legacy reconstruction belongs exclusively to upgrade and import boundaries.
pub fn read_v4_projection(
    db: &Database,
) -> anyhow::Result<Result<DestinationProjection, Vec<ProjectionRefusal>>> {
    let stored = load_persisted(db)?;
    validate_persisted_references(&stored)?;
    let refusals = persisted_destination_refusals(&stored);
    if refusals.is_empty() {
        Ok(Ok(stored))
    } else {
        Ok(Err(refusals))
    }
}

/// The validated configuration used by routing and configuration consumers.
/// An I/O error or invalid relationship is never an invitation to reconstruct
/// a different configuration from compatibility records.
pub fn load_runtime(db: &Database) -> anyhow::Result<DestinationProjection> {
    read_v4_projection(db)?
        .map_err(|refusals| anyhow::anyhow!("invalid destination configuration: {refusals:?}"))
}

fn validate_persisted_references(stored: &DestinationProjection) -> anyhow::Result<()> {
    let destinations: HashMap<_, _> = stored
        .destinations
        .iter()
        .map(|destination| (destination.id.as_str(), destination))
        .collect();
    for credential in &stored.credentials {
        anyhow::ensure!(
            destinations.contains_key(credential.destination_id.as_str()),
            "credential `{}` references missing destination `{}`",
            credential.id,
            credential.destination_id
        );
    }
    for destination in &stored.destinations {
        if let Some(limit) = destination.max_credentials {
            let count = stored
                .credentials
                .iter()
                .filter(|credential| credential.destination_id == destination.id)
                .count();
            anyhow::ensure!(
                count <= limit as usize,
                "destination `{}` exceeds its credential limit",
                destination.id
            );
        }
        for model in &destination.catalog {
            anyhow::ensure!(
                !model.public_model.trim().is_empty() && !model.upstream_model.trim().is_empty(),
                "destination `{}` contains an empty model identity",
                destination.id
            );
        }
    }
    Ok(())
}

/// Validate structural invariants that the legacy mapper used to enforce.
/// Persisted rows are authoritative after v57, but corrupted endpoint-bearing
/// destinations must still fail closed instead of reaching the dashboard or
/// routing runtime as apparently usable records.
fn persisted_destination_refusals(stored: &DestinationProjection) -> Vec<ProjectionRefusal> {
    stored
        .destinations
        .iter()
        .filter_map(|destination| {
            let missing_base = destination
                .base_url
                .as_deref()
                .is_none_or(|value| value.trim().is_empty());
            if !missing_base {
                return None;
            }
            match &destination.legacy {
                LegacyDestinationRef::CustomAccount(id) => Some(ProjectionRefusal {
                    row: RefusedRow::Account {
                        id: id.clone(),
                        provider_id: CUSTOM_PROVIDER_ID.to_string(),
                    },
                    error: MappingError::CustomAccountMissingEndpoint {
                        account_id: id.clone(),
                    },
                }),
                LegacyDestinationRef::PlatformParent(id) => Some(ProjectionRefusal {
                    row: RefusedRow::PlatformParent { id: id.clone() },
                    error: MappingError::PlatformMissingBaseUrl { id: id.clone() },
                }),
                _ => None,
            }
        })
        .collect()
}

/// V3 `GET /accounts` rows: shadow credential order, then live leftovers.
///
/// The frozen V3 list still decrypts and maps each legacy `accounts` row.
/// A populated shadow only chooses identity and order. Accounts the shadow
/// does not name stay on the list, appended in live `list_accounts` order.
/// An empty shadow or load error is live order.
pub fn list_accounts_for_v3(db: &Database) -> anyhow::Result<Vec<Account>> {
    let live = db.list_accounts()?;
    Ok(match load_persisted(db) {
        Ok(stored) if shadow_is_populated(&stored) => order_rows_by_ids(
            live,
            stored
                .credentials
                .iter()
                .map(|credential| credential.legacy_account_id.as_str()),
            |account| account.id.as_str(),
        ),
        _ => live,
    })
}

/// V3 `GET /platform-accounts` parents: shadow destination order, then live leftovers.
pub fn list_platform_accounts_for_v3(db: &Database) -> anyhow::Result<Vec<PlatformAccount>> {
    let live = db.list_platform_accounts()?;
    Ok(match load_persisted(db) {
        Ok(stored) if shadow_is_populated(&stored) => order_rows_by_ids(
            live,
            stored
                .destinations
                .iter()
                .filter_map(|destination| match &destination.legacy {
                    LegacyDestinationRef::PlatformParent(id) => Some(id.as_str()),
                    _ => None,
                }),
            |account| account.id.as_str(),
        ),
        _ => live,
    })
}

/// Compatibility signature for callers migrating to [`load_runtime`].
/// Success always contains the persisted configuration, even when it is empty.
pub fn routing_projection(db: &Database) -> anyhow::Result<Option<DestinationProjection>> {
    load_runtime(db).map(Some)
}

/// True when a Zen destination credential is still inside `free_until`.
///
/// The planner uses destination `adapter` and credential cooldowns, not the
/// reserved Zen account id.
pub fn free_channel_exhausted(projection: &DestinationProjection, now: DateTime<Utc>) -> bool {
    let zen: HashSet<&str> = projection
        .destinations
        .iter()
        .filter(|destination| destination.adapter == AdapterKind::Zen)
        .map(|destination| destination.id.as_str())
        .collect();
    projection.credentials.iter().any(|credential| {
        zen.contains(credential.destination_id.as_str())
            && credential
                .cooldowns
                .free_until
                .is_some_and(|until| until > now)
    })
}

fn order_rows_by_ids<T>(
    live: Vec<T>,
    preferred_ids: impl IntoIterator<Item = impl AsRef<str>>,
    id_of: impl Fn(&T) -> &str,
) -> Vec<T> {
    let live_ids: Vec<String> = live.iter().map(|row| id_of(row).to_string()).collect();
    let mut remaining: HashMap<String, T> = live_ids.iter().cloned().zip(live).collect();
    let mut ordered = Vec::with_capacity(remaining.len());
    for id in preferred_ids {
        if let Some(row) = remaining.remove(id.as_ref()) {
            ordered.push(row);
        }
    }
    for id in live_ids {
        if let Some(row) = remaining.remove(&id) {
            ordered.push(row);
        }
    }
    ordered
}

struct PendingDestination {
    row: RefusedRow,
    facts: LegacyDestinationFacts,
}

fn push_destination(
    destinations: &mut Vec<Destination>,
    refusals: &mut Vec<ProjectionRefusal>,
    pending: PendingDestination,
) {
    match destination_from_legacy(&pending.facts) {
        Ok(destination) => destinations.push(destination),
        Err(error) => refusals.push(ProjectionRefusal {
            row: pending.row,
            error,
        }),
    }
}

fn push_credential(
    credentials: &mut Vec<Credential>,
    refusals: &mut Vec<ProjectionRefusal>,
    destinations: &[Destination],
    facts: LegacyCredentialFacts,
) {
    match credential_from_legacy(&facts, destinations) {
        Ok(credential) => credentials.push(credential),
        // A credential whose own destination already refused would only add
        // a derived `MissingDestination`; the destination refusal names the
        // row and its real cause, so keep one entry per row.
        Err(MappingError::MissingDestination { .. })
            if refusals.iter().any(
                |refusal| matches!(&refusal.row, RefusedRow::Account { id, .. } if *id == facts.id),
            ) => {}
        Err(error) => refusals.push(ProjectionRefusal {
            row: RefusedRow::Account {
                id: facts.id,
                provider_id: facts.provider_id,
            },
            error,
        }),
    }
}

fn builtin_destination_facts(
    accounts: &[Account],
    cpa_destination_present: bool,
) -> Vec<PendingDestination> {
    let present: HashSet<&str> = accounts
        .iter()
        .map(|account| account.provider_id.as_str())
        .collect();
    let mut facts = Vec::new();
    for plan in &BUILTIN_PROVIDERS {
        if plan.provider_id == CUSTOM_PROVIDER_ID {
            continue;
        }
        if plan.provider_id == CPA_PROVIDER_ID {
            // CPA is persisted only once the integration writes its reserved
            // account or leftover maps onto the destination; until then there
            // is no destination to project (the projection never invents state).
            let configured = cpa_destination_present
                || accounts.iter().any(|account| {
                    account.id == CPA_ACCOUNT_ID || account.provider_id == CPA_PROVIDER_ID
                });
            if !configured {
                continue;
            }
            facts.push(PendingDestination {
                row: RefusedRow::Account {
                    id: CPA_ACCOUNT_ID.to_string(),
                    provider_id: CPA_PROVIDER_ID.to_string(),
                },
                facts: LegacyDestinationFacts::Cpa,
            });
            continue;
        }
        let singleton = plan.singleton_account_id.is_some()
            || plan.provider_id == OPENCODE_ZEN_FREE_PROVIDER_ID;
        if !singleton && !present.contains(plan.provider_id) {
            continue;
        }
        facts.push(PendingDestination {
            row: RefusedRow::Account {
                id: plan
                    .singleton_account_id
                    .unwrap_or(plan.provider_id)
                    .to_string(),
                provider_id: plan.provider_id.to_string(),
            },
            facts: LegacyDestinationFacts::Builtin {
                provider_id: plan.provider_id.to_string(),
            },
        });
    }
    facts
}

struct CustomAccountFacts {
    endpoint_url: String,
    protocol: UpstreamProtocolKind,
    model_capabilities: Vec<(String, String)>,
}

fn custom_account_facts(db: &Database, account_id: &str) -> anyhow::Result<CustomAccountFacts> {
    let config = db.account_custom_config(account_id)?;
    let protocol = config
        .as_ref()
        .map(|row| row.upstream_protocol)
        .unwrap_or(UpstreamProtocolKind::ChatCompletions);
    Ok(CustomAccountFacts {
        endpoint_url: config.map(|row| row.endpoint_url).unwrap_or_default(),
        protocol,
        model_capabilities: db
            .list_account_model_capabilities_for_projection(account_id)?
            .into_iter()
            .map(|row| (row.public_model, row.upstream_model))
            .collect(),
    })
}

fn platform_parent_facts(parent: &PlatformAccount) -> LegacyDestinationFacts {
    LegacyDestinationFacts::PlatformParent {
        id: parent.id.clone(),
        kind: match parent.kind {
            StoredPlatformKind::NewApi => PlatformKind::NewApi,
            StoredPlatformKind::Sub2api => PlatformKind::Sub2Api,
        },
        name: parent.name.clone(),
        base_url: parent.base_url.clone(),
        has_user_credential: parent.has_user_credential,
    }
}

fn credential_facts(
    account: &Account,
    order_index: u32,
    identity: Option<&IdentityAccountRecord>,
    platform_parent_id: Option<String>,
) -> LegacyCredentialFacts {
    let verified = identity
        .map(|record| record.verification_status == ConnectionVerificationStatus::Verified)
        .unwrap_or(false);
    let binding_enabled = identity
        .map(|record| record.binding_enabled)
        .unwrap_or(true);
    LegacyCredentialFacts {
        id: account.id.clone(),
        provider_id: account.provider_id.clone(),
        name: account.name.clone(),
        notes: account.notes.clone(),
        has_key: !account.key_cipher.is_empty(),
        enabled: account.enabled && binding_enabled,
        order_index,
        setup_step: account.setup_step,
        account_type: account.account_type,
        auth_error: account.auth_error.clone(),
        last_error: account.last_error.clone(),
        purchase_date: Some(account.purchase_date.clone()),
        cooldown_generic_until: account.cooldown_generic_until,
        cooldown_5h_until: account.cooldown_5h_until,
        cooldown_week_until: account.cooldown_week_until,
        cooldown_month_until: account.cooldown_month_until,
        cooldown_free_until: account.cooldown_free_until,
        verified,
        platform_link: platform_parent_id.map(|parent_id| LegacyPlatformLink { parent_id }),
        identity: identity.map(|record| LegacyIdentityFacts {
            quota_pool_id: record.quota_pool_id.clone(),
            model_scope: record.binding_model_scope.clone(),
            allowed_endpoint_ids: record.allowed_endpoint_ids.clone(),
            allowed_origins: record.allowed_origins.clone(),
            binding_enabled: record.binding_enabled,
        }),
    }
}

fn join_persisted_catalogs(
    db: &Database,
    destinations: &mut [Destination],
    accounts: &[Account],
    link_by_account: &HashMap<&str, String>,
) {
    // Catalogs are additive. Unreadable leftover protocol evidence must not
    // fail destination/credential persist — catalog writers refresh the
    // shadow in the same transaction, and a poison evidence row is meant to
    // fail reload after the write commits, not roll the write back.
    let Ok(zen) = db
        .zen_free_model_catalog()
        .map(|row| row.unwrap_or_default())
    else {
        return;
    };
    let Ok(persisted) = db.load_persisted_contracts() else {
        return;
    };
    let contracts = build_effective_contracts(&zen, &[], persisted);
    let mut by_builtin: HashMap<String, &EffectiveScopeContract> = HashMap::new();
    for scope in contracts.providers.values() {
        by_builtin.insert(destination_id_for_builtin(&scope.provider_id), scope);
    }

    let mut platform_capabilities: HashMap<String, Vec<AccountModelCapability>> = HashMap::new();
    for account in accounts {
        let Some(parent_id) = link_by_account.get(account.id.as_str()) else {
            continue;
        };
        let Ok(capabilities) = db.list_account_model_capabilities_for_projection(&account.id)
        else {
            continue;
        };
        platform_capabilities
            .entry(parent_id.clone())
            .or_default()
            .extend(capabilities);
    }

    let platform_catalogs: HashMap<String, Vec<CatalogModel>> = platform_capabilities
        .into_iter()
        .map(|(parent_id, capabilities)| {
            (
                destination_id_for_platform_account(&parent_id),
                platform_catalog_union(&capabilities),
            )
        })
        .collect();

    for destination in destinations.iter_mut() {
        if let Some(scope) = by_builtin.get(&destination.id) {
            destination.catalog = catalog_from_persisted_scope(scope);
            continue;
        }
        if let Some(catalog) = platform_catalogs.get(&destination.id) {
            destination.catalog = catalog.clone();
        }
    }
}

fn catalog_from_persisted_scope(scope: &EffectiveScopeContract) -> Vec<CatalogModel> {
    let mut catalog = Vec::new();
    let mut seen = HashSet::new();
    for model_id in &scope.catalog.models {
        let folded = model_id.to_ascii_lowercase();
        if !seen.insert(folded) {
            continue;
        }
        let Some(model) = scope.model(model_id) else {
            continue;
        };
        catalog.push(CatalogModel {
            public_model: model_id.clone(),
            upstream_model: model.model_id.clone(),
            protocols: model.enabled_protocols(),
            preferred: Some(model.preferred_protocol),
            enabled: model.has_enabled_protocol(),
            upstream_override: None,
        });
    }
    catalog
}

/// Same rule as frontend `platformModelOverlay`: public-name union,
/// case-insensitive, first wins.
fn platform_catalog_union(capabilities: &[AccountModelCapability]) -> Vec<CatalogModel> {
    let mut catalog = Vec::new();
    let mut seen = HashSet::new();
    for capability in capabilities {
        let public_model = capability.public_model.trim();
        if public_model.is_empty() {
            continue;
        }
        let folded = public_model.to_ascii_lowercase();
        if !seen.insert(folded) {
            continue;
        }
        let protocol = capability.protocol;
        catalog.push(CatalogModel {
            public_model: public_model.to_string(),
            upstream_model: capability.upstream_model.clone(),
            protocols: vec![protocol],
            preferred: Some(protocol),
            enabled: true,
            upstream_override: None,
        });
    }
    catalog
}

#[cfg(test)]
mod tests;
