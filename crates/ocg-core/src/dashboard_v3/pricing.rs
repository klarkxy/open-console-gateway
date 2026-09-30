//! GET/POST/PUT pricing and provider-scoped pricing reads.
//!
//! Maps the in-memory kernel snapshot into frozen V3 DTOs. Production
//! official fetch is always `fetch_official_snapshot` (fixed SOURCE_URL and
//! the configured proxy). Debug tests may bind a processGeneration-keyed
//! loopback seam; that installer, map, and dyn dispatch are absent from
//! release. `settings_update` is never held across the fetch.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
#[cfg(debug_assertions)]
use chrono::Utc;

use crate::kernel::pricing as kernel_pricing;
use crate::pricing::{
    OfficialPricingRefresh, PricingRefreshConfirmPolicy, evaluate_official_pricing_refresh,
    fetch_goat_pricing_snapshot, fetch_official_snapshot, fetch_ollama_pricing_snapshot,
    latest_provider_pricing_snapshot, merge_current_provider_multipliers,
    prepare_multiplier_update, prepare_provider_multiplier_update, provider_multiplier_deltas,
    provider_pricing_semantically_equal, stamp_pricing_activation, store_provider_pricing_snapshot,
};
use crate::provider::ProviderRegistry;
use crate::state::CoreState;

use super::types::{
    PricingAdjustment, PricingAvailability, PricingLimits, PricingModel, PricingMultiplierChange,
    PricingMultipliersUpdate, PricingRefresh, PricingRefreshPolicy, PricingRefreshStatus,
    PricingRefreshUpdate, PricingSnapshot, PricingTimeWindow, ProviderPricing,
    ProviderPricingRefresh, ProviderPricingRefreshUpdate, ProviderPricingSnapshot,
    ProviderPricingValue,
};
use super::{V3ApiError, check_expectation, check_pricing_expectation, parse_mutation_json};

const UNINITIALIZED_PROVIDER_PRICING_REVISION: &str = "uninitialized";

#[cfg(debug_assertions)]
mod official_pricing_fetch {
    use super::kernel_pricing;
    use crate::state::CoreState;
    use parking_lot::Mutex;
    use std::collections::HashMap;
    use std::sync::{Arc, OnceLock};

    type OfficialFetch =
        Arc<dyn Fn(&CoreState) -> crate::Result<kernel_pricing::PricingSnapshot> + Send + Sync>;

    static OFFICIAL_FETCH_OVERRIDES: OnceLock<Mutex<HashMap<u64, OfficialFetch>>> = OnceLock::new();

    fn official_fetch_overrides() -> &'static Mutex<HashMap<u64, OfficialFetch>> {
        OFFICIAL_FETCH_OVERRIDES.get_or_init(|| Mutex::new(HashMap::new()))
    }

    /// Test-only guard that restores the production official fetch when dropped.
    pub struct OfficialPricingFetchGuard {
        process_generation: u64,
    }

    impl Drop for OfficialPricingFetchGuard {
        fn drop(&mut self) {
            official_fetch_overrides()
                .lock()
                .remove(&self.process_generation);
        }
    }

    /// Bind an injected official snapshot fetch to one CoreState process generation.
    #[must_use]
    pub fn install_official_pricing_fetch_for_tests(
        process_generation: u64,
        fetch: impl Fn(&CoreState) -> crate::Result<kernel_pricing::PricingSnapshot>
        + Send
        + Sync
        + 'static,
    ) -> OfficialPricingFetchGuard {
        official_fetch_overrides()
            .lock()
            .insert(process_generation, Arc::new(fetch));
        OfficialPricingFetchGuard { process_generation }
    }

    /// Bind a local official-refresh failure to one CoreState process generation.
    #[must_use]
    pub fn install_official_pricing_fetch_error_for_tests(
        process_generation: u64,
        message: impl Into<String>,
    ) -> OfficialPricingFetchGuard {
        let message = message.into();
        install_official_pricing_fetch_for_tests(process_generation, move |_| {
            Err(anyhow::anyhow!(message.clone()))
        })
    }

    pub(super) async fn fetch(state: &CoreState) -> crate::Result<kernel_pricing::PricingSnapshot> {
        let override_fetch = official_fetch_overrides()
            .lock()
            .get(&state.process_generation())
            .cloned();
        if let Some(fetch) = override_fetch {
            return fetch(state);
        }
        super::fetch_configured_official_snapshot(state).await
    }

    pub(super) fn has_override(state: &CoreState) -> bool {
        official_fetch_overrides()
            .lock()
            .contains_key(&state.process_generation())
    }
}

