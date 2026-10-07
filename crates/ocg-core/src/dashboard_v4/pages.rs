//! Local management projections. Every query filters a coherent immutable snapshot.
//! This cache is display-only and never authorizes inference or a mutation.
mod accounts;
mod aliases;
mod cache;
pub(crate) mod model_rows;
mod operations;
pub(crate) mod overview;
mod providers;
mod refresh;
#[cfg(test)]
mod tests;
pub(crate) mod types;

use super::types::{ConnectionSummary, DestinationCredentialDto, DestinationDto, RoutingCard};
use crate::{
    dashboard_v3::{self, ControlRevision, V3ApiError},
    state::CoreState,
};
use axum::{
    Json,
    extract::{Path, Query, State},
};
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use types::*;

pub(crate) use cache::PageReadCache;
pub(crate) use refresh::AutomaticRefreshCache;
pub(crate) use refresh::RefreshOutcome;
pub(super) use refresh::refresh_account;

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PageQuery {
    pub search: Option<String>,
    pub plan: Option<String>,
    pub status: Option<String>,
    pub sort: Option<String>,
    pub offset: Option<u32>,
    pub limit: Option<u32>,
    pub model: Option<String>,
    pub enabled_only: Option<bool>,
}
impl PageQuery {
    fn bounds(&self) -> (usize, usize) {
        (
            self.offset.unwrap_or(0) as usize,
            self.limit.unwrap_or(50).clamp(1, 100) as usize,
        )
    }
    fn search(&self) -> String {
        self.search
            .as_deref()
            .unwrap_or_default()
            .trim()
            .to_lowercase()
    }
}
fn count(n: usize) -> u32 {
    n.min(u32::MAX as usize) as u32
}
fn action(key: &str, allowed: bool) -> PageAction {
    PageAction {
        key: key.into(),
        allowed,
        reason: (!allowed).then(|| "unavailable".into()),
    }
}

pub(super) struct PageSnapshot {
    revision: ControlRevision,
    read_version: String,
    as_of: DateTime<Utc>,
    valid_until: DateTime<Utc>,
    cards: Vec<RoutingCard>,
    destinations: Vec<DestinationDto>,
    credentials: Vec<DestinationCredentialDto>,
    accounts: Vec<AccountSummary>,
    connections: Vec<ConnectionSummary>,
    contracts: dashboard_v3::ProviderContracts,
    definitions: Vec<dashboard_v3::ProviderDefinition>,
    catalog: Vec<dashboard_v3::ProviderCatalogEntry>,
    metadata: HashMap<String, Vec<super::model_metadata::DestinationModelMetadataEntry>>,
    platforms: Vec<crate::platform::PlatformAccount>,
    links: Vec<crate::platform::PlatformLink>,
    billing: HashMap<String, crate::billing_types::BillingStatus>,
    unpublished: Vec<String>,
    errors: Vec<PageReadIssue>,
    tags: HashMap<String, AccountPageTags>,
    custom_capabilities: HashMap<String, Vec<crate::models::AccountModelCapability>>,
    cpa_status: Option<String>,
    routing_mode: dashboard_v3::RoutingMode,
    conversation_sticky: bool,
}

