//! Local/Zen Dashboard V3 provider control plane.
//!
//! Catalog, contracts, model capabilities, and saved Zen models are local
//! reads. Zen enablement and provider-scope protocol switches share the V3
//! CAS envelope. Zen catalog refresh uses the fixed official keyless directory.
//! Every built-in Provider scope shares the crate-root protocol-probe transport.
//! Custom scopes hide the Provider Test action; account-page tests use V3 model-tests.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use chrono::{DateTime, Utc};
#[cfg(debug_assertions)]
use std::collections::BTreeMap;
use std::collections::{HashMap, HashSet};

#[cfg(debug_assertions)]
use futures_util::StreamExt;
#[cfg(debug_assertions)]
use parking_lot::Mutex;
#[cfg(debug_assertions)]
use std::net::{Ipv4Addr, Ipv6Addr};
#[cfg(debug_assertions)]
use std::time::Duration;

use crate::alias;
use crate::goat;
use crate::kernel::ids::is_free_model;
#[cfg(debug_assertions)]
use crate::kernel::zen::{ZEN_MODELS_SOURCE_URL, parse_catalog};
use crate::kernel::zen::{ZenFreeModelCatalog, model_views};
use crate::models::{Account as ModelAccount, AppConfig, ForwardLog, UpstreamChannel};
use crate::protocol_probe::{self, ProtocolProbeContext, ProtocolProbeRunError};
use crate::provider::{
    BUILTIN_PROVIDERS, BuiltinProvider, COMMAND_CODE_PROVIDER_ID, CUSTOM_PROVIDER_ID,
    ConnectionVerificationStatus, KIMI_CN_BASE_URL, KIMI_PROVIDER_ID, MINIMAX_CN_BASE_URL,
    MINIMAX_PROVIDER_ID, OLLAMA_PROVIDER_ID, OPENCODE_PROVIDER_ID, OPENCODE_ZEN_FREE_PROVIDER_ID,
    ProviderAdapterKind, ProviderOrigin, ProviderRegistry, ZEN_FREE_ACCOUNT_ID, builtin_offering,
    default_verification_status,
};
use crate::provider_contracts::{
    self, ContractScope, EffectiveContractSet, EffectiveModelContract as DomainModelContract,
    EffectiveProtocolEvidence as DomainProtocolEvidence, PersistedModelProtocol,
    ProtocolOverrideState as DomainProtocolOverrideState,
};
use crate::routing_runtime::{account_is_available_for_at, free_channel_is_exhausted_at};
use crate::state::CoreState;

use super::accounts::load_model_account;
use super::types::{
    AccountAuthScheme, AccountQuotaScope, AccountUpstreamProtocol, AccountVerificationStatus,
    CapabilitySummary, CardCapabilitySummary, ContractEvidenceSource, ContractScopeKind,
    ControlRevision, CustomEndpointContract, EffectiveCatalog, EffectiveModelContract,
    EffectiveModelProtocols, EffectiveProtocolEvidence, ModelProtocolOverride,
    ModelProtocolOverridesUpdate, MutationExpectation, ProbeResultKind, ProtocolOverrideState,
    ProtocolProbeRequest, ProtocolProbeResponse, ProtocolProbeResult, ProviderAccountChoice,
    ProviderCatalog, ProviderCatalogEntry, ProviderCatalogFormField, ProviderContractGroup,
    ProviderContracts, ProviderModelCapability, ProviderModels, ProviderModelsRefreshUpdate,
    ZenFreeModel, ZenFreeModels, ZenFreeSettings, ZenFreeSettingsUpdate,
};
use super::{V3ApiError, check_expectation, parse_mutation_json};

#[cfg(debug_assertions)]
const MAX_ZEN_BODY_BYTES: usize = 512 * 1024;
#[cfg(debug_assertions)]
const ZEN_REFRESH_TIMEOUT_SECS: u64 = 30;

#[cfg(debug_assertions)]
static ZEN_MODELS_SOURCE_OVERRIDES: Mutex<BTreeMap<u64, String>> = Mutex::new(BTreeMap::new());

/// Loopback-only Zen directory used by Dashboard V3 refresh tests.
///
/// Keyed by `CoreState::process_generation` so parallel harnesses cannot
/// overwrite each other. Compiled out of release production.
#[cfg(debug_assertions)]
pub fn set_zen_models_source_url_override_for_tests(process_generation: u64, url: Option<String>) {
    let mut overrides = ZEN_MODELS_SOURCE_OVERRIDES.lock();
    match url.and_then(|value| parse_loopback_http_url(&value)) {
        Some(canonical) => {
            overrides.insert(process_generation, canonical);
        }
        None => {
            overrides.remove(&process_generation);
        }
    }
}

#[cfg(debug_assertions)]
fn debug_zen_models_source_url(process_generation: u64) -> Option<String> {
    ZEN_MODELS_SOURCE_OVERRIDES
        .lock()
        .get(&process_generation)
        .cloned()
}

/// Accept only an unambiguous loopback HTTP(S) origin: parsed host must be
/// exactly `127.0.0.1`, `localhost`, or `::1`, with no userinfo, query, or
/// fragment. Prefix matching is not used.
#[cfg(debug_assertions)]
fn parse_loopback_http_url(url: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(url.trim()).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return None;
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return None;
    }
    if !host_is_exact_loopback(&parsed) {
        return None;
    }
    Some(parsed.as_str().to_string())
}

#[cfg(debug_assertions)]
fn host_is_exact_loopback(parsed: &reqwest::Url) -> bool {
    let Some(host) = parsed.host() else {
        return false;
    };
    let rendered = host.to_string();
    if let Some(inside) = rendered
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
    {
        return inside
            .parse::<Ipv6Addr>()
            .is_ok_and(|ip| ip == Ipv6Addr::LOCALHOST);
    }
    if let Ok(ip) = rendered.parse::<Ipv4Addr>() {
        return ip == Ipv4Addr::LOCALHOST;
    }
    rendered.eq_ignore_ascii_case("localhost")
}

pub(super) async fn get_providers(State(state): State<CoreState>) -> Json<ProviderCatalog> {
    let _settings_update = state.settings_update.lock();
    Json(provider_catalog_from_state(&state))
}

pub(super) async fn get_model_capabilities(
    State(state): State<CoreState>,
) -> Json<Vec<ProviderModelCapability>> {
    let _settings_update = state.settings_update.lock();
    Json(model_capabilities(&state.provider_contracts()))
}

pub(super) async fn get_zen_free_settings(
    State(state): State<CoreState>,
) -> Result<Json<ZenFreeSettings>, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    zen_free_settings_from_state(&state).map(Json)
}