#[cfg(debug_assertions)]
pub use official_pricing_fetch::{
    OfficialPricingFetchGuard, install_official_pricing_fetch_error_for_tests,
    install_official_pricing_fetch_for_tests,
};

pub(super) async fn refresh_provider_pricing(
    State(state): State<CoreState>,
    Path(provider_id): Path<String>,
    body: Bytes,
) -> Result<Json<ProviderPricingRefresh>, V3ApiError> {
    let update = parse_mutation_json::<ProviderPricingRefreshUpdate>(&body)?;
    match provider_id.as_str() {
        crate::provider::OPENCODE_PROVIDER_ID => {
            let refreshed = refresh_go_pricing(
                &state,
                PricingRefreshUpdate {
                    expectation: update.expectation,
                    expected_pricing_revision: update.expected_provider_pricing_revision,
                    policy: update.policy,
                    expected_official_content_hash: update.expected_official_content_hash,
                },
            )
            .await?;
            Ok(Json(provider_refresh_from_go(refreshed)))
        }
        crate::provider::COMMAND_CODE_PROVIDER_ID => {
            refresh_html_provider_pricing(&state, crate::provider::COMMAND_CODE_PROVIDER_ID, update)
                .await
                .map(Json)
        }
        crate::provider::OLLAMA_PROVIDER_ID => {
            refresh_html_provider_pricing(&state, crate::provider::OLLAMA_PROVIDER_ID, update)
                .await
                .map(Json)
        }
        _ => Err(V3ApiError::invalid_request_at(
            &state,
            "provider does not support pricing refresh",
        )),
    }
}

async fn refresh_go_pricing(
    state: &CoreState,
    update: PricingRefreshUpdate,
) -> Result<PricingRefresh, V3ApiError> {
    let Ok(_refresh) = state.pricing_refresh.try_lock() else {
        return Err(V3ApiError::conflict_at(
            state,
            "provider pricing refresh is already running",
        ));
    };
    {
        let _settings_update = state.settings_update.lock();
        check_pricing_expectation(
            state,
            &update.expectation,
            &update.expected_pricing_revision,
        )?;
    }

    let official = {
        #[cfg(debug_assertions)]
        {
            official_pricing_fetch::fetch(state).await
        }
        #[cfg(not(debug_assertions))]
        {
            fetch_configured_official_snapshot(state).await
        }
    };

    let _settings_update = state.settings_update.lock();
    check_pricing_expectation(
        state,
        &update.expectation,
        &update.expected_pricing_revision,
    )?;
    apply_go_refresh_locked(state, official, update)
}

pub(super) async fn put_pricing_multipliers(
    State(state): State<CoreState>,
    Path(provider_id): Path<String>,
    body: Bytes,
) -> Result<Response, V3ApiError> {
    let is_go = provider_id == crate::provider::OPENCODE_PROVIDER_ID;
    let is_goat = provider_id == crate::provider::COMMAND_CODE_PROVIDER_ID;
    if !is_go && !is_goat {
        return Err(V3ApiError::invalid_request_at(
            &state,
            "provider offering does not support pricing multipliers",
        ));
    }
    let update = parse_mutation_json::<PricingMultipliersUpdate>(&body)?;
    let Ok(_refresh) = state.pricing_refresh.try_lock() else {
        return Err(V3ApiError::conflict_at(
            &state,
            "pricing update is already running",
        ));
    };
    let _settings_update = state.settings_update.lock();
    if is_go {
        check_pricing_expectation(
            &state,
            &update.expectation,
            &update.expected_pricing_revision,
        )?;
        return apply_multipliers_locked(&state, update)
            .map(|snapshot| Json(snapshot).into_response());
    }

    check_expectation(&state, &update.expectation)?;
    let current_revision = current_provider_pricing_revision(&state, &provider_id)?;
    if update.expected_pricing_revision != current_revision {
        return Err(V3ApiError::conflict_at(
            &state,
            "provider pricing revision changed",
        ));
    }
    let active = latest_provider_pricing_snapshot(&state.db.lock(), &provider_id)
        .map_err(V3ApiError::internal)?
        .ok_or_else(|| V3ApiError::invalid_request_at(&state, "provider pricing is not loaded"))?;
    let writes = update
        .multipliers
        .into_iter()
        .map(|write| (write.model_id, write.multiplier))
        .collect::<Vec<_>>();
    let active = match prepare_provider_multiplier_update(&active, &writes) {
        Err(message) => return Err(V3ApiError::invalid_request_at(&state, message)),
        Ok(None) => active,
        Ok(Some(snapshot)) => {
            store_provider_pricing_snapshot(&state.db.lock(), &snapshot)
                .map_err(V3ApiError::internal)?;
            state.bump_settings_revision();
            audit_pricing(
                &state,
                "info",
                &format!(
                    "updated provider pricing multipliers in {}/{}",
                    provider_id,
                    snapshot.revision()
                ),
            );
            snapshot
        }
    };
    Ok(Json(provider_pricing_from_snapshot(
        &state,
        provider_id,
        PricingAvailability::Available,
        state.pricing_snapshot().as_ref(),
        Some(&active),
    ))
    .into_response())
}