fn snapshot(state: &CoreState) -> Result<Arc<PageSnapshot>, V3ApiError> {
    for _ in 0..3 {
        let before = cache::ReadVersion::capture(state)?;
        let now = state.sample_gateway_clock().0;
        if let Some(cached) = state.management_page_cache.lock().get(&before, now) {
            return Ok(cached);
        }
        let (
            mut routing,
            accounts,
            raw_accounts,
            custom,
            dynamic,
            zen,
            persisted,
            platforms,
            links,
            unpublished,
            billing,
            records,
            projection,
            identity,
            modelsdev,
            mut errors,
        ) = {
            let _settings = state.settings_update.lock();
            let routing = super::routing_cards::snapshot(state)
                .map_err(|_| V3ApiError::conflict_at(state, "destination projection refused"))?;
            let db = state.db.lock();
            let dynamic = db
                .list_control_plane_dynamic_providers()
                .map_err(V3ApiError::internal)?;
            let custom = db
                .list_custom_account_runtimes()
                .map_err(V3ApiError::internal)?;
            let raw_accounts = crate::destination_projection::list_accounts_for_v3(&db)
                .map_err(V3ApiError::internal)?;
            let mut accounts = Vec::new();
            let mut errors = Vec::new();
            let mut billing = HashMap::new();
            for raw in &raw_accounts {
                match accounts::summary_from_db(
                    state,
                    &db,
                    raw.clone(),
                    &dynamic,
                    count(
                        custom
                            .iter()
                            .find(|c| c.account_id == raw.id)
                            .map(|c| c.capabilities.len())
                            .unwrap_or(0),
                    ),
                ) {
                    Ok(account) => accounts.push(account),
                    Err(_) => errors.push(PageReadIssue {
                        resource: "account".into(),
                        id: Some(raw.id.clone()),
                        code: "read_failed".into(),
                    }),
                }
                match super::billing::cached_status(state, &db, &raw.id) {
                    Ok(status) => {
                        billing.insert(raw.id.clone(), status);
                    }
                    Err(_) => errors.push(PageReadIssue {
                        resource: "billing".into(),
                        id: Some(raw.id.clone()),
                        code: "read_failed".into(),
                    }),
                }
            }
            // Build from persisted sources, so a second SQLite connection can change catalogs.
            let zen = db
                .zen_free_model_catalog()
                .map_err(V3ApiError::internal)?
                .unwrap_or_default();
            let projection =
                crate::destination_projection::load_runtime(&db).map_err(V3ApiError::internal)?;
            let persisted = db
                .load_persisted_contracts()
                .map_err(V3ApiError::internal)?;
            let platforms = crate::destination_projection::list_platform_accounts_for_v3(&db)
                .map_err(V3ApiError::internal)?;
            let links = db.list_platform_links().map_err(V3ApiError::internal)?;
            let unpublished = db
                .list_unpublished_public_models()
                .map_err(V3ApiError::internal)?;
            let records = match crate::model_metadata::load(&db) {
                Ok(records) => records,
                Err(_) => {
                    errors.push(PageReadIssue {
                        resource: "metadata".into(),
                        id: None,
                        code: "read_failed".into(),
                    });
                    Vec::new()
                }
            };
            let identity = db.list_identity_model().map_err(V3ApiError::internal)?;
            let modelsdev = match crate::modelsdev::load(&db) {
                Ok(catalog) => catalog,
                Err(_) => {
                    errors.push(PageReadIssue {
                        resource: "modelsdev".into(),
                        id: None,
                        code: "read_failed".into(),
                    });
                    crate::modelsdev::ModelsDevCatalog::default()
                }
            };
            (
                routing,
                accounts,
                raw_accounts,
                custom,
                dynamic,
                zen,
                persisted,
                platforms,
                links,
                unpublished,
                billing,
                records,
                projection,
                identity,
                modelsdev,
                errors,
            )
        };
        let mut contracts =
            crate::provider_contracts::build_effective_contracts(&zen, &custom, persisted);
        contracts.apply_destination_configuration(&projection);
        let models_for = |provider: &str| {
            contracts
                .providers
                .get(provider)
                .map(|scope| scope.catalog.models.as_slice())
                .unwrap_or_default()
        };
        let pins = crate::provider_contracts::ollama_cloud_pinned_model_ids(&contracts);
        let mut catalog = crate::provider::BUILTIN_PROVIDERS
            .iter()
            .map(|plan| {
                dashboard_v3::providers::catalog_entry(
                    plan,
                    &zen.models,
                    models_for(crate::provider::COMMAND_CODE_PROVIDER_ID),
                    models_for(crate::provider::MINIMAX_PROVIDER_ID),
                    models_for(crate::provider::KIMI_PROVIDER_ID),
                    models_for(crate::provider::OLLAMA_PROVIDER_ID),
                    &pins,
                )
            })
            .collect::<Vec<_>>();
        catalog.extend(
            dynamic
                .iter()
                .map(dashboard_v3::providers::dynamic_catalog_entry),
        );
        let definitions = dynamic
            .into_iter()
            .map(|runtime| {
                dashboard_v3::dynamic_providers::to_wire(
                    runtime,
                    routing.revision.revision,
                    routing.revision.process_generation,
                )
            })
            .collect();
        let mut names = HashMap::<String, usize>::new();
        let mut members = HashMap::<&str, Vec<&crate::db::identity::IdentityAccountRecord>>::new();
        for raw in &raw_accounts {
            *names.entry(raw.name.trim().to_lowercase()).or_default() += 1;
        }
        for record in &identity.accounts {
            members.entry(&record.identity_id).or_default().push(record);
        }
        let tags = identity
            .accounts
            .iter()
            .map(|record| {
                let members = members.get(record.identity_id.as_str()).unwrap();
                let siblings = members
                    .iter()
                    .filter(|other| {
                        other.account.id != record.account.id
                            && record.quota_pool_id.is_some()
                            && other.quota_pool_id == record.quota_pool_id
                    })
                    .map(|r| r.account.name.trim())
                    .filter(|name| !name.is_empty())
                    .collect::<Vec<_>>();
                (
                    record.account.id.clone(),
                    AccountPageTags {
                        credential_count: count(members.len()),
                        binding_disabled: !record.binding_enabled,
                        quota_share_name: (siblings.len() == 1).then(|| siblings[0].to_string()),
                        quota_share_count: count(siblings.len()),
                        duplicate_name: names
                            .get(&record.account.name.trim().to_lowercase())
                            .is_some_and(|count| *count > 1),
                    },
                )
            })
            .collect();
        let custom_capabilities = custom
            .into_iter()
            .map(|c| (c.account_id, c.capabilities))
            .collect();
        // Projection storage can contain a legacy upstream error. Only the
        // account mapper's known-secret-redacted error is safe for a page read.
        for credential in &mut routing.credentials {
            credential.last_error = accounts
                .iter()
                .find(|a| a.id == credential.legacy_account_id)
                .and_then(|a| a.last_error.clone());
        }
        let metadata = if errors.iter().any(|e| e.resource == "metadata") {
            HashMap::new()
        } else {
            projection
                .destinations
                .iter()
                .map(|d| {
                    (
                        d.id.clone(),
                        super::model_metadata::entries(&records, &modelsdev, d),
                    )
                })
                .collect()
        };
        let connections = super::connections::list_connections_locked(state)?.connections;
        let statuses = accounts
            .iter()
            .map(|a| {
                (
                    a.id.clone(),
                    match a.verification_status {
                        dashboard_v3::AccountVerificationStatus::NotRequired => {
                            crate::provider::ConnectionVerificationStatus::NotRequired
                        }
                        dashboard_v3::AccountVerificationStatus::Pending => {
                            crate::provider::ConnectionVerificationStatus::Pending
                        }
                        dashboard_v3::AccountVerificationStatus::Verified => {
                            crate::provider::ConnectionVerificationStatus::Verified
                        }
                        dashboard_v3::AccountVerificationStatus::Failed => {
                            crate::provider::ConnectionVerificationStatus::Failed
                        }
                    },
                )
            })
            .collect();
        let contracts = dashboard_v3::providers::provider_contracts_from_capture(
            state,
            &contracts,
            &raw_accounts,
            &statuses,
            &projection,
        )?;
        let after = cache::ReadVersion::capture(state)?;
        if before != after {
            continue;
        }
        let config = state.config();
        let cpa_runtime = state.cpa_runtime_snapshot();
        if cpa_runtime.error.is_some() {
            errors.push(PageReadIssue {
                resource: "cpa".into(),
                id: None,
                code: "read_failed".into(),
            });
        }
        let cpa_configured = state
            .db
            .lock()
            .cpa_integration()
            .map_err(V3ApiError::internal)?
            .is_some()
            && accounts
                .iter()
                .any(|a| a.provider_id == crate::provider::CPA_PROVIDER_ID);
        let cpa_status = cpa_status(&cpa_runtime, cpa_configured);
        let mut valid_until = now + Duration::seconds(15);
        for credential in &routing.credentials {
            for at in [
                &credential.cooldowns.generic_until,
                &credential.cooldowns.five_hour_until,
                &credential.cooldowns.week_until,
                &credential.cooldowns.month_until,
                &credential.cooldowns.free_until,
            ] {
                if let Some(at) = at
                    .as_deref()
                    .and_then(|v| DateTime::parse_from_rfc3339(v).ok())
                    .map(|v| v.with_timezone(&Utc))
                    && at > now
                {
                    valid_until = valid_until.min(at);
                }
            }
            if let Some(recovery) = &credential.quota_recovery
                && let Ok(at) = DateTime::parse_from_rfc3339(&recovery.next_retry_at)
                && at > now
            {
                valid_until = valid_until.min(at.with_timezone(&Utc));
            }
        }
        for status in billing.values() {
            valid_until = valid_until.min(super::billing_cache::next_change(status, now));
        }
        errors.sort_by(|a, b| a.resource.cmp(&b.resource).then_with(|| a.id.cmp(&b.id)));
        let snapshot = Arc::new(PageSnapshot {
            revision: routing.revision,
            read_version: after.token(),
            as_of: now,
            valid_until,
            cards: routing.cards,
            destinations: routing.destinations,
            credentials: routing.credentials,
            accounts,
            connections,
            contracts,
            definitions,
            catalog,
            metadata,
            platforms,
            links,
            billing,
            unpublished,
            errors,
            tags,
            custom_capabilities,
            cpa_status,
            routing_mode: match config.routing_mode {
                crate::models::RoutingMode::StrictPriority => {
                    dashboard_v3::RoutingMode::StrictPriority
                }
                crate::models::RoutingMode::StickyGlobal => dashboard_v3::RoutingMode::StickyGlobal,
                crate::models::RoutingMode::RoundRobin => dashboard_v3::RoutingMode::RoundRobin,
            },
            conversation_sticky: config.conversation_sticky,
        });
        state
            .management_page_cache
            .lock()
            .insert(after, snapshot.clone());
        return Ok(snapshot);
    }
    Err(V3ApiError::conflict_at(
        state,
        "management data changed during read",
    ))
}
fn cpa_status(
    runtime: &crate::cpa_runtime::CpaRuntimeSnapshot,
    configured: bool,
) -> Option<String> {
    use crate::cpa_runtime::CpaRuntimePhase;
    if runtime.phase == CpaRuntimePhase::Idle && runtime.error.is_some() {
        return Some("failed".into());
    }
    Some(
        match runtime.phase {
            CpaRuntimePhase::Checking => "checking",
            CpaRuntimePhase::Downloading => "downloading",
            CpaRuntimePhase::Installing => "installing",
            CpaRuntimePhase::Starting => "starting",
            CpaRuntimePhase::Failed => "failed",
            CpaRuntimePhase::Idle => {
                if runtime.owned {
                    if !runtime.installed {
                        "not_installed"
                    } else if runtime.running {
                        "running"
                    } else {
                        "stopped"
                    }
                } else if configured {
                    "external"
                } else {
                    return None;
                }
            }
        }
        .into(),
    )
}

