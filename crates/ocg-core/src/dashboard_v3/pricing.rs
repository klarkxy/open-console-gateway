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

use crate::kernel::pricing as kernel_pricing;
use crate::pricing::{
    OfficialPricingRefresh, PricingRefreshConfirmPolicy, evaluate_official_pricing_refresh,
    fetch_official_snapshot, prepare_multiplier_update, stamp_pricing_activation,
};
use crate::provider::ProviderRegistry;
use crate::state::CoreState;

use super::types::{
    PricingAdjustment, PricingAvailability, PricingLimits, PricingModel, PricingMultiplierChange,
    PricingMultipliersUpdate, PricingRefresh, PricingRefreshPolicy, PricingRefreshStatus,
    PricingRefreshUpdate, PricingSnapshot, PricingTimeWindow, ProviderPricing,
};
use super::{V3ApiError, check_pricing_expectation, parse_mutation_json};

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
}

#[cfg(debug_assertions)]
pub use official_pricing_fetch::{
    OfficialPricingFetchGuard, install_official_pricing_fetch_error_for_tests,
    install_official_pricing_fetch_for_tests,
};

pub(super) async fn get_pricing(State(state): State<CoreState>) -> Json<PricingSnapshot> {
    let _settings_update = state.settings_update.lock();
    Json(snapshot_from_state(&state))
}

pub(super) async fn refresh_pricing(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<PricingRefresh>, V3ApiError> {
    let update = parse_mutation_json::<PricingRefreshUpdate>(&body)?;
    let Ok(_refresh) = state.pricing_refresh.try_lock() else {
        return Err(V3ApiError::conflict_at(
            &state,
            "OpenCode Go pricing refresh is already running",
        ));
    };
    {
        let _settings_update = state.settings_update.lock();
        check_pricing_expectation(
            &state,
            &update.expectation,
            &update.expected_pricing_revision,
        )?;
    }

    let official = {
        #[cfg(debug_assertions)]
        {
            official_pricing_fetch::fetch(&state).await
        }
        #[cfg(not(debug_assertions))]
        {
            fetch_configured_official_snapshot(&state).await
        }
    };

    let _settings_update = state.settings_update.lock();
    check_pricing_expectation(
        &state,
        &update.expectation,
        &update.expected_pricing_revision,
    )?;
    apply_refresh_locked(&state, official, update).map(Json)
}

pub(super) async fn put_pricing_multipliers(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<PricingSnapshot>, V3ApiError> {
    let update = parse_mutation_json::<PricingMultipliersUpdate>(&body)?;
    let Ok(_refresh) = state.pricing_refresh.try_lock() else {
        return Err(V3ApiError::conflict_at(
            &state,
            "pricing update is already running",
        ));
    };
    let _settings_update = state.settings_update.lock();
    check_pricing_expectation(
        &state,
        &update.expectation,
        &update.expected_pricing_revision,
    )?;
    apply_multipliers_locked(&state, update).map(Json)
}

pub(super) async fn get_provider_pricing(
    State(state): State<CoreState>,
    Path((provider_id, offering_id)): Path<(String, String)>,
) -> Result<Json<ProviderPricing>, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    let descriptor = ProviderRegistry::get(&provider_id, &offering_id)
        .ok_or_else(|| V3ApiError::not_found_at(&state, "provider offering not found"))?;
    let availability =
        map_availability(descriptor.pricing.availability).map_err(V3ApiError::internal)?;
    let pricing = state.pricing_snapshot();
    Ok(Json(provider_pricing_from_snapshot(
        &state,
        provider_id,
        offering_id,
        availability,
        pricing.as_ref(),
    )))
}

async fn fetch_configured_official_snapshot(
    state: &CoreState,
) -> crate::Result<kernel_pricing::PricingSnapshot> {
    let config = state.config();
    fetch_official_snapshot(&config).await
}

fn provider_pricing_from_snapshot(
    state: &CoreState,
    provider_id: String,
    offering_id: String,
    availability: PricingAvailability,
    pricing: &kernel_pricing::PricingSnapshot,
) -> ProviderPricing {
    ProviderPricing {
        provider_id,
        offering_id,
        availability,
        snapshot: (availability == PricingAvailability::Available)
            .then(|| map_kernel_snapshot(state, pricing)),
        revision: state.settings_revision(),
        process_generation: state.process_generation(),
        pricing_revision: pricing.revision.clone(),
    }
}

fn apply_refresh_locked(
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
            state
                .activate_pricing_snapshot(snapshot.clone())
                .map_err(V3ApiError::internal)?;
            audit_pricing(
                state,
                "info",
                &format!("activated OpenCode Go pricing {}", snapshot.revision),
            );
            state.bump_settings_revision();
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
            state
                .activate_pricing_snapshot(snapshot.clone())
                .map_err(V3ApiError::internal)?;
            audit_pricing(
                state,
                "info",
                &format!("updated pricing multipliers in {}", snapshot.revision),
            );
            state.bump_settings_revision();
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
    use crate::db::Database;
    use crate::provider::{
        COMMAND_CODE_PROVIDER_ID, GO_OFFERING_ID, GOAT_OFFERING_ID, OPENCODE_PROVIDER_ID,
    };
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
            GO_OFFERING_ID.into(),
            PricingAvailability::Available,
            captured.as_ref(),
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
            GOAT_OFFERING_ID.into(),
            PricingAvailability::Unavailable,
            captured.as_ref(),
        );
        assert!(goat.snapshot.is_none());
        assert_eq!(goat.pricing_revision, captured.revision);

        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn production_source_gates_the_official_fetch_seam_and_keeps_the_fixed_client() {
        let production = include_str!("pricing.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("production source precedes tests");
        assert!(
            production.contains("fetch_official_snapshot(&config)"),
            "production fetch must use the existing fixed SOURCE_URL client"
        );
        assert!(production.contains("#[cfg(debug_assertions)]"));
        assert!(production.contains("#[cfg(not(debug_assertions))]"));
        assert!(production.contains("fetch_configured_official_snapshot"));
        for needle in [
            "OfficialPricingFetchGuard",
            "install_official_pricing_fetch_for_tests",
            "install_official_pricing_fetch_error_for_tests",
            "OFFICIAL_FETCH_OVERRIDES",
            "dyn Fn",
        ] {
            let idx = production
                .find(needle)
                .unwrap_or_else(|| panic!("{needle} must exist behind debug_assertions"));
            let before = &production[..idx];
            let cfg_idx = before
                .rfind("#[cfg(debug_assertions)]")
                .unwrap_or_else(|| panic!("{needle} must be gated by debug_assertions"));
            assert!(
                !before[cfg_idx..].contains("#[cfg(not(debug_assertions))]"),
                "{needle} compiled into release"
            );
        }
        let release_idx = production
            .find("#[cfg(not(debug_assertions))]")
            .expect("release fetch path");
        let release = &production[release_idx..];
        assert!(release.contains("fetch_configured_official_snapshot"));
        assert!(!release.contains("OFFICIAL_FETCH_OVERRIDES"));
        assert!(!release.contains("dyn Fn"));
        assert!(!release.contains("install_official_pricing_fetch_for_tests"));
    }
}