pub(super) async fn get_provider_pricing(
    State(state): State<CoreState>,
    Path(provider_id): Path<String>,
) -> Result<Json<ProviderPricing>, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    if crate::dynamic::find_runtime(&state.dynamic_providers(), &provider_id).is_some() {
        return Ok(Json(provider_pricing_from_snapshot(
            &state,
            provider_id,
            PricingAvailability::Unpriced,
            state.pricing_snapshot().as_ref(),
            None,
        )));
    }
    let descriptor = ProviderRegistry::get(&provider_id)
        .ok_or_else(|| V3ApiError::not_found_at(&state, "provider offering not found"))?;
    let availability =
        map_availability(descriptor.pricing.availability).map_err(V3ApiError::internal)?;
    let pricing = state.pricing_snapshot();
    let scoped = latest_provider_pricing_snapshot(&state.db.lock(), &provider_id)
        .map_err(V3ApiError::internal)?;
    Ok(Json(provider_pricing_from_snapshot(
        &state,
        provider_id,
        availability,
        pricing.as_ref(),
        scoped.as_ref(),
    )))
}

async fn fetch_configured_official_snapshot(
    state: &CoreState,
) -> crate::Result<kernel_pricing::PricingSnapshot> {
    let config = state.config();
    fetch_official_snapshot(&config).await
}

async fn fetch_configured_goat_snapshot(
    state: &CoreState,
) -> crate::Result<crate::pricing::ProviderScopedPricingSnapshot> {
    #[cfg(debug_assertions)]
    if official_pricing_fetch::has_override(state) {
        let go = official_pricing_fetch::fetch(state).await;
        return synthetic_goat_snapshot_for_tests(go.as_ref());
    }
    let config = state.config();
    fetch_goat_pricing_snapshot(&config).await
}

async fn fetch_configured_ollama_snapshot(
    state: &CoreState,
) -> crate::Result<crate::pricing::ProviderScopedPricingSnapshot> {
    #[cfg(debug_assertions)]
    if official_pricing_fetch::has_override(state) {
        let go = official_pricing_fetch::fetch(state).await;
        return synthetic_ollama_snapshot_for_tests(go.as_ref());
    }
    let config = state.config();
    fetch_ollama_pricing_snapshot(&config).await
}

#[cfg(debug_assertions)]
fn synthetic_goat_snapshot_for_tests(
    go: Result<&kernel_pricing::PricingSnapshot, &anyhow::Error>,
) -> crate::Result<crate::pricing::ProviderScopedPricingSnapshot> {
    let go = go.map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let first = go
        .models
        .first()
        .ok_or_else(|| anyhow::anyhow!("test Go pricing snapshot has no models"))?;
    let value = crate::pricing::ProviderPricingValue::new(
        first.model_id.clone(),
        first.display_name.clone(),
        Some(first.input),
        Some(first.output),
        Some(first.cache_read),
        first.cache_write,
        Some(70.0),
        Some(20.0),
        Some(10.0),
        Some("USD".to_string()),
        first.min_input_tokens,
        first.max_input_tokens,
        first.time_window,
    )?;
    crate::pricing::ProviderScopedPricingSnapshot::new(
        crate::provider::COMMAND_CODE_PROVIDER_ID,
        format!("test-goat-{}", go.content_hash),
        Utc::now().to_rfc3339(),
        None,
        crate::pricing::GOAT_SOURCE_URL,
        go.content_hash.clone(),
        crate::pricing::ProviderPricingEvidence::Verified,
        vec![value],
    )
}