macro_rules! page_get {
    ($name:ident,$ty:ty,$project:path) => {
        pub(super) async fn $name(
            State(state): State<CoreState>,
            Query(query): Query<PageQuery>,
        ) -> Result<Json<$ty>, V3ApiError> {
            tokio::task::spawn_blocking(move || {
                let snapshot = snapshot(&state)?;
                Ok(Json($project(&snapshot, &query)))
            })
            .await
            .map_err(V3ApiError::internal)?
        }
    };
}
page_get!(accounts_page, AccountsPage, accounts::project);
page_get!(providers_page, ProvidersPage, providers::project);
page_get!(aliases_page, AliasesPage, aliases::project);
pub(super) async fn card_credentials(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    Query(query): Query<PageQuery>,
) -> Result<Json<AccountCardCredentialsPage>, V3ApiError> {
    tokio::task::spawn_blocking(move || {
        accounts::credentials(&state, snapshot(&state)?.as_ref(), &id, &query).map(Json)
    })
    .await
    .map_err(V3ApiError::internal)?
}
pub(super) async fn account_detail(
    State(state): State<CoreState>,
    Path(id): Path<String>,
) -> Result<Json<AccountPageDetail>, V3ApiError> {
    tokio::task::spawn_blocking(move || {
        accounts::detail(&state, snapshot(&state)?.as_ref(), &id).map(Json)
    })
    .await
    .map_err(V3ApiError::internal)?
}
pub(super) async fn account_layout(
    State(state): State<CoreState>,
) -> Result<Json<AccountPageLayout>, V3ApiError> {
    tokio::task::spawn_blocking(move || {
        let snapshot = snapshot(&state)?;
        Ok(Json(AccountPageLayout {
            revision: snapshot.revision.clone(),
            cards: snapshot.cards.clone(),
        }))
    })
    .await
    .map_err(V3ApiError::internal)?
}
pub(super) async fn provider_detail(
    State(state): State<CoreState>,
    Path(id): Path<String>,
) -> Result<Json<ProviderPageDetail>, V3ApiError> {
    tokio::task::spawn_blocking(move || {
        providers::detail(&state, snapshot(&state)?.as_ref(), &id).map(Json)
    })
    .await
    .map_err(V3ApiError::internal)?
}
pub(super) async fn provider_models(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    Query(query): Query<PageQuery>,
) -> Result<Json<ProviderModelsPage>, V3ApiError> {
    tokio::task::spawn_blocking(move || {
        providers::models(&state, snapshot(&state)?.as_ref(), &id, &query).map(Json)
    })
    .await
    .map_err(V3ApiError::internal)?
}
pub(super) async fn provider_edit_detail(
    State(state): State<CoreState>,
    Path(id): Path<String>,
) -> Result<Json<ProviderEditDetail>, V3ApiError> {
    tokio::task::spawn_blocking(move || {
        providers::edit_detail(&state, snapshot(&state)?.as_ref(), &id).map(Json)
    })
    .await
    .map_err(V3ApiError::internal)?
}

impl From<&DestinationDto> for DestinationSummary {
    fn from(d: &DestinationDto) -> Self {
        Self {
            account_controls: d.account_controls.clone(),
            id: d.id.clone(),
            legacy: d.legacy.clone(),
            adapter: d.adapter,
            name: d.name.clone(),
            brand_family: d.brand_family.clone(),
            base_url: d.base_url.clone(),
            protocols: d.protocols.clone(),
            protocol_routes: d.protocol_routes.clone(),
            auth_scheme: d.auth_scheme,
            model_resolution: d.model_resolution,
            capabilities: d.capabilities.clone(),
            plan: d.plan.clone(),
            max_credentials: d.max_credentials,
            observer_credential_id: d.observer_credential_id.clone(),
            enabled: d.enabled,
            catalog_count: count(d.catalog.len()),
            enabled_catalog_count: count(d.catalog.iter().filter(|m| m.enabled).count()),
        }
    }
}
