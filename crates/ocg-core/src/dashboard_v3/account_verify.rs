//! POST `/accounts/{id}/verify`: V2 verification semantics behind the V3 CAS envelope.
//!
//! Network work (Custom's first declared model) never holds `settings_update`.
//! Go, Zen, and Command Code are `NotRequired` no-ops. Debug builds may
//! install a processGeneration-keyed loopback probe seam; that installer, map,
//! and dyn dispatch are absent from release.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use chrono::Utc;

use crate::custom::{self, CustomVerificationContract, CustomVerifyFailure};
use crate::log_types::OperationOutcome;
use crate::models::{
    Account as ModelAccount, AccountCustomConfig as ModelCustomConfig,
    AccountModelCapability as ModelCapability, AppConfig,
};
use crate::provider::{
    ConnectionVerificationStatus, VerificationPolicy, is_custom_api, plan_requires_custom_config,
};
use crate::state::CoreState;

use super::accounts::{load_model_account, mutation_at, mutation_from_state};
use super::types::{AccountMutation, AccountVerify, MutationExpectation};
use super::{V3ApiError, check_expectation, parse_mutation_json};

#[cfg(debug_assertions)]
mod custom_verify_probe {
    use super::CustomVerifyFailure;
    use crate::models::{AccountCustomConfig, AccountModelCapability, AppConfig};
    use parking_lot::Mutex;
    use std::collections::HashMap;
    use std::net::{Ipv4Addr, Ipv6Addr};
    use std::sync::{Arc, OnceLock};

    type CustomProbe = Arc<
        dyn Fn(
                &AppConfig,
                &AccountCustomConfig,
                &AccountModelCapability,
                &str,
            ) -> Result<(), CustomVerifyFailure>
            + Send
            + Sync,
    >;

    static CUSTOM_PROBE_OVERRIDES: OnceLock<Mutex<HashMap<u64, CustomProbe>>> = OnceLock::new();

    fn overrides() -> &'static Mutex<HashMap<u64, CustomProbe>> {
        CUSTOM_PROBE_OVERRIDES.get_or_init(|| Mutex::new(HashMap::new()))
    }

    /// Test-only guard that restores the production Custom probe when dropped.
    pub struct CustomVerifyProbeGuard {
        process_generation: u64,
    }

    impl Drop for CustomVerifyProbeGuard {
        fn drop(&mut self) {
            overrides().lock().remove(&self.process_generation);
        }
    }

    /// Bind an injected Custom probe to one CoreState process generation.
    ///
    /// The override is ignored unless the captured Custom endpoint URL is an
    /// unambiguous loopback HTTP(S) origin.
    #[must_use]
    pub fn install_custom_verify_probe_for_tests(
        process_generation: u64,
        probe: impl Fn(
            &AppConfig,
            &AccountCustomConfig,
            &AccountModelCapability,
            &str,
        ) -> Result<(), CustomVerifyFailure>
        + Send
        + Sync
        + 'static,
    ) -> CustomVerifyProbeGuard {
        overrides()
            .lock()
            .insert(process_generation, Arc::new(probe));
        CustomVerifyProbeGuard { process_generation }
    }

    pub(super) fn override_for(process_generation: u64) -> Option<CustomProbe> {
        overrides().lock().get(&process_generation).cloned()
    }

    pub(super) fn base_url_is_loopback(url: &str) -> bool {
        parse_loopback_http_url(url).is_some()
    }

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
}

#[cfg(debug_assertions)]
pub use custom_verify_probe::{CustomVerifyProbeGuard, install_custom_verify_probe_for_tests};