#[cfg(debug_assertions)]
fn synthetic_ollama_snapshot_for_tests(
    go: Result<&kernel_pricing::PricingSnapshot, &anyhow::Error>,
) -> crate::Result<crate::pricing::ProviderScopedPricingSnapshot> {
    let go = go.map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let first = go
        .models
        .first()
        .ok_or_else(|| anyhow::anyhow!("test Go pricing snapshot has no models"))?;
    let value = crate::pricing::ProviderPricingValue::new(
        first.model_id.clone(),
        first.display_name.clone(),
        Some(first.input),
        Some(first.output),
        Some(first.cache_read),
        None,
        None,
        None,
        None,
        Some("USD".to_string()),
        first.min_input_tokens,
        first.max_input_tokens,
        first.time_window,
    )?
    .with_quota_multiplier(1.0)?;
    crate::pricing::ProviderScopedPricingSnapshot::new(
        crate::provider::OLLAMA_PROVIDER_ID,
        format!("test-ollama-{}", go.content_hash),
        Utc::now().to_rfc3339(),
        None,
        crate::pricing::OLLAMA_SOURCE_URL,
        go.content_hash.clone(),
        crate::pricing::ProviderPricingEvidence::Verified,
        vec![value],
    )
}

async fn refresh_html_provider_pricing(
    state: &CoreState,
    provider_id: &str,
    update: ProviderPricingRefreshUpdate,
) -> Result<ProviderPricingRefresh, V3ApiError> {
    let Ok(_refresh) = state.pricing_refresh.try_lock() else {
        return Err(V3ApiError::conflict_at(
            state,
            "provider pricing refresh is already running",
        ));
    };
    {
        let _settings_update = state.settings_update.lock();
        check_provider_pricing_expectation(state, provider_id, &update)?;
    }

    let fetched = match provider_id {
        crate::provider::COMMAND_CODE_PROVIDER_ID => fetch_configured_goat_snapshot(state).await,
        crate::provider::OLLAMA_PROVIDER_ID => fetch_configured_ollama_snapshot(state).await,
        _ => {
            return Err(V3ApiError::invalid_request_at(
                state,
                "provider does not support pricing refresh",
            ));
        }
    };
    let label = if provider_id == crate::provider::OLLAMA_PROVIDER_ID {
        "Ollama Cloud"
    } else {
        "Command Code GOAT"
    };

    let _settings_update = state.settings_update.lock();
    check_provider_pricing_expectation(state, provider_id, &update)?;
    let current_revision = current_provider_pricing_revision(state, provider_id)?;
    let active = latest_provider_pricing_snapshot(&state.db.lock(), provider_id)
        .map_err(V3ApiError::internal)?;
    match fetched {
        Err(error) => {
            let error = error.to_string();
            audit_pricing(
                state,
                "warn",
                &format!("{label} pricing refresh failed: {error}"),
            );
            Ok(provider_refresh_result(
                state,
                provider_id,
                PricingRefreshStatus::FailedNoChange,
                Vec::new(),
                None,
                Some(error),
                current_revision,
            ))
        }
        Ok(mut snapshot) => {
            let multiplier_changes = active
                .as_ref()
                .map(|active| provider_multiplier_deltas(active, &snapshot))
                .unwrap_or_default();
            let official_content_hash = snapshot.content_hash().to_string();
            let confirmation_matches = update
                .expected_official_content_hash
                .as_deref()
                .is_some_and(|expected| expected == official_content_hash);
            if !multiplier_changes.is_empty() && (update.policy.is_none() || !confirmation_matches)
            {
                return Ok(provider_refresh_result(
                    state,
                    provider_id,
                    PricingRefreshStatus::NeedsConfirmation,
                    map_changes(multiplier_changes),
                    Some(official_content_hash),
                    None,
                    current_revision,
                ));
            }
            if matches!(update.policy, Some(PricingRefreshPolicy::KeepCurrent))
                && let Some(active) = active.as_ref()
            {
                merge_current_provider_multipliers(active, &mut snapshot);
            }
            if active
                .as_ref()
                .is_some_and(|active| provider_pricing_semantically_equal(active, &snapshot))
            {
                return Ok(provider_refresh_result(
                    state,
                    provider_id,
                    PricingRefreshStatus::Unchanged,
                    map_changes(multiplier_changes),
                    None,
                    None,
                    current_revision,
                ));
            }
            store_provider_pricing_snapshot(&state.db.lock(), &snapshot)
                .map_err(V3ApiError::internal)?;
            state.bump_settings_revision();
            audit_pricing(
                state,
                "info",
                &format!("activated {label} provider pricing {}", snapshot.revision()),
            );
            Ok(provider_refresh_result(
                state,
                provider_id,
                PricingRefreshStatus::Success,
                map_changes(multiplier_changes),
                None,
                None,
                snapshot.revision().to_string(),
            ))
        }
    }
}