pub(super) async fn patch_zen_free_settings(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<ZenFreeSettings>, V3ApiError> {
    let input = parse_mutation_json::<ZenFreeSettingsUpdate>(&body)?;
    let _settings_update = state.settings_update.lock();
    check_expectation(&state, &input.expectation)?;
    {
        let db = state.db.lock();
        db.set_zen_free_enabled(input.enabled)
            .map_err(V3ApiError::internal)?;
    }
    let revision = state.bump_settings_revision();
    state.log_runtime_event(
        "info",
        "provider",
        &format!(
            "event=zen_free_state_updated enabled={} revision={revision}",
            input.enabled
        ),
    );
    zen_free_settings_from_state(&state).map(Json)
}

pub(super) async fn get_zen_free_models(State(state): State<CoreState>) -> Json<ZenFreeModels> {
    let _settings_update = state.settings_update.lock();
    Json(zen_free_models_from_state(&state))
}

pub(super) async fn refresh_zen_free_models(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<ZenFreeModels>, V3ApiError> {
    let expectation = parse_mutation_json::<MutationExpectation>(&body)?;
    let _refresh = state.zen_free_models_refresh.try_lock().map_err(|_| {
        V3ApiError::conflict_at(&state, "Zen Free model refresh is already running")
    })?;
    let config = {
        let _settings_update = state.settings_update.lock();
        check_expectation(&state, &expectation)?;
        state.config()
    };
    let official_protocols = crate::official_protocols::fetch_official_protocol_baseline(
        &config,
        OPENCODE_ZEN_FREE_PROVIDER_ID,
        state.process_generation(),
    )
    .await;
    let fetched = fetch_zen_free_catalog(&state, &config).await;
    let _settings_update = state.settings_update.lock();
    let catalog = match fetched {
        Ok(catalog) => catalog,
        Err(message) => {
            audit_catalog_failure(&state, OPENCODE_ZEN_FREE_PROVIDER_ID, "fetch");
            return Err(V3ApiError::outbound_failed(&state, message));
        }
    };
    check_expectation(&state, &expectation)?;
    if catalog.models.is_empty() {
        audit_catalog_failure(&state, OPENCODE_ZEN_FREE_PROVIDER_ID, "empty_catalog");
        return Err(V3ApiError::outbound_failed(
            &state,
            "Zen model catalog contains no model IDs ending in `-free`",
        ));
    }
    let models = catalog.models.clone();
    let model_count = models.len();
    state
        .activate_zen_free_model_catalog(catalog)
        .map_err(V3ApiError::internal)?;
    let now = Utc::now();
    {
        let db = state.db.lock();
        db.apply_official_protocol_baseline(
            &ContractScope::provider(OPENCODE_ZEN_FREE_PROVIDER_ID),
            &models,
            &official_protocols,
            now,
        )
        .map_err(V3ApiError::internal)?;
        state
            .reload_provider_contracts_locked(&db)
            .map_err(V3ApiError::internal)?;
    }
    state.routing.reset();
    let revision = state.bump_settings_revision();
    audit_catalog_success(&state, OPENCODE_ZEN_FREE_PROVIDER_ID, model_count, revision);
    Ok(Json(zen_free_models_from_state(&state)))
}

/// Catalog fetch is control-plane, not routing: a ready stored Key is enough
/// even when the new account is still disabled.
fn account_can_supply_catalog_refresh_key(account: &ModelAccount) -> bool {
    account.setup_step.is_ready()
        && !account.key_cipher.trim().is_empty()
        && account.auth_error.is_none()
}

fn select_catalog_refresh_account(
    accounts: impl IntoIterator<Item = ModelAccount>,
    provider_id: &str,
) -> Option<ModelAccount> {
    accounts
        .into_iter()
        .filter(|account| {
            account.provider_id == provider_id && account_can_supply_catalog_refresh_key(account)
        })
        .max_by_key(|account| account.enabled)
}

struct GoCommandCatalogRefresh {
    provider_id: String,
    account_id: Option<String>,
    models: Vec<String>,
    refreshed_at: DateTime<Utc>,
    source_url: String,
    revision: u64,
}

async fn refresh_go_or_command_catalog(
    state: &CoreState,
    provider_id: &str,
    expectation: &MutationExpectation,
) -> Result<GoCommandCatalogRefresh, V3ApiError> {
    if provider_id != OPENCODE_PROVIDER_ID && provider_id != COMMAND_CODE_PROVIDER_ID {
        return Err(V3ApiError::invalid_request_at(
            state,
            "this provider does not support model refresh",
        ));
    }
    let _refresh = state
        .provider_models_refresh
        .try_lock()
        .map_err(|_| V3ApiError::conflict_at(state, "provider model refresh is already running"))?;
    let scope = ContractScope::provider(provider_id);
    let (config, base_url, source_url, previous_models) = {
        let _settings_update = state.settings_update.lock();
        check_expectation(state, expectation)?;
        validate_provider_scope(state, &scope)?;
        let config = state.config();
        let base_url = if provider_id == OPENCODE_PROVIDER_ID {
            crate::gateway::free_models::opencode_go_base_url(&config.upstream_base_url)
        } else {
            #[cfg(debug_assertions)]
            {
                goat::goat_catalog_base_url(Some(state.process_generation()))
            }
            #[cfg(not(debug_assertions))]
            {
                crate::provider::COMMAND_CODE_GOAT_BASE_URL.to_string()
            }
        };
        let source_url = if provider_id == OPENCODE_PROVIDER_ID {
            goat::opencode_go_models_url_for_base(&base_url)
        } else {
            goat::goat_models_url_for_base(&base_url)
        };
        let previous_models = state
            .provider_contracts()
            .scope(&scope)
            .map(|contract| contract.catalog.models.clone())
            .unwrap_or_default();
        (config, base_url, source_url, previous_models)
    };

    let models_result = if provider_id == OPENCODE_PROVIDER_ID {
        goat::refresh_opencode_go_models(&config, &base_url).await
    } else {
        goat::refresh_command_code_models(&config, &base_url).await
    };
    let models = match models_result {
        Ok(models) => models,
        Err(failure) => {
            audit_catalog_failure(state, provider_id, "fetch");
            return Err(V3ApiError::outbound_failed(state, failure.message));
        }
    };
    if models.is_empty() {
        audit_catalog_failure(state, provider_id, "empty_catalog");
        return Err(V3ApiError::outbound_failed(
            state,
            if provider_id == COMMAND_CODE_PROVIDER_ID {
                "Command Code model refresh returned an empty catalog"
            } else {
                "provider model refresh returned an empty catalog"
            },
        ));
    }
    let official_protocols = crate::official_protocols::fetch_official_protocol_baseline(
        &config,
        provider_id,
        state.process_generation(),
    )
    .await;
    // Zen Free owns every `-free` id; keep them out of the persisted Go
    // catalog so they never reach the Go provider-contracts surface.
    let models = if provider_id == OPENCODE_PROVIDER_ID {
        let filtered: Vec<String> = models.into_iter().filter(|id| !is_free_model(id)).collect();
        if filtered.is_empty() {
            audit_catalog_failure(state, provider_id, "zen_only_catalog");
            return Err(V3ApiError::outbound_failed(
                state,
                "provider model refresh returned only Zen Free models",
            ));
        }
        filtered
    } else {
        models
    };

    let now = Utc::now();
    let _settings_update = state.settings_update.lock();
    check_expectation(state, expectation)?;
    let source = if provider_id == OPENCODE_PROVIDER_ID {
        provider_contracts::CATALOG_SOURCE_OPENCODE_MODELS
    } else {
        provider_contracts::CATALOG_SOURCE_COMMAND_CODE_MODELS
    };
    {
        let db = state.db.lock();
        db.refresh_contract_catalog_with_default_off(
            &scope,
            &previous_models,
            &models,
            now,
            source,
            &source_url,
        )
        .map_err(V3ApiError::internal)?;
        db.apply_official_protocol_baseline(&scope, &models, &official_protocols, now)
            .map_err(V3ApiError::internal)?;
        state
            .reload_provider_contracts_locked(&db)
            .map_err(V3ApiError::internal)?;
    }
    state.routing.reset();
    let revision = state.bump_settings_revision();
    audit_catalog_success(state, provider_id, models.len(), revision);
    Ok(GoCommandCatalogRefresh {
        provider_id: provider_id.to_string(),
        account_id: None,
        models,
        refreshed_at: now,
        source_url,
        revision,
    })
}

pub(super) async fn refresh_provider_models(
    State(state): State<CoreState>,
    Path(provider_id): Path<String>,
    body: Bytes,
) -> Result<Json<ProviderModels>, V3ApiError> {
    let input = parse_mutation_json::<ProviderModelsRefreshUpdate>(&body)?;
    // The optional legacy accountId is accepted but is not used for public discovery.
    let refreshed = refresh_go_or_command_catalog(&state, &provider_id, &input.expectation).await?;
    Ok(Json(ProviderModels {
        provider_id: refreshed.provider_id,
        account_id: refreshed.account_id,
        models: refreshed.models,
        refreshed_at: refreshed.refreshed_at.to_rfc3339(),
        source_url: refreshed.source_url,
        revision: refreshed.revision,
        process_generation: state.process_generation(),
        pricing_revision: state.pricing_snapshot().revision.clone(),
    }))
}

pub(super) async fn refresh_contract_catalog(
    State(state): State<CoreState>,
    Path((scope_kind, scope_id)): Path<(String, String)>,
    body: Bytes,
) -> Result<Json<ProviderContracts>, V3ApiError> {
    let expectation = parse_mutation_json::<MutationExpectation>(&body)?;
    let scope = ContractScope::parse(&scope_kind, &scope_id)
        .map_err(|message| V3ApiError::invalid_request_at(&state, message))?;
    if scope_kind == provider_contracts::SCOPE_KIND_CUSTOM_ENDPOINT {
        return Err(V3ApiError::invalid_request_at(
            &state,
            "Custom API model catalogs are account declarations and cannot be refreshed",
        ));
    }
    if scope_kind != provider_contracts::SCOPE_KIND_PROVIDER {
        return Err(V3ApiError::not_found_at(&state, "provider scope not found"));
    }
    if scope_id == OPENCODE_ZEN_FREE_PROVIDER_ID {
        let _ = refresh_zen_free_models(State(state.clone()), body).await?;
        return provider_contracts_response(&state);
    }
    if scope_id == OLLAMA_PROVIDER_ID {
        // Public keyless GET /models: no account, no Key verification, explicit
        // control-plane refresh only. Reuses the process-wide catalog lock.
        let _refresh = state.provider_models_refresh.try_lock().map_err(|_| {
            V3ApiError::conflict_at(&state, "provider model refresh is already running")
        })?;
        let (config, base_url, source_url) = {
            let _settings_update = state.settings_update.lock();
            check_expectation(&state, &expectation)?;
            validate_provider_scope(&state, &scope)?;
            let config = state.config();
            let base_url = {
                #[cfg(debug_assertions)]
                {
                    goat::ollama_cloud_models_base_url(Some(state.process_generation()))
                }
                #[cfg(not(debug_assertions))]
                {
                    crate::provider::OLLAMA_CLOUD_BASE_URL.to_string()
                }
            };
            let source_url = goat::ollama_cloud_models_url_for_base(&base_url);
            (config, base_url, source_url)
        };
        let models = match goat::refresh_ollama_cloud_models(&config, &base_url).await {
            Ok(models) => models,
            Err(failure) => {
                audit_catalog_failure(&state, &scope_id, "fetch");
                return Err(V3ApiError::outbound_failed(&state, failure.message));
            }
        };
        if models.is_empty() {
            audit_catalog_failure(&state, &scope_id, "empty_catalog");
            return Err(V3ApiError::outbound_failed(
                &state,
                "Ollama Cloud model refresh returned an empty catalog",
            ));
        }
        let now = Utc::now();
        let _settings_update = state.settings_update.lock();
        check_expectation(&state, &expectation)?;
        {
            let db = state.db.lock();
            let previous = db
                .load_persisted_scope(&scope)
                .map_err(V3ApiError::internal)?
                .map(|row| row.catalog_models)
                .unwrap_or_default();
            db.refresh_contract_catalog_with_default_off(
                &scope,
                &previous,
                &models,
                now,
                provider_contracts::CATALOG_SOURCE_OLLAMA_CLOUD_MODELS,
                &source_url,
            )
            .map_err(V3ApiError::internal)?;
            state
                .reload_provider_contracts_locked(&db)
                .map_err(V3ApiError::internal)?;
        }
        state.routing.reset();
        let revision = state.bump_settings_revision();
        audit_catalog_success(&state, &scope_id, models.len(), revision);
        return provider_contracts_response(&state);
    }
    if matches!(scope_id.as_str(), MINIMAX_PROVIDER_ID | KIMI_PROVIDER_ID) {
        let _refresh = state.provider_models_refresh.try_lock().map_err(|_| {
            V3ApiError::conflict_at(&state, "provider model refresh is already running")
        })?;
        let (account, config, key, base_url, source_url, source) = {
            let _settings_update = state.settings_update.lock();
            check_expectation(&state, &expectation)?;
            validate_provider_scope(&state, &scope)?;
            let account = select_catalog_refresh_account(
                state
                    .db
                    .lock()
                    .list_accounts()
                    .map_err(V3ApiError::internal)?,
                &scope_id,
            )
            .ok_or_else(|| {
                V3ApiError::invalid_request_at(
                    &state,
                    "no eligible account is available for provider catalog refresh",
                )
            })?;
            let key = state
                .decrypt_key(&account.key_cipher)
                .map_err(V3ApiError::internal)?;
            let config = state.config();
            let (base_url, source) = if scope_id == MINIMAX_PROVIDER_ID {
                (
                    MINIMAX_CN_BASE_URL.to_string(),
                    provider_contracts::CATALOG_SOURCE_MINIMAX_CN_MODELS,
                )
            } else {
                (
                    KIMI_CN_BASE_URL.to_string(),
                    provider_contracts::CATALOG_SOURCE_KIMI_CN_MODELS,
                )
            };
            let source_url = goat::goat_models_url_for_base(&base_url);
            (account, config, key, base_url, source_url, source)
        };
        let models_result = goat::probe_provider_models(
            &config,
            &key,
            &base_url,
            if scope_id == MINIMAX_PROVIDER_ID {
                "MiniMax CN"
            } else {
                "Kimi Code CN"
            },
        )
        .await;
        let models = match models_result {
            Ok(models) => models,
            Err(failure) => {
                audit_catalog_failure(&state, &scope_id, "fetch");
                return Err(V3ApiError::outbound_failed(&state, failure.message));
            }
        };
        let now = Utc::now();
        let _settings_update = state.settings_update.lock();
        check_expectation(&state, &expectation)?;
        let current = load_model_account(&state, &account.id)?;
        if current.updated_at != account.updated_at || current.key_cipher != account.key_cipher {
            return Err(V3ApiError::conflict_at(
                &state,
                "the selected account changed while models were refreshing",
            ));
        }
        {
            let db = state.db.lock();
            let previous = db
                .load_persisted_scope(&scope)
                .map_err(V3ApiError::internal)?
                .map(|row| row.catalog_models)
                .unwrap_or_default();
            db.refresh_contract_catalog_with_default_off(
                &scope,
                &previous,
                &models,
                now,
                source,
                &source_url,
            )
            .map_err(V3ApiError::internal)?;
            state
                .reload_provider_contracts_locked(&db)
                .map_err(V3ApiError::internal)?;
        }
        state.routing.reset();
        let revision = state.bump_settings_revision();
        audit_catalog_success(&state, &scope_id, models.len(), revision);
        return provider_contracts_response(&state);
    }
    if scope_id == COMMAND_CODE_PROVIDER_ID || scope_id == OPENCODE_PROVIDER_ID {
        refresh_go_or_command_catalog(&state, &scope_id, &expectation).await?;
        return provider_contracts_response(&state);
    }
    Err(V3ApiError::not_found_at(&state, "provider scope not found"))
}

pub(super) async fn get_provider_contracts(
    State(state): State<CoreState>,
) -> Result<Json<ProviderContracts>, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    let contracts = state.provider_contracts();
    let (accounts, statuses) = load_accounts_with_verification(&state)?;
    Ok(Json(provider_contracts_from_state(
        &state, &contracts, &accounts, &statuses,
    )))
}

fn provider_contracts_response(state: &CoreState) -> Result<Json<ProviderContracts>, V3ApiError> {
    let contracts = state.provider_contracts();
    let (accounts, statuses) = load_accounts_with_verification(state)?;
    Ok(Json(provider_contracts_from_state(
        state, &contracts, &accounts, &statuses,
    )))
}

pub(super) async fn put_provider_model_protocol_overrides(
    State(state): State<CoreState>,
    Path(scope_id): Path<String>,
    body: Bytes,
) -> Result<Json<ProviderContracts>, V3ApiError> {
    let input = parse_mutation_json::<ModelProtocolOverridesUpdate>(&body)?;
    let _settings_update = state.settings_update.lock();
    check_expectation(&state, &input.expectation)?;
    let scope = ContractScope::provider(&scope_id);
    validate_provider_scope(&state, &scope)?;
    validate_provider_protocol_overrides(&state, &scope_id, &input.overrides)?;
    commit_model_protocol_overrides(&state, &scope, input.overrides)
}

fn validate_provider_protocol_overrides(
    state: &CoreState,
    scope_id: &str,
    overrides: &[ModelProtocolOverride],
) -> Result<(), V3ApiError> {
    provider_contracts::provider_scope_descriptor(scope_id)
        .ok_or_else(|| V3ApiError::not_found_at(state, "provider not found"))?;
    let contracts = state.provider_contracts();
    let scope = contracts
        .providers
        .get(scope_id)
        .ok_or_else(|| V3ApiError::not_found_at(state, "provider scope not found"))?;
    for item in overrides {
        let model = scope.model(&item.model_id).ok_or_else(|| {
            V3ApiError::invalid_request_at(
                state,
                "modelId is not present in the current provider catalog",
            )
        })?;
        let protocol = crate::provider::UpstreamProtocolKind::from(item.protocol);
        if !model.protocols.contains_key(protocol.as_str()) {
            return Err(V3ApiError::invalid_request_at(
                state,
                "protocol is outside this provider's documented capability ceiling",
            ));
        }
    }
    Ok(())
}

/// V3 route: rewrite the current catalog to official-docs or snapshot
/// protocols. The dashboard no longer exposes this; catalog refresh writes
/// the same evidence. OpenCode Go, Zen Free, and Command Code fetch official
/// docs; missing models supply no new evidence. Snapshot providers stay local.
pub(super) async fn reset_provider_model_protocols_to_static(
    State(state): State<CoreState>,
    Path(scope_id): Path<String>,
    body: Bytes,
) -> Result<Json<ProviderContracts>, V3ApiError> {
    let expectation = parse_mutation_json::<MutationExpectation>(&body)?;
    let docs_baseline = crate::official_protocols::uses_official_docs_protocol_baseline(&scope_id);
    let (scope, models, config) = {
        let _settings_update = state.settings_update.lock();
        check_expectation(&state, &expectation)?;
        if provider_contracts::static_protocol_snapshot_date(&scope_id).is_none() && !docs_baseline
        {
            return Err(V3ApiError::invalid_request_at(
                &state,
                "this provider does not support restoring an official protocol baseline",
            ));
        }
        let scope = ContractScope::parse(provider_contracts::SCOPE_KIND_PROVIDER, &scope_id)
            .map_err(|message| V3ApiError::invalid_request_at(&state, message))?;
        validate_provider_scope(&state, &scope)?;
        let models = state
            .provider_contracts()
            .scope(&scope)
            .map(|contract| contract.catalog.models.to_vec())
            .ok_or_else(|| V3ApiError::not_found_at(&state, "provider scope not found"))?;
        (scope, models, state.config())
    };
    let official = if docs_baseline {
        Some(
            crate::official_protocols::fetch_official_protocol_baseline(
                &config,
                &scope_id,
                state.process_generation(),
            )
            .await,
        )
    } else {
        None
    };
    let now = Utc::now();
    let revision = {
        let _settings_update = state.settings_update.lock();
        check_expectation(&state, &expectation)?;
        let db = state.db.lock();
        if let Some(baseline) = official.as_ref() {
            db.reset_provider_docs_model_protocols(&scope, &models, baseline, now)
                .map_err(V3ApiError::internal)?;
        } else {
            db.reset_provider_static_model_protocols(&scope, &models, now)
                .map_err(V3ApiError::internal)?;
        }
        // The reset transaction is already durable. Advance CAS before the
        // fallible reload so persisted state can never hide behind an old token.
        let revision = state.bump_settings_revision();
        state
            .reload_provider_contracts_locked(&db)
            .map_err(V3ApiError::internal)?;
        revision
    };
    state.routing.reset();
    state.log_runtime_event(
        "info",
        "provider",
        &format!(
            "event=provider_protocols_reset provider={scope_id} model_count={} revision={revision}",
            models.len()
        ),
    );
    provider_contracts_response(&state)
}

pub(super) async fn put_custom_endpoint_model_protocol_overrides(
    State(state): State<CoreState>,
    Path(scope_id): Path<String>,
    body: Bytes,
) -> Result<Json<ProviderContracts>, V3ApiError> {
    let input = parse_mutation_json::<ModelProtocolOverridesUpdate>(&body)?;
    let _settings_update = state.settings_update.lock();
    check_expectation(&state, &input.expectation)?;
    let scope = ContractScope::parse(provider_contracts::SCOPE_KIND_CUSTOM_ENDPOINT, &scope_id)
        .map_err(|message| V3ApiError::invalid_request_at(&state, message))?;
    validate_custom_endpoint_scope(&state, &scope)?;
    let ContractScope::CustomEndpoint(account_id) = &scope else {
        unreachable!("custom endpoint scope was validated above");
    };
    let declared_protocol = state
        .db
        .lock()
        .account_custom_config(account_id)
        .map_err(V3ApiError::internal)?
        .ok_or_else(|| V3ApiError::not_found_at(&state, "custom endpoint config not found"))?
        .upstream_protocol;
    if input
        .overrides
        .iter()
        .any(|item| crate::provider::UpstreamProtocolKind::from(item.protocol) != declared_protocol)
    {
        return Err(V3ApiError::invalid_request_at(
            &state,
            "custom endpoint overrides must use the account's declared upstream protocol",
        ));
    }
    commit_model_protocol_overrides(&state, &scope, input.overrides)
}

fn commit_model_protocol_overrides(
    state: &CoreState,
    scope: &ContractScope,
    overrides: Vec<ModelProtocolOverride>,
) -> Result<Json<ProviderContracts>, V3ApiError> {
    if overrides.is_empty() {
        return Err(V3ApiError::invalid_request_at(
            state,
            "override batch must be nonempty",
        ));
    }
    let mut rows = Vec::with_capacity(overrides.len());
    let mut preferences = Vec::new();
    let mut preferred_models = std::collections::HashSet::new();
    for item in overrides {
        let protocol = crate::provider::UpstreamProtocolKind::from(item.protocol);
        if item.preferred == Some(true) {
            let on_model = state
                .provider_contracts()
                .scope(scope)
                .and_then(|contract| contract.model(&item.model_id))
                .is_some_and(|model| model.protocols.contains_key(protocol.as_str()));
            if scope.kind_str() != "provider"
                || !provider_contracts::selectable_model_protocol(scope.id(), protocol)
                || !on_model
                || !preferred_models.insert(item.model_id.trim().to_ascii_lowercase())
            {
                return Err(V3ApiError::invalid_request_at(
                    state,
                    "one preferred protocol per model is allowed",
                ));
            }
            preferences.push((item.model_id.trim().to_ascii_lowercase(), protocol));
        }
        rows.push((
            item.model_id,
            protocol,
            override_state_to_domain(item.state),
        ));
    }
    let now = Utc::now();
    {
        let db = state.db.lock();
        db.set_model_protocol_settings(scope, &rows, &preferences, now)
            .map_err(V3ApiError::internal)?;
        state
            .reload_provider_contracts_locked(&db)
            .map_err(V3ApiError::internal)?;
    }
    state.routing.reset();
    let _revision = state.bump_settings_revision();
    let contracts = state.provider_contracts();
    let (accounts, statuses) = load_accounts_with_verification(state)?;
    Ok(Json(provider_contracts_from_state(
        state, &contracts, &accounts, &statuses,
    )))
}

fn override_state_to_domain(state: ProtocolOverrideState) -> DomainProtocolOverrideState {
    match state {
        ProtocolOverrideState::Auto => DomainProtocolOverrideState::Auto,
        ProtocolOverrideState::ForceOn => DomainProtocolOverrideState::ForceOn,
        ProtocolOverrideState::ForceOff => DomainProtocolOverrideState::ForceOff,
    }
}

pub(super) async fn run_provider_protocol_probes(
    State(state): State<CoreState>,
    Path(provider_id): Path<String>,
    body: Bytes,
) -> Result<Json<ProtocolProbeResponse>, V3ApiError> {
    let input = parse_mutation_json::<ProtocolProbeRequest>(&body)?;
    let prepared = {
        let _settings_update = state.settings_update.lock();
        check_expectation(&state, &input.expectation)?;
        prepare_protocol_probe(&state, &provider_id, &input)?
    };
    let outcomes = protocol_probe::run_protocol_probes(
        &ProtocolProbeContext {
            state: &state,
            config: &prepared.config,
            accounts: &prepared.accounts,
            adapter: prepared.adapter,
            model_id: &prepared.model_id,
            custom_route: None,
            now: prepared.now,
        },
        &prepared.scope,
        &prepared.protocols,
        |protocol| Ok(prepared.existing.get(&protocol).cloned().flatten()),
    )
    .await
    .map_err(|error| match error {
        ProtocolProbeRunError::Apply(message) => V3ApiError::invalid_request_at(&state, message),
        ProtocolProbeRunError::Evidence(message) => V3ApiError::internal(message),
    })?;
    log_protocol_probe_requests(&state, &prepared, &outcomes);
    let observations: Vec<_> = outcomes
        .iter()
        .filter_map(|outcome| outcome.observation.clone())
        .collect();
    // Connection tests record observations only. Protocol enablement and
    // preference are configuration, never a side effect of testing.
    let overrides = Vec::new();
    let _settings_update = state.settings_update.lock();
    check_expectation(&state, &prepared.expectation)?;
    ensure_probe_model_is_current(
        &state,
        &prepared.scope,
        &prepared.provider_id,
        &prepared.model_id,
    )?;
    persist_probe_results(
        &state,
        &prepared.scope,
        &observations,
        &overrides,
        prepared.now,
    )?;
    let revision = ControlRevision::from_state(&state);
    let contract = state
        .provider_contracts()
        .scope(&prepared.scope)
        .and_then(|scope| scope.model(&prepared.model_id).cloned())
        .map(|model| model_contract_from_domain(&model, model.model_id.clone()));
    Ok(Json(ProtocolProbeResponse {
        account_id: None,
        provider_id: prepared.provider_id,
        model_id: prepared.model_id.clone(),
        results: outcomes
            .into_iter()
            .map(|outcome| ProtocolProbeResult {
                protocol: AccountUpstreamProtocol::from(outcome.protocol),
                success: outcome.success,
                skipped: outcome.skipped,
                error: outcome.error,
            })
            .collect(),
        contract,
        revision: revision.revision,
        process_generation: revision.process_generation,
        pricing_revision: revision.pricing_revision,
    }))
}

fn persist_probe_results(
    state: &CoreState,
    scope: &ContractScope,
    observations: &[PersistedModelProtocol],
    overrides: &[(
        String,
        crate::provider::UpstreamProtocolKind,
        DomainProtocolOverrideState,
    )],
    now: DateTime<Utc>,
) -> Result<(), V3ApiError> {
    if observations.is_empty() && overrides.is_empty() {
        return Ok(());
    }
    {
        let db = state.db.lock();
        db.commit_model_protocol_probe_results(scope, observations, overrides, now)
            .map_err(V3ApiError::internal)?;
        // Advance CAS immediately after commit so a later reload/read
        // failure cannot hide the persisted mutation behind an unchanged token.
        let _revision = state.bump_settings_revision();
        state
            .reload_provider_contracts_locked(&db)
            .map_err(V3ApiError::internal)?;
    }
    state.routing.reset();
    Ok(())
}

fn log_protocol_probe_requests(
    state: &CoreState,
    prepared: &PreparedProtocolProbe,
    outcomes: &[protocol_probe::ProtocolProbeOutcome],
) {
    let request_id = format!("ocg-probe-{}", uuid::Uuid::new_v4());
    let db = state.db.lock();
    let mut attempt_number = 0_i64;
    for outcome in outcomes {
        for attempt in &outcome.attempts {
            attempt_number += 1;
            let Some(account) = prepared
                .accounts
                .iter()
                .find(|account| account.id == attempt.account_id)
            else {
                continue;
            };
            let result = if attempt.success {
                "succeeded"
            } else {
                "failed"
            };
            let protocol = attempt.protocol.as_str();
            let diagnostic = serde_json::json!({
                "event": "protocol_probe",
                "outcome": result,
                "attempt": attempt_number,
                "duration_ms": attempt.duration_ms,
                "provider_id": prepared.provider_id,
                "account_id": account.id,
                "model_id": prepared.model_id,
                "client_format": protocol,
                "upstream_format": protocol,
                "upstream_error": attempt.error });
            let log = ForwardLog {
                id: 0,
                timestamp: prepared.now,
                model: prepared.model_id.clone(),
                account_id: account.id.clone(),
                account_name: account.name.clone(),
                route_account_id: Some(account.id.clone()),
                provider_id: Some(prepared.provider_id.clone()),

                credential_account_id: (prepared.adapter != ProviderAdapterKind::ZenFree)
                    .then(|| account.id.clone()),
                client_key_id: None,
                client_key_name: None,
                status: if attempt.success { "success" } else { "error" }.to_string(),
                http_status: attempt.http_status,
                route: String::new(),
                prompt_tokens: 0,
                completion_tokens: 0,
                cached_tokens: 0,
                cache_creation_tokens: 0,
                cost: None,
                raw_cost_usd: None,
                quota_debit: None,
                effective_paid_cost_usd: None,
                pricing_revision_id: None,
                quota_multiplier: None,
                local_adjustment_multiplier: None,
                service_tier: None,
                cost_state: "not_applicable".to_string(),
                error_message: attempt.error.clone(),
                request_id: Some(request_id.clone()),
                attempt: Some(attempt_number),
                error_source: None,
                error_stage: (!attempt.success).then(|| "protocol_probe".to_string()),
                duration_ms: Some(attempt.duration_ms),
                diagnostic: Some(diagnostic),
            };
            if let Err(error) = db.log_forward(&log) {
                let _ = db.log_gateway(
                    "error",
                    "observability",
                    "Failed to persist a protocol probe request log.",
                );
                eprintln!("failed to persist protocol probe request log: {error}");
            }
        }
    }
}

struct PreparedProtocolProbe {
    provider_id: String,
    accounts: Vec<ModelAccount>,
    adapter: ProviderAdapterKind,
    config: AppConfig,
    scope: ContractScope,
    model_id: String,
    protocols: Vec<crate::provider::UpstreamProtocolKind>,
    existing: HashMap<crate::provider::UpstreamProtocolKind, Option<PersistedModelProtocol>>,
    expectation: MutationExpectation,
    now: DateTime<Utc>,
}

fn ensure_probe_model_is_current(
    state: &CoreState,
    scope: &ContractScope,
    provider_id: &str,
    model_id: &str,
) -> Result<(), V3ApiError> {
    let contracts = state.provider_contracts();
    let catalog_contains_model = contracts.scope(scope).is_some_and(|contract| {
        contract.catalog.models.iter().any(|id| id == model_id)
            && !(provider_id == OPENCODE_PROVIDER_ID && is_free_model(model_id))
    });
    if catalog_contains_model {
        Ok(())
    } else {
        Err(V3ApiError::invalid_request_at(
            state,
            "modelId is not present in the current provider catalog",
        ))
    }
}

fn prepare_protocol_probe(
    state: &CoreState,
    provider_id: &str,
    input: &ProtocolProbeRequest,
) -> Result<PreparedProtocolProbe, V3ApiError> {
    if provider_id == CUSTOM_PROVIDER_ID {
        return Err(V3ApiError::invalid_request_at(
            state,
            "protocol probes for Custom API are account-owned",
        ));
    }
    let descriptor = provider_contracts::provider_scope_descriptor(provider_id)
        .ok_or_else(|| V3ApiError::not_found_at(state, "provider not found"))?;
    if !descriptor.card_actions.protocol_probe {
        return Err(V3ApiError::not_implemented(
            state,
            "protocol probes are not available for this Plan in this slice",
        ));
    }
    let adapter = descriptor.kind;
    let model_id = input.model_id.trim();
    if model_id.is_empty() {
        return Err(V3ApiError::invalid_request_at(state, "modelId is required"));
    }
    if input.protocols.is_empty() {
        return Err(V3ApiError::invalid_request_at(
            state,
            "at least one explicit upstream protocol is required",
        ));
    }
    let scope = ContractScope::provider(provider_id);
    ensure_probe_model_is_current(state, &scope, provider_id, model_id)?;
    let requested_protocols: Vec<_> = input
        .protocols
        .iter()
        .copied()
        .map(crate::provider::UpstreamProtocolKind::from)
        .collect();
    protocol_probe::require_unique_probe_protocols(&requested_protocols)
        .map_err(|message| V3ApiError::invalid_request_at(state, message))?;
    let probeable = state
        .provider_contracts()
        .scope(&scope)
        .and_then(|contract| contract.model(model_id))
        .map(|model| model.protocols.keys().cloned().collect::<HashSet<_>>())
        .unwrap_or_default();
    let protocols = requested_protocols
        .into_iter()
        .filter(|protocol| probeable.contains(protocol.as_str()))
        .collect::<Vec<_>>();
    if protocols.is_empty() {
        return Err(V3ApiError::invalid_request_at(
            state,
            "none of the requested protocols are probeable for this model",
        ));
    }
    let now = Utc::now();
    let all_accounts = state
        .db
        .lock()
        .list_accounts()
        .map_err(V3ApiError::internal)?;
    let channel = if adapter == ProviderAdapterKind::ZenFree {
        UpstreamChannel::Free
    } else {
        UpstreamChannel::Go
    };
    let mut accounts: Vec<_> = all_accounts
        .iter()
        .filter(|account| {
            account.provider_id == provider_id
                && !matches!(
                    adapter,
                    ProviderAdapterKind::ConfigurableHttp | ProviderAdapterKind::OllamaCloud
                )
                && account_is_available_for_at(account, channel, &[], now)
        })
        .cloned()
        .collect();
    if channel == UpstreamChannel::Free && free_channel_is_exhausted_at(&all_accounts, now) {
        accounts.clear();
    }
    if accounts.is_empty() {
        return Err(V3ApiError::invalid_request_at(
            state,
            "no eligible provider accounts are available for protocol probes",
        ));
    }
    let mut existing = HashMap::new();
    {
        let db = state.db.lock();
        for protocol in &protocols {
            existing.insert(
                *protocol,
                db.load_model_protocol(&scope, model_id, *protocol)
                    .map_err(V3ApiError::internal)?,
            );
        }
    }
    Ok(PreparedProtocolProbe {
        provider_id: provider_id.to_string(),
        accounts,
        adapter,
        config: state.config(),
        scope,
        model_id: model_id.to_string(),
        protocols,
        existing,
        expectation: input.expectation.clone(),
        now,
    })
}

fn validate_provider_scope(state: &CoreState, scope: &ContractScope) -> Result<(), V3ApiError> {
    match scope {
        ContractScope::Provider(provider_id)
            if provider_contracts::builtin_provider_scope_ids().contains(&provider_id.as_str()) =>
        {
            Ok(())
        }
        ContractScope::Provider(_) => Err(V3ApiError::not_found_at(
            state,
            "provider contract scope not found",
        )),
        ContractScope::CustomEndpoint(_) => Err(V3ApiError::invalid_request_at(
            state,
            "protocol switches on this path are limited to provider scopes",
        )),
    }
}

fn audit_catalog_success(state: &CoreState, provider_id: &str, model_count: usize, revision: u64) {
    state.log_runtime_event(
        "info",
        "provider",
        &format!(
            "event=provider_catalog_refresh_succeeded provider={provider_id} model_count={model_count} revision={revision}"
        ),
    );
}

fn audit_catalog_failure(state: &CoreState, provider_id: &str, stage: &str) {
    state.log_runtime_event(
        "warn",
        "provider",
        &format!("event=provider_catalog_refresh_failed provider={provider_id} stage={stage}"),
    );
}

fn validate_custom_endpoint_scope(
    state: &CoreState,
    scope: &ContractScope,
) -> Result<(), V3ApiError> {
    let ContractScope::CustomEndpoint(account_id) = scope else {
        return Err(V3ApiError::invalid_request_at(
            state,
            "protocol switches on this path are limited to custom endpoint scopes",
        ));
    };
    let account = load_model_account(state, account_id)?;
    let plan = crate::provider::builtin_provider(&account.provider_id).ok_or_else(|| {
        V3ApiError::not_found_at(state, "custom endpoint contract scope not found")
    })?;
    if crate::provider::plan_requires_custom_config(plan) {
        Ok(())
    } else {
        Err(V3ApiError::not_found_at(
            state,
            "custom endpoint contract scope not found",
        ))
    }
}

fn provider_catalog_from_state(state: &CoreState) -> ProviderCatalog {
    let revision = ControlRevision::from_state(state);
    let zen_catalog = state.zen_free_model_catalog();
    let contracts = state.provider_contracts();
    let goat_models = contracts
        .providers
        .get(COMMAND_CODE_PROVIDER_ID)
        .map(|scope| scope.catalog.models.as_slice())
        .unwrap_or_default();
    let minimax_models = contracts
        .providers
        .get(MINIMAX_PROVIDER_ID)
        .map(|scope| scope.catalog.models.as_slice())
        .unwrap_or_default();
    let kimi_models = contracts
        .providers
        .get(KIMI_PROVIDER_ID)
        .map(|scope| scope.catalog.models.as_slice())
        .unwrap_or_default();
    let ollama_models = contracts
        .providers
        .get(OLLAMA_PROVIDER_ID)
        .map(|scope| scope.catalog.models.as_slice())
        .unwrap_or_default();
    let ollama_pinned_models = provider_contracts::ollama_cloud_pinned_model_ids(&contracts);
    let go_models = contracts
        .providers
        .get(OPENCODE_PROVIDER_ID)
        .map(|scope| scope.catalog.models.as_slice())
        .unwrap_or_default();
    let public_catalogs = alias::RuntimeCatalogs {
        go: go_models,
        zen_free: &zen_catalog.models,
        command_code: goat_models,
        minimax: minimax_models,
        kimi: kimi_models,
        ollama: ollama_models,
        ollama_pinned: &ollama_pinned_models,
        ..alias::RuntimeCatalogs::default()
    };
    let mut entries: Vec<ProviderCatalogEntry> = BUILTIN_PROVIDERS
        .iter()
        .filter(|plan| !plan.product_surface.is_external_integration())
        .map(|plan| {
            let mut entry = catalog_entry(
                plan,
                &zen_catalog.models,
                goat_models,
                minimax_models,
                kimi_models,
                ollama_models,
                &ollama_pinned_models,
            );
            if plan.provider_id == OPENCODE_PROVIDER_ID {
                entry.model_aliases = alias::routeable_models_for_with_runtime_catalogs(
                    plan.provider_id,
                    public_catalogs,
                );
            }
            entry
        })
        .collect();
    for runtime in state.dynamic_providers().iter() {
        entries.push(dynamic_catalog_entry(runtime));
    }
    ProviderCatalog {
        entries,
        revision: revision.revision,
        process_generation: revision.process_generation,
        pricing_revision: revision.pricing_revision,
    }
}

fn dynamic_catalog_entry(runtime: &crate::dynamic::DynamicProviderRuntime) -> ProviderCatalogEntry {
    let auth_schemes = match runtime.auth_kind.upstream_auth() {
        Some(scheme) => vec![AccountAuthScheme::from(scheme)],
        None => Vec::new(),
    };
    let editable = !matches!(runtime.origin, ProviderOrigin::Builtin);
    ProviderCatalogEntry {
        provider_id: runtime.id.clone(),
        origin: runtime.origin,
        editable,
        deletable: editable,
        offering: runtime.offering.clone(),
        display_name: runtime.name.clone(),
        display_family: runtime.name.clone(),
        credential_kind: runtime.auth_kind.credential_kind().into(),
        quota_scope: AccountQuotaScope::from(runtime.auth_kind.quota_scope()),
        singleton: runtime.auth_kind.is_singleton(),
        creation_availability: if runtime.auth_kind.is_singleton() {
            "unavailable".into()
        } else {
            "available".into()
        },
        creation_unavailable_reason: runtime
            .auth_kind
            .is_singleton()
            .then(|| "no-auth providers own a singleton account".to_string()),
        verification_policy: "not_required".into(),
        verification_runtime_availability: "not_applicable".into(),
        routable: true,
        managed_registration: false,
        pricing_availability: if crate::official_api::kind_for_runtime(runtime).is_some() {
            "available"
        } else {
            "unpriced"
        }
        .into(),
        usage_availability: "unavailable".into(),
        manual_usage_calibration: false,
        quota_unit: "none".into(),
        model_source: if crate::official_api::kind_for_runtime(runtime).is_some() {
            "official_api_preset"
        } else {
            "dynamic_provider"
        }
        .into(),
        key_prefix: None,
        auth_schemes,
        upstream_protocols: {
            let mut protocols = vec![AccountUpstreamProtocol::from(runtime.upstream_protocol)];
            for mapping in &runtime.mappings {
                if let Some(value) = &mapping.upstream_override {
                    let protocol = AccountUpstreamProtocol::from(value.protocol);
                    if !protocols.contains(&protocol) {
                        protocols.push(protocol);
                    }
                }
            }
            protocols
        },
        form_fields: {
            let mut fields = vec![ProviderCatalogFormField {
                id: "name".into(),
                kind: "text".into(),
                required: true,
                immutable_after_create: false,
            }];
            if runtime.auth_kind.requires_key() {
                fields.push(ProviderCatalogFormField {
                    id: "key".into(),
                    kind: "secret".into(),
                    required: true,
                    immutable_after_create: false,
                });
            }
            fields.push(ProviderCatalogFormField {
                id: "notes".into(),
                kind: "text".into(),
                required: false,
                immutable_after_create: false,
            });
            fields
        },
        model_aliases: runtime
            .mappings
            .iter()
            .map(|mapping| mapping.public_model.clone())
            .collect(),
    }
}

fn catalog_entry(
    plan: &BuiltinProvider,
    zen_models: &[String],
    goat_models: &[String],
    minimax_models: &[String],
    kimi_models: &[String],
    ollama_models: &[String],
    ollama_pinned_models: &[String],
) -> ProviderCatalogEntry {
    ProviderCatalogEntry {
        provider_id: plan.provider_id.to_string(),

        origin: ProviderOrigin::Builtin,
        editable: false,
        deletable: false,
        offering: builtin_offering(plan.provider_id).to_string(),
        display_name: plan.display_name.to_string(),
        display_family: plan.display_family.to_string(),
        credential_kind: plan.credential_kind.into(),
        quota_scope: AccountQuotaScope::from(plan.quota_scope),
        singleton: plan.singleton_account_id.is_some(),
        creation_availability: plan.creation_availability.as_str().to_string(),
        creation_unavailable_reason: plan.creation_unavailable_reason.map(str::to_string),
        verification_policy: plan.verification_policy.as_str().to_string(),
        verification_runtime_availability: plan.verification_runtime_availability.to_string(),
        routable: plan.routable,
        managed_registration: plan.managed_registration,
        pricing_availability: plan.pricing_availability.to_string(),
        usage_availability: plan.usage_availability.to_string(),
        manual_usage_calibration: plan.manual_usage_calibration,
        quota_unit: plan.quota_unit.to_string(),
        model_source: plan.model_source.to_string(),
        key_prefix: plan.key_prefix.map(str::to_string),
        auth_schemes: plan
            .auth_schemes
            .iter()
            .copied()
            .map(AccountAuthScheme::from)
            .collect(),
        upstream_protocols: plan
            .upstream_protocols
            .iter()
            .copied()
            .map(AccountUpstreamProtocol::from)
            .collect(),
        form_fields: plan
            .form_fields
            .iter()
            .map(|field| ProviderCatalogFormField {
                id: field.id.to_string(),
                kind: field.kind.to_string(),
                required: field.required,
                immutable_after_create: field.immutable_after_create,
            })
            .collect(),
        model_aliases: alias::routeable_aliases_for_with_runtime_catalogs(
            plan.provider_id,
            alias::RuntimeCatalogs {
                zen_free: zen_models,
                command_code: goat_models,
                minimax: minimax_models,
                kimi: kimi_models,
                ollama: ollama_models,
                ollama_pinned: ollama_pinned_models,
                ..alias::RuntimeCatalogs::default()
            },
        ),
    }
}

fn model_capabilities(contracts: &EffectiveContractSet) -> Vec<ProviderModelCapability> {
    contracts
        .providers
        .get(OPENCODE_PROVIDER_ID)
        .into_iter()
        .flat_map(|scope| scope.models.values())
        .map(|model| ProviderModelCapability {
            model_id: model.model_id.clone(),
            provider_id: OPENCODE_PROVIDER_ID.to_string(),
            preferred_protocol: AccountUpstreamProtocol::from(model.preferred_protocol),
            supported_protocols: model
                .protocols
                .values()
                .filter(|row| row.available)
                .map(|row| AccountUpstreamProtocol::from(row.protocol))
                .collect(),
        })
        .collect()
}

fn zen_free_settings_from_state(state: &CoreState) -> Result<ZenFreeSettings, V3ApiError> {
    let account = state
        .db
        .lock()
        .get_account(ZEN_FREE_ACCOUNT_ID)
        .map_err(V3ApiError::internal)?
        .ok_or_else(|| V3ApiError::internal("Zen Free singleton is missing"))?;
    let revision = ControlRevision::from_state(state);
    Ok(ZenFreeSettings {
        account_id: account.id,
        enabled: account.enabled,
        revision: revision.revision,
        process_generation: revision.process_generation,
        pricing_revision: revision.pricing_revision,
    })
}

fn zen_free_models_from_state(state: &CoreState) -> ZenFreeModels {
    let catalog = state.zen_free_model_catalog();
    let revision = ControlRevision::from_state(state);
    ZenFreeModels {
        account_id: ZEN_FREE_ACCOUNT_ID.to_string(),
        models: model_views(&catalog)
            .into_iter()
            .map(|model| ZenFreeModel {
                model_id: model.model_id,
                alias: model.alias,
            })
            .collect(),
        refreshed_at: catalog.refreshed_at.map(|value| value.to_rfc3339()),
        source_url: catalog.source_url.clone(),
        revision: revision.revision,
        process_generation: revision.process_generation,
        pricing_revision: revision.pricing_revision,
    }
}

async fn fetch_zen_free_catalog(
    state: &CoreState,
    config: &crate::models::AppConfig,
) -> Result<ZenFreeModelCatalog, String> {
    #[cfg(debug_assertions)]
    if let Some(url) = debug_zen_models_source_url(state.process_generation()) {
        let mut catalog = fetch_zen_catalog_at(config, &url).await?;
        catalog.source_url = ZEN_MODELS_SOURCE_URL.to_string();
        return Ok(catalog);
    }
    #[cfg(not(debug_assertions))]
    let _ = state;
    crate::zen_models::fetch_catalog(config).await
}

#[cfg(debug_assertions)]
async fn fetch_zen_catalog_at(
    config: &crate::models::AppConfig,
    source_url: &str,
) -> Result<ZenFreeModelCatalog, String> {
    let client = crate::http_client::build_no_redirect(config)
        .map_err(|error| format!("failed to build Zen model catalog client: {error}"))?;
    let timeout = Duration::from_secs(
        config
            .non_stream_timeout_secs
            .clamp(5, ZEN_REFRESH_TIMEOUT_SECS),
    );
    let response = tokio::time::timeout(
        Duration::from_secs(ZEN_REFRESH_TIMEOUT_SECS),
        client
            .get(source_url)
            .header(reqwest::header::ACCEPT, "application/json")
            .timeout(timeout)
            .send(),
    )
    .await
    .map_err(|_| "Zen model catalog refresh timed out".to_string())?
    .map_err(|error| format!("Zen model catalog request failed: {error}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!(
            "Zen model catalog upstream returned HTTP {}",
            status.as_u16()
        ));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_ZEN_BODY_BYTES as u64)
    {
        return Err("Zen model catalog response is too large".to_string());
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| format!("Zen model catalog body failed: {error}"))?;
        if body.len().saturating_add(chunk.len()) > MAX_ZEN_BODY_BYTES {
            return Err("Zen model catalog response is too large".to_string());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(ZenFreeModelCatalog {
        models: parse_catalog(&body)?,
        refreshed_at: Some(Utc::now()),
        source_url: source_url.to_string(),
    })
}

fn load_accounts_with_verification(
    state: &CoreState,
) -> Result<
    (
        Vec<ModelAccount>,
        HashMap<String, ConnectionVerificationStatus>,
    ),
    V3ApiError,
> {
    let db = state.db.lock();
    let accounts = db.list_accounts().map_err(V3ApiError::internal)?;
    let mut statuses = HashMap::new();
    for account in &accounts {
        if let Some(verification) = db
            .account_verification_state(&account.id)
            .map_err(V3ApiError::internal)?
        {
            statuses.insert(account.id.clone(), verification.status);
        }
    }
    Ok((accounts, statuses))
}

fn provider_contracts_from_state(
    state: &CoreState,
    contracts: &EffectiveContractSet,
    accounts: &[ModelAccount],
    statuses: &HashMap<String, ConnectionVerificationStatus>,
) -> ProviderContracts {
    let revision = ControlRevision::from_state(state);
    let mut providers = Vec::new();
    for scope_id in provider_contracts::builtin_provider_scope_ids() {
        let Some(contract) = contracts.providers.get(scope_id) else {
            continue;
        };
        let descriptor = provider_contracts::provider_scope_descriptor(scope_id)
            .expect("provider contract scope has an exact descriptor");
        let plan = crate::provider::builtin_provider(descriptor.provider_id)
            .expect("provider contract descriptor has a catalog plan");
        let group_accounts = accounts
            .iter()
            .filter(|account| account.provider_id == plan.provider_id)
            .map(|account| account_choice(account, statuses))
            .collect();
        let mut catalog = catalog_from_domain(&contract.catalog);
        let mut models: Vec<EffectiveModelContract> = contract
            .models
            .values()
            .map(|model| model_contract_from_provider(descriptor.provider_id, model, contracts))
            .collect();
        if descriptor.provider_id == OPENCODE_PROVIDER_ID {
            // Presentation-only filter: Zen Free owns every `-free` id, so the
            // Go scope must not project them even when a persisted catalog row
            // still contains them. The effective contract set used by gateway
            // routing is left untouched.
            catalog.models.retain(|id| !is_free_model(id));
            models.retain(|model| !is_free_model(&model.model_id));
        }
        providers.push(ProviderContractGroup {
            scope_kind: ContractScopeKind::Provider,
            scope_id: scope_id.to_string(),
            provider_id: descriptor.provider_id.to_string(),
            static_protocol_snapshot_date: provider_contracts::static_protocol_snapshot_date(
                scope_id,
            )
            .map(str::to_string),
            accounts: group_accounts,
            catalog,
            models,
            pricing: CapabilitySummary {
                availability: descriptor.pricing.availability.to_string(),
            },
            usage: CapabilitySummary {
                availability: descriptor.usage.catalog_availability.to_string(),
            },
            card: card_summary(descriptor),
            catalog_routable: contract.catalog_routable,
            production_inference: contract.production_inference,
            disabled_reasons: contract.disabled_reasons.clone(),
            revision: contract.revision,
        });
    }
    let custom_endpoints = contracts
        .custom_endpoints
        .values()
        .map(|contract| {
            let descriptor =
                ProviderRegistry::get(CUSTOM_PROVIDER_ID).expect("custom offering is registered");
            let account = accounts
                .iter()
                .find(|account| account.id == contract.scope.id())
                .map(|account| account_choice(account, statuses))
                .unwrap_or(ProviderAccountChoice {
                    id: contract.scope.id().to_string(),
                    name: contract.scope.id().to_string(),
                    enabled: false,
                    verification_status: AccountVerificationStatus::Pending,
                });
            CustomEndpointContract {
                scope_kind: ContractScopeKind::CustomEndpoint,
                scope_id: contract.scope.id().to_string(),
                provider_id: CUSTOM_PROVIDER_ID.to_string(),
                account,
                catalog: catalog_from_domain(&contract.catalog),
                models: contract
                    .models
                    .values()
                    .map(|model| model_contract_from_domain(model, model.model_id.clone()))
                    .collect(),
                pricing: CapabilitySummary {
                    availability: descriptor.pricing.availability.to_string(),
                },
                usage: CapabilitySummary {
                    availability: descriptor.usage.catalog_availability.to_string(),
                },
                card: card_summary(descriptor),
                catalog_routable: contract.catalog_routable,
                production_inference: contract.production_inference,
                disabled_reasons: contract.disabled_reasons.clone(),
                revision: contract.revision,
            }
        })
        .collect();
    ProviderContracts {
        providers,
        custom_endpoints,
        revision: revision.revision,
        process_generation: revision.process_generation,
        pricing_revision: revision.pricing_revision,
    }
}

fn account_choice(
    account: &ModelAccount,
    statuses: &HashMap<String, ConnectionVerificationStatus>,
) -> ProviderAccountChoice {
    let verification_status = statuses
        .get(&account.id)
        .copied()
        .or_else(|| {
            crate::provider::builtin_provider(&account.provider_id).map(default_verification_status)
        })
        .unwrap_or(ConnectionVerificationStatus::NotRequired);
    ProviderAccountChoice {
        id: account.id.clone(),
        name: account.name.clone(),
        enabled: account.enabled,
        verification_status: AccountVerificationStatus::from(verification_status),
    }
}

fn card_summary(descriptor: crate::provider::ProviderDescriptor) -> CardCapabilitySummary {
    CardCapabilitySummary {
        fetch_zen_models: descriptor.card_actions.fetch_zen_models,
        discover_models: descriptor.card_actions.discover_models,
        protocol_probe: descriptor.card_actions.protocol_probe,
        catalog_refresh: descriptor.card_actions.catalog_refresh,
    }
}

fn catalog_from_domain(catalog: &provider_contracts::EffectiveCatalog) -> EffectiveCatalog {
    EffectiveCatalog {
        source: catalog.source.clone(),
        source_url: catalog.source_url.clone(),
        refreshed_at: catalog.refreshed_at.map(|value| value.to_rfc3339()),
        models: catalog.models.clone(),
        refresh_supported: catalog.refresh_supported,
    }
}

fn model_contract_from_provider(
    provider_id: &str,
    model: &DomainModelContract,
    contracts: &EffectiveContractSet,
) -> EffectiveModelContract {
    let go_models = contracts
        .providers
        .get(OPENCODE_PROVIDER_ID)
        .map(|scope| scope.catalog.models.as_slice())
        .unwrap_or_default();
    let zen_models = contracts
        .providers
        .get(OPENCODE_ZEN_FREE_PROVIDER_ID)
        .map(|scope| scope.catalog.models.as_slice())
        .unwrap_or_default();
    let alias = crate::alias::canonical_alias_for_provider_model(
        provider_id,
        &model.model_id,
        go_models,
        zen_models,
    );
    model_contract_from_domain(model, alias)
}

fn model_contract_from_domain(
    model: &DomainModelContract,
    alias: String,
) -> EffectiveModelContract {
    EffectiveModelContract {
        alias,
        model_id: model.model_id.clone(),
        preferred_protocol: AccountUpstreamProtocol::from(model.preferred_protocol),
        protocols: model_protocols_from_domain(&model.protocols),
        routable: model.routable,
        disabled_reasons: model.disabled_reasons.clone(),
    }
}

fn model_protocols_from_domain(
    map: &std::collections::BTreeMap<String, DomainProtocolEvidence>,
) -> EffectiveModelProtocols {
    let mut protocols = EffectiveModelProtocols {
        chat_completions: None,
        responses: None,
        messages: None,
    };
    for row in map.values() {
        let evidence = evidence_from_domain(row);
        match row.protocol {
            crate::provider::UpstreamProtocolKind::ChatCompletions => {
                protocols.chat_completions = Some(evidence);
            }
            crate::provider::UpstreamProtocolKind::Responses => {
                protocols.responses = Some(evidence);
            }
            crate::provider::UpstreamProtocolKind::Messages => {
                protocols.messages = Some(evidence);
            }
        }
    }
    protocols
}

fn evidence_from_domain(row: &DomainProtocolEvidence) -> EffectiveProtocolEvidence {
    EffectiveProtocolEvidence {
        protocol: AccountUpstreamProtocol::from(row.protocol),
        available: row.available,
        enabled: row.enabled,
        source: ContractEvidenceSource::from(row.source),
        verified_at: row.verified_at.map(|value| value.to_rfc3339()),
        observed_at: row.observed_at.map(|value| value.to_rfc3339()),
        last_probe_result: row.last_probe_result.map(ProbeResultKind::from),
        last_probe_at: row.last_probe_at.map(|value| value.to_rfc3339()),
        last_probe_error: row.last_probe_error.clone(),
        r#override: override_state_from_domain(row.r#override),
    }
}

fn override_state_from_domain(state: DomainProtocolOverrideState) -> ProtocolOverrideState {
    match state {
        DomainProtocolOverrideState::Auto => ProtocolOverrideState::Auto,
        DomainProtocolOverrideState::ForceOn => ProtocolOverrideState::ForceOn,
        DomainProtocolOverrideState::ForceOff => ProtocolOverrideState::ForceOff,
    }
}

#[cfg(test)]
mod catalog_refresh_account_tests {
    use super::{account_can_supply_catalog_refresh_key, select_catalog_refresh_account};
    use crate::models::{Account, AccountSetupStep, AccountType};
    use crate::provider::{CredentialKind, OPENCODE_PROVIDER_ID, QuotaScope};
    use chrono::Utc;

    fn go_account(
        id: &str,
        enabled: bool,
        setup_step: AccountSetupStep,
        key: &str,
        auth_error: Option<&str>,
    ) -> Account {
        let now = Utc::now();
        Account {
            id: id.to_string(),
            provider_id: OPENCODE_PROVIDER_ID.to_string(),
            credential_kind: CredentialKind::ApiKey,
            quota_scope: QuotaScope::Key,
            name: id.to_string(),
            username: None,
            password_cipher: None,
            key_cipher: key.to_string(),
            enabled,
            account_type: AccountType::Key,
            setup_step,
            referral_code: None,
            purchase_date: String::new(),
            expires_on: String::new(),
            cooldown_until: None,
            cooldown_generic_until: None,
            cooldown_5h_until: None,
            cooldown_week_until: None,
            cooldown_month_until: None,
            cooldown_free_until: None,
            last_error: None,
            auth_error: auth_error.map(str::to_string),
            notes: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn a_disabled_ready_key_can_supply_catalog_refresh() {
        let disabled = go_account("go-off", false, AccountSetupStep::Ready, "cipher", None);
        assert!(account_can_supply_catalog_refresh_key(&disabled));
        assert_eq!(
            select_catalog_refresh_account([disabled.clone()], OPENCODE_PROVIDER_ID)
                .map(|account| account.id),
            Some("go-off".into())
        );
    }

    #[test]
    fn catalog_refresh_prefers_an_enabled_ready_key() {
        let disabled = go_account("go-off", false, AccountSetupStep::Ready, "cipher-off", None);
        let enabled = go_account("go-on", true, AccountSetupStep::Ready, "cipher-on", None);
        assert_eq!(
            select_catalog_refresh_account([disabled, enabled], OPENCODE_PROVIDER_ID)
                .map(|account| account.id),
            Some("go-on".into())
        );
    }

    #[test]
    fn drafts_empty_keys_and_auth_errors_cannot_refresh() {
        assert!(!account_can_supply_catalog_refresh_key(&go_account(
            "draft",
            true,
            AccountSetupStep::KeyVerification,
            "cipher",
            None,
        )));
        assert!(!account_can_supply_catalog_refresh_key(&go_account(
            "empty",
            true,
            AccountSetupStep::Ready,
            "  ",
            None,
        )));
        assert!(!account_can_supply_catalog_refresh_key(&go_account(
            "auth",
            true,
            AccountSetupStep::Ready,
            "cipher",
            Some("upstream 401"),
        )));
        assert!(
            select_catalog_refresh_account(
                [go_account(
                    "draft",
                    true,
                    AccountSetupStep::KeyVerification,
                    "cipher",
                    None,
                )],
                OPENCODE_PROVIDER_ID
            )
            .is_none()
        );
    }
}

#[cfg(all(test, debug_assertions))]
mod zen_source_override_tests {
    use super::{
        debug_zen_models_source_url, parse_loopback_http_url,
        set_zen_models_source_url_override_for_tests,
    };

    fn unique_generation() -> u64 {
        uuid::Uuid::new_v4().as_u128() as u64
    }

    #[test]
    fn parse_loopback_http_url_requires_exact_host_without_userinfo_query_or_fragment() {
        assert_eq!(
            parse_loopback_http_url("http://127.0.0.1:9/zen/v1/models").as_deref(),
            Some("http://127.0.0.1:9/zen/v1/models")
        );
        assert_eq!(
            parse_loopback_http_url("http://localhost:9/zen/v1/models").as_deref(),
            Some("http://localhost:9/zen/v1/models")
        );
        assert_eq!(
            parse_loopback_http_url("http://[::1]:9/zen/v1/models").as_deref(),
            Some("http://[::1]:9/zen/v1/models")
        );
        assert_eq!(
            parse_loopback_http_url("HTTP://127.0.0.1:9/zen/v1/models").as_deref(),
            Some("http://127.0.0.1:9/zen/v1/models")
        );

        assert!(parse_loopback_http_url("http://127.0.0.1:9/zen/v1/models?x=1").is_none());
        assert!(parse_loopback_http_url("http://127.0.0.1:9/zen/v1/models#frag").is_none());
        assert!(parse_loopback_http_url("http://user@127.0.0.1:9/zen/v1/models").is_none());
        assert!(parse_loopback_http_url("http://:pass@127.0.0.1:9/zen/v1/models").is_none());
        assert!(parse_loopback_http_url("http://127.0.0.1:9@example.com/zen/v1/models").is_none());
        assert!(parse_loopback_http_url("http://127.0.0.1.example.com:9/zen/v1/models").is_none());
        assert!(parse_loopback_http_url("http://127.0.0.2:9/zen/v1/models").is_none());
        assert!(parse_loopback_http_url("http://[::ffff:127.0.0.1]:9/zen/v1/models").is_none());
        assert!(parse_loopback_http_url("https://opencode.ai/zen/v1/models").is_none());
        assert!(parse_loopback_http_url("http://example.com/zen/v1/models").is_none());
    }

    #[test]
    fn overrides_are_isolated_by_process_generation_and_reject_ambiguous_urls() {
        let first = unique_generation();
        let second = unique_generation();
        set_zen_models_source_url_override_for_tests(
            first,
            Some("http://127.0.0.1:11/a".to_string()),
        );
        set_zen_models_source_url_override_for_tests(
            second,
            Some("http://127.0.0.1:12/b".to_string()),
        );
        assert_eq!(
            debug_zen_models_source_url(first).as_deref(),
            Some("http://127.0.0.1:11/a")
        );
        assert_eq!(
            debug_zen_models_source_url(second).as_deref(),
            Some("http://127.0.0.1:12/b")
        );

        set_zen_models_source_url_override_for_tests(
            first,
            Some("http://127.0.0.1:11@example.com/a".to_string()),
        );
        assert!(debug_zen_models_source_url(first).is_none());
        assert_eq!(
            debug_zen_models_source_url(second).as_deref(),
            Some("http://127.0.0.1:12/b")
        );

        set_zen_models_source_url_override_for_tests(second, None);
        assert!(debug_zen_models_source_url(second).is_none());
    }
}