pub(super) async fn verify_account(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<AccountMutation>, V3ApiError> {
    let mut op =
        super::settings::open_dashboard(&state, "account.verify", "account", Some(id.clone()));
    let input = match parse_mutation_json::<AccountVerify>(&body) {
        Ok(input) => input,
        Err(error) => {
            return super::settings::record_after(
                op,
                &state,
                &[],
                (None, None, None),
                None,
                Err(error),
            )
            .map(Json);
        }
    };
    let prepared = {
        let _settings_update = state.settings_update.lock();
        prepare_verify(&state, &id, &input.expectation)
    };
    let prepared = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            return super::settings::record_after(
                op,
                &state,
                &[],
                (None, None, None),
                None,
                Err(error),
            )
            .map(Json);
        }
    };
    match prepared {
        PreparedVerify::Ready(mutation) => super::settings::record_after(
            op,
            &state,
            &[],
            (Some(1), Some(1), None),
            None,
            Ok(Json(*mutation)),
        ),
        PreparedVerify::Custom(job) => {
            op.subject(job.account.id.clone());
            op.accepted(super::settings::metadata_for(
                &state,
                &[],
                Some(1),
                Some(0),
                None,
                None,
            ));
            let mut effect = super::settings::CommittedEffect::Atomic;
            let result = complete_custom_verification(&state, *job, &mut effect).await;
            match &result {
                Ok(Json(mutation)) => {
                    let verified = mutation.account.as_ref().is_some_and(|account| {
                        account.verification_status
                            == super::types::AccountVerificationStatus::Verified
                    });
                    let (outcome, reason, completed, failed) = if verified {
                        (OperationOutcome::Success, None, Some(1), None)
                    } else {
                        (
                            OperationOutcome::Failed,
                            Some("verificationFailed"),
                            None,
                            Some(1),
                        )
                    };
                    op.complete(
                        outcome,
                        reason,
                        super::settings::metadata_for(
                            &state,
                            &[],
                            Some(1),
                            completed,
                            failed,
                            None,
                        ),
                    );
                    result
                }
                Err(_) => super::settings::record_effect(
                    op,
                    &state,
                    &[],
                    (Some(1), None, Some(1)),
                    None,
                    effect,
                    result,
                ),
            }
        }
    }
}

enum PreparedVerify {
    Ready(Box<AccountMutation>),
    Custom(Box<CustomVerificationJob>),
}

struct CustomVerificationJob {
    expectation: MutationExpectation,
    #[cfg(debug_assertions)]
    process_generation: u64,
    account: ModelAccount,
    config: AppConfig,
    contract: CustomVerificationContract,
    custom_config: ModelCustomConfig,
    first_capability: ModelCapability,
    api_key: String,
}

fn prepare_verify(
    state: &CoreState,
    id: &str,
    expectation: &MutationExpectation,
) -> Result<PreparedVerify, V3ApiError> {
    check_expectation(state, expectation)?;
    let account = load_model_account(state, id)?;
    let plan = crate::provider::builtin_provider(&account.provider_id)
        .ok_or_else(|| V3ApiError::invalid_request_at(state, "unknown provider offering"))?;
    if plan.verification_policy == VerificationPolicy::NotRequired {
        return Ok(PreparedVerify::Ready(Box::new(mutation_from_state(
            state, account,
        )?)));
    }
    if plan_requires_custom_config(plan)
        && state
            .db
            .lock()
            .account_custom_config(id)
            .map_err(V3ApiError::internal)?
            .is_none()
    {
        return Err(V3ApiError::invalid_request_at(
            state,
            "Custom API accounts require a persisted API URL and upstream protocol",
        ));
    }
    if plan.verification_runtime_availability != "available"
        && plan.verification_runtime_availability != "optional"
    {
        return Err(V3ApiError::not_implemented(
            state,
            "connection verification runtime is not available for this Plan in this slice",
        ));
    }
    if is_custom_api(&account.provider_id) {
        let verification = state
            .db
            .lock()
            .account_verification_state(id)
            .map_err(V3ApiError::internal)?
            .ok_or_else(|| V3ApiError::not_found(state))?;
        if verification.status == ConnectionVerificationStatus::Verified {
            return Ok(PreparedVerify::Ready(Box::new(mutation_from_state(
                state, account,
            )?)));
        }
        let job = capture_custom_verification_job(state, account, expectation.clone())?;
        return Ok(PreparedVerify::Custom(Box::new(job)));
    }
    Ok(PreparedVerify::Ready(Box::new(mutation_from_state(
        state, account,
    )?)))
}