fn check_provider_pricing_expectation(
    state: &CoreState,
    provider_id: &str,

    update: &ProviderPricingRefreshUpdate,
) -> Result<(), V3ApiError> {
    check_expectation(state, &update.expectation)?;
    let current = current_provider_pricing_revision(state, provider_id)?;
    if update.expected_provider_pricing_revision != current {
        Err(V3ApiError::conflict_at(
            state,
            "provider pricing revision changed",
        ))
    } else {
        Ok(())
    }
}

fn current_provider_pricing_revision(
    state: &CoreState,
    provider_id: &str,
) -> Result<String, V3ApiError> {
    if provider_id == crate::provider::OPENCODE_PROVIDER_ID {
        return Ok(state.pricing_snapshot().revision.clone());
    }
    Ok(
        latest_provider_pricing_snapshot(&state.db.lock(), provider_id)
            .map_err(V3ApiError::internal)?
            .map(|snapshot| snapshot.revision().to_string())
            .unwrap_or_else(|| UNINITIALIZED_PROVIDER_PRICING_REVISION.to_string()),
    )
}

fn provider_refresh_from_go(refreshed: PricingRefresh) -> ProviderPricingRefresh {
    let pricing_revision = refreshed.snapshot.pricing_revision.clone();
    let revision = refreshed.snapshot.revision;
    let process_generation = refreshed.snapshot.process_generation;
    ProviderPricingRefresh {
        provider_id: crate::provider::OPENCODE_PROVIDER_ID.to_string(),
        refresh_status: refreshed.refresh_status,
        multiplier_changes: refreshed.multiplier_changes,
        official_content_hash: refreshed.official_content_hash,
        error: refreshed.error,
        snapshot: Some(refreshed.snapshot),
        revision,
        process_generation,
        pricing_revision: pricing_revision.clone(),
        provider_pricing_revision: pricing_revision,
    }
}

#[allow(clippy::too_many_arguments)]
fn provider_refresh_result(
    state: &CoreState,
    provider_id: &str,
    refresh_status: PricingRefreshStatus,
    multiplier_changes: Vec<PricingMultiplierChange>,
    official_content_hash: Option<String>,
    error: Option<String>,
    provider_pricing_revision: String,
) -> ProviderPricingRefresh {
    ProviderPricingRefresh {
        provider_id: provider_id.to_string(),
        refresh_status,
        multiplier_changes,
        official_content_hash,
        error,
        snapshot: None,
        revision: state.settings_revision(),
        process_generation: state.process_generation(),
        pricing_revision: state.pricing_snapshot().revision.clone(),
        provider_pricing_revision,
    }
}

fn provider_pricing_from_snapshot(
    state: &CoreState,
    provider_id: String,

    availability: PricingAvailability,
    pricing: &kernel_pricing::PricingSnapshot,
    scoped: Option<&crate::pricing::ProviderScopedPricingSnapshot>,
) -> ProviderPricing {
    let is_go = provider_id == crate::provider::OPENCODE_PROVIDER_ID;
    let provider_pricing_revision = if is_go {
        pricing.revision.clone()
    } else {
        scoped
            .map(|snapshot| snapshot.revision().to_string())
            .unwrap_or_else(|| UNINITIALIZED_PROVIDER_PRICING_REVISION.to_string())
    };
    ProviderPricing {
        provider_id,
        availability,
        snapshot: (availability == PricingAvailability::Available && is_go)
            .then(|| map_kernel_snapshot(state, pricing)),
        // Go's live snapshot is already represented by `snapshot`; historical
        // migration rows in the provider-neutral table are not a second price
        // source and must not be exposed alongside it.
        provider_snapshot: (!is_go)
            .then(|| scoped.map(map_provider_snapshot))
            .flatten(),
        revision: state.settings_revision(),
        process_generation: state.process_generation(),
        pricing_revision: pricing.revision.clone(),
        provider_pricing_revision,
    }
}

fn map_provider_snapshot(
    snapshot: &crate::pricing::ProviderScopedPricingSnapshot,
) -> ProviderPricingSnapshot {
    ProviderPricingSnapshot {
        revision: snapshot.revision().to_string(),
        activated_at: snapshot.activated_at().to_string(),
        document_updated_at: snapshot.document_updated_at().map(str::to_string),
        source_url: snapshot.source_url().to_string(),
        content_hash: snapshot.content_hash().to_string(),
        evidence: match snapshot.evidence() {
            crate::pricing::ProviderPricingEvidence::Verified => "verified",
            crate::pricing::ProviderPricingEvidence::Experimental => "experimental",
            crate::pricing::ProviderPricingEvidence::Unavailable => "unavailable",
        }
        .to_string(),
        values: snapshot
            .values()
            .iter()
            .map(|value| ProviderPricingValue {
                model_id: value.model_id().to_string(),
                display_name: value.display_name().to_string(),
                input_per_million: value.input_per_million(),
                output_per_million: value.output_per_million(),
                cache_read_per_million: value.cache_read_per_million(),
                cache_write_per_million: value.cache_write_per_million(),
                plan_limit: value.plan_limit(),
                model_allowance: value.model_allowance(),
                quota_multiplier: value.quota_multiplier(),
                paid_plan_price: value.paid_plan_price(),
                currency: value.currency().map(str::to_string),
                min_input_tokens: value.min_input_tokens(),
                max_input_tokens: value.max_input_tokens(),
                time_window: map_time_window(value.time_window()),
            })
            .collect(),
    }
}

fn apply_go_refresh_locked(
    state: &CoreState,
    official: crate::Result<kernel_pricing::PricingSnapshot>,
    update: PricingRefreshUpdate,
) -> Result<PricingRefresh, V3ApiError> {
    let policy = update.policy.map(|policy| match policy {
        PricingRefreshPolicy::KeepCurrent => PricingRefreshConfirmPolicy::KeepCurrent,
        PricingRefreshPolicy::UseOfficial => PricingRefreshConfirmPolicy::UseOfficial,
    });
    match evaluate_official_pricing_refresh(
        state.pricing_snapshot().as_ref(),
        official,
        policy,
        update.expected_official_content_hash.as_deref(),
    ) {
        OfficialPricingRefresh::NeedsConfirmation {
            multiplier_changes,
            official_content_hash,
        } => Ok(PricingRefresh {
            snapshot: snapshot_from_state(state),
            refresh_status: PricingRefreshStatus::NeedsConfirmation,
            multiplier_changes: map_changes(multiplier_changes),
            official_content_hash: Some(official_content_hash),
            error: None,
        }),
        OfficialPricingRefresh::Unchanged { multiplier_changes } => Ok(PricingRefresh {
            snapshot: snapshot_from_state(state),
            refresh_status: PricingRefreshStatus::Unchanged,
            multiplier_changes: map_changes(multiplier_changes),
            official_content_hash: None,
            error: None,
        }),
        OfficialPricingRefresh::Activate {
            candidate,
            multiplier_changes,
        } => {
            let snapshot = stamp_pricing_activation(candidate);
            // Bump before activating, not after. `activate_pricing_snapshot`
            // republishes the preparation aggregate itself, and it stamps that
            // publication with the revision as it stands. Activating first would
            // publish the new pricing at the previous revision and then advance
            // the revision, leaving every later request to pay a drift rebuild
            // for a change this handler already published. The gate is held
            // across both, so no other writer can observe the window.
            state.bump_settings_revision();
            state
                .activate_pricing_snapshot(snapshot.clone())
                .map_err(V3ApiError::internal)?;
            audit_pricing(
                state,
                "info",
                &format!(
                    "activated OpenCode Go provider pricing {}",
                    snapshot.revision
                ),
            );
            Ok(PricingRefresh {
                snapshot: map_kernel_snapshot(state, &snapshot),
                refresh_status: PricingRefreshStatus::Success,
                multiplier_changes: map_changes(multiplier_changes),
                official_content_hash: None,
                error: None,
            })
        }
        OfficialPricingRefresh::Failed { error } => {
            audit_pricing(
                state,
                "warn",
                &format!("OpenCode Go pricing refresh failed: {error}"),
            );
            Ok(PricingRefresh {
                snapshot: snapshot_from_state(state),
                refresh_status: PricingRefreshStatus::FailedNoChange,
                multiplier_changes: Vec::new(),
                official_content_hash: None,
                error: Some(error),
            })
        }
    }
}