fn capture_custom_verification_job(
    state: &CoreState,
    account: ModelAccount,
    expectation: MutationExpectation,
) -> Result<CustomVerificationJob, V3ApiError> {
    let db = state.db.lock();
    let custom_config = db
        .account_custom_config(&account.id)
        .map_err(V3ApiError::internal)?
        .ok_or_else(|| {
            V3ApiError::invalid_request_at(
                state,
                "Custom API accounts require a persisted API URL and upstream protocol",
            )
        })?;
    let capabilities = db
        .list_account_model_capabilities_declared(&account.id)
        .map_err(V3ApiError::internal)?;
    let first_capability = custom::first_declared_capability(&capabilities)
        .cloned()
        .ok_or_else(|| {
            V3ApiError::invalid_request_at(
                state,
                "Custom API accounts require at least one model capability",
            )
        })?;
    let contract = db
        .capture_custom_verification_contract(&account.id)
        .map_err(V3ApiError::internal)?
        .ok_or_else(|| V3ApiError::not_found(state))?;
    drop(db);
    let api_key = state
        .decrypt_key(&account.key_cipher)
        .map_err(V3ApiError::internal)?;
    let config = state.config();
    drop(state.provider_contracts());
    Ok(CustomVerificationJob {
        expectation,
        #[cfg(debug_assertions)]
        process_generation: state.process_generation(),
        account,
        config,
        contract,
        custom_config,
        first_capability,
        api_key,
    })
}

async fn complete_custom_verification(
    state: &CoreState,
    job: CustomVerificationJob,
    effect: &mut super::settings::CommittedEffect,
) -> Result<Json<AccountMutation>, V3ApiError> {
    let result = run_custom_probe(&job).await;
    let _settings_update = state.settings_update.lock();
    check_expectation(state, &job.expectation)?;
    let (status, error) = match result {
        Ok(()) => (ConnectionVerificationStatus::Verified, None),
        Err(failure) => (ConnectionVerificationStatus::Failed, Some(failure.message)),
    };
    let verified_at = (status == ConnectionVerificationStatus::Verified).then(Utc::now);
    let committed = state
        .db
        .lock()
        .commit_custom_verification_if_contract_matches(
            &job.contract,
            status,
            verified_at,
            error.as_deref(),
        )
        .map_err(V3ApiError::internal)?;
    if !committed {
        return Err(V3ApiError::conflict_at(
            state,
            custom::CUSTOM_VERIFICATION_CONFLICT_MESSAGE,
        ));
    }
    let revision = state.bump_settings_revision();
    let account = effect.note_follow_up(load_model_account(state, &job.account.id))?;
    effect.note_follow_up(mutation_at(state, account, revision).map(Json))
}

async fn run_custom_probe(job: &CustomVerificationJob) -> Result<(), CustomVerifyFailure> {
    #[cfg(debug_assertions)]
    {
        if let Some(probe) = custom_verify_probe::override_for(job.process_generation)
            && custom_verify_probe::base_url_is_loopback(&job.custom_config.endpoint_url)
        {
            return probe(
                &job.config,
                &job.custom_config,
                &job.first_capability,
                &job.api_key,
            );
        }
        return custom::probe_custom_connection(
            &job.config,
            &job.custom_config,
            &job.first_capability,
            &job.api_key,
        )
        .await;
    }
    #[cfg(not(debug_assertions))]
    {
        custom::probe_custom_connection(
            &job.config,
            &job.custom_config,
            &job.first_capability,
            &job.api_key,
        )
        .await
    }
}