fn apply_multipliers_locked(
    state: &CoreState,
    update: PricingMultipliersUpdate,
) -> Result<PricingSnapshot, V3ApiError> {
    let active = state.pricing_snapshot();
    let writes = update
        .multipliers
        .into_iter()
        .map(|write| (write.model_id, write.multiplier))
        .collect::<Vec<_>>();
    match prepare_multiplier_update(&active, &writes) {
        Err(message) => Err(V3ApiError::invalid_request_at(state, message)),
        Ok(None) => Ok(snapshot_from_state(state)),
        Ok(Some(snapshot)) => {
            let snapshot = stamp_pricing_activation(snapshot);
            // Bump before activating, for the same reason as
            // `apply_go_refresh_locked`: the activation republishes the aggregate
            // stamped with the revision as it stands, so bumping afterwards would
            // leave that publication one revision behind and send the next
            // request through a gated rebuild for a change already published.
            state.bump_settings_revision();
            state
                .activate_pricing_snapshot(snapshot.clone())
                .map_err(V3ApiError::internal)?;
            audit_pricing(
                state,
                "info",
                &format!("updated pricing multipliers in {}", snapshot.revision),
            );
            Ok(map_kernel_snapshot(state, &snapshot))
        }
    }
}

fn snapshot_from_state(state: &CoreState) -> PricingSnapshot {
    map_kernel_snapshot(state, state.pricing_snapshot().as_ref())
}

fn map_kernel_snapshot(
    state: &CoreState,
    snapshot: &kernel_pricing::PricingSnapshot,
) -> PricingSnapshot {
    PricingSnapshot {
        revision: state.settings_revision(),
        process_generation: state.process_generation(),
        pricing_revision: snapshot.revision.clone(),
        activated_at: snapshot.activated_at.clone(),
        document_updated_at: snapshot.document_updated_at.clone(),
        source_url: snapshot.source_url.clone(),
        content_hash: snapshot.content_hash.clone(),
        adjustment_policy_version: snapshot.adjustment_policy_version.clone(),
        limits: PricingLimits {
            window_5h: snapshot.limits.window_5h,
            window_week: snapshot.limits.window_week,
            window_month: snapshot.limits.window_month,
        },
        models: snapshot.models.iter().map(map_model).collect(),
    }
}

fn map_model(model: &kernel_pricing::PricingModel) -> PricingModel {
    PricingModel {
        model_id: model.model_id.clone(),
        display_name: model.display_name.clone(),
        input: model.input,
        output: model.output,
        cache_read: model.cache_read,
        cache_write: model.cache_write,
        usage: model.usage,
        quota_multiplier: model.quota_multiplier,
        min_input_tokens: model.min_input_tokens,
        max_input_tokens: model.max_input_tokens,
        time_window: map_time_window(model.time_window),
        adjustments: model.adjustments.iter().map(map_adjustment).collect(),
    }
}

fn map_adjustment(adjustment: &kernel_pricing::PricingAdjustment) -> PricingAdjustment {
    PricingAdjustment {
        label: adjustment.label.clone(),
        multiplier: adjustment.multiplier,
        applies_to: adjustment.applies_to.clone(),
    }
}

fn map_time_window(value: kernel_pricing::PricingTimeWindow) -> PricingTimeWindow {
    match value {
        kernel_pricing::PricingTimeWindow::Always => PricingTimeWindow::Always,
        kernel_pricing::PricingTimeWindow::OffPeak => PricingTimeWindow::OffPeak,
        kernel_pricing::PricingTimeWindow::Peak => PricingTimeWindow::Peak,
    }
}

fn map_changes(
    changes: Vec<crate::pricing::PricingMultiplierDelta>,
) -> Vec<PricingMultiplierChange> {
    changes
        .into_iter()
        .map(|change| PricingMultiplierChange {
            model_id: change.model_id,
            current_multiplier: change.current_multiplier,
            official_multiplier: change.official_multiplier,
        })
        .collect()
}

fn map_availability(value: &str) -> Result<PricingAvailability, String> {
    match value {
        "available" => Ok(PricingAvailability::Available),
        "unavailable" => Ok(PricingAvailability::Unavailable),
        "not_applicable" => Ok(PricingAvailability::NotApplicable),
        "unpriced" => Ok(PricingAvailability::Unpriced),
        other => Err(format!("unknown pricing availability `{other}`")),
    }
}

fn audit_pricing(state: &CoreState, level: &str, message: &str) {
    if let Err(error) = state.db.lock().log_gateway(level, "pricing", message) {
        eprintln!("warning: failed to audit pricing event: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{KeyCipher, StaticKeyCipher};
    use crate::dashboard_v3::types::{MutationExpectation, PricingMultiplierWrite};
    use crate::db::Database;
    use crate::provider::{COMMAND_CODE_PROVIDER_ID, OPENCODE_PROVIDER_ID};
    use crate::state::CoreStateInner;
    use std::sync::Arc;

    fn test_state(label: &str) -> (Arc<CoreStateInner>, std::path::PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("ocg-v3-pricing-{label}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Database::open(dir.clone()).unwrap();
        let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("v3-pricing"));
        let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());
        (state, dir)
    }

    #[test]
    fn provider_pricing_derives_nested_and_outer_revision_from_one_captured_snapshot() {
        let (state, dir) = test_state("coherent");
        let captured = state.pricing_snapshot();
        let mut concurrent = captured.as_ref().clone();
        concurrent.revision = "concurrent-v2-activation".into();
        concurrent.activated_at = "2099-01-01T00:00:00Z".into();
        state.activate_pricing_snapshot(concurrent).unwrap();
        assert_ne!(state.pricing_snapshot().revision, captured.revision);

        let go = provider_pricing_from_snapshot(
            &state,
            OPENCODE_PROVIDER_ID.into(),
            PricingAvailability::Available,
            captured.as_ref(),
            None,
        );
        assert_eq!(go.pricing_revision, captured.revision);
        assert_eq!(
            go.snapshot.as_ref().unwrap().pricing_revision,
            captured.revision
        );
        assert_ne!(go.pricing_revision, state.pricing_snapshot().revision);

        let goat = provider_pricing_from_snapshot(
            &state,
            COMMAND_CODE_PROVIDER_ID.into(),
            PricingAvailability::Unavailable,
            captured.as_ref(),
            None,
        );
        assert!(goat.snapshot.is_none());
        assert_eq!(goat.pricing_revision, captured.revision);

        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A pricing activation republishes the preparation aggregate itself, so the
    /// revision bump has to happen *before* it. Publish-then-bump leaves the
    /// aggregate stamped one revision behind, and the next request's preparation
    /// falls off the fast path onto a gated rebuild — for a change the handler
    /// already published.
    ///
    /// Both orders leave the next `gateway_preparation()` reporting the new
    /// pricing, because the slow path repairs whatever drift it finds, so the
    /// observable is the gate itself: the fast path never wants
    /// `settings_update`. Hold it on a second thread and require the read to
    /// return anyway. An ordering regression deadlocks on the non-reentrant gate
    /// instead, and the receive timeout turns that into a failure rather than a
    /// hung suite.
    #[test]
    fn pricing_activation_leaves_the_next_preparation_read_off_the_gate() {
        use std::time::Duration as StdDuration;

        let (state, dir) = test_state("pricing-bump-order");
        let active = state.pricing_snapshot();
        let model_id = active
            .models
            .first()
            .expect("the seed pricing snapshot should list models")
            .model_id
            .clone();
        {
            let _settings_update = state.settings_update.lock();
            apply_multipliers_locked(
                &state,
                PricingMultipliersUpdate {
                    expectation: MutationExpectation {
                        expected_revision: state.settings_revision(),
                        process_generation: state.process_generation(),
                    },
                    expected_pricing_revision: active.revision.clone(),
                    multipliers: vec![PricingMultiplierWrite {
                        model_id,
                        multiplier: 1.5,
                    }],
                },
            )
            .expect("a multiplier write should activate");
        }

        let expected = state.settings_revision();
        let worker_state = Arc::clone(&state);
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _gate = worker_state.settings_update.lock();
            let read = worker_state.gateway_preparation();
            let _ = tx.send(read.map(|published| published.revision()));
        });

        match rx.recv_timeout(StdDuration::from_secs(10)) {
            Ok(Ok(revision)) => {
                worker.join().expect("the reader thread should finish");
                assert_eq!(
                    revision, expected,
                    "the read must be the fast path, serving the generation the activation published"
                );
            }
            Ok(Err(error)) => panic!("preparation should read after a pricing activation: {error}"),
            // Deliberately not joining: a regression leaves that thread blocked
            // on the gate forever, and joining it would hang the suite.
            Err(_) => panic!(
                "pricing activation published before bumping: the next preparation read fell back to a gated rebuild"
            ),
        }

        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }
}
