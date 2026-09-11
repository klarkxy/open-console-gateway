//! PATCH `/bindings/{id}` — edit one inference binding (CAS-only).

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use ocg_domain::catalog::CredentialKind;
use ocg_domain::credential::observer_credential_id_for_platform_account;

use crate::dashboard_v3::{ControlRevision, V3ApiError, check_expectation, parse_mutation_json};
use crate::db::identity::IdentityAccountRecord;
use crate::provider::CPA_ACCOUNT_ID;
use crate::state::CoreState;

use super::identities::{assigned_endpoints, project_binding_dto};
use super::types::{BindingPatchRequest, BindingPatchResult};

pub(super) async fn patch(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<BindingPatchResult>, V3ApiError> {
    let input = parse_mutation_json::<BindingPatchRequest>(&body)?;
    patch_locked(&state, &id, input).map(Json)
}

fn patch_locked(
    state: &CoreState,
    binding_id: &str,
    input: BindingPatchRequest,
) -> Result<BindingPatchResult, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    check_expectation(state, &input.expectation)?;
    if input.model_scope.is_none() && input.enabled.is_none() {
        return Err(V3ApiError::invalid_request_at(
            state,
            "modelScope or enabled is required",
        ));
    }

    let snapshot = {
        let db = state.db.lock();
        db.list_identity_model().map_err(V3ApiError::internal)?
    };
    if snapshot.platform_parents.iter().any(|parent| {
        observer_credential_id_for_platform_account(&parent.platform_id).as_str() == binding_id
    }) {
        return Err(V3ApiError::invalid_request_at(
            state,
            "platform observer bindings cannot be edited here",
        ));
    }
    let record = snapshot
        .accounts
        .iter()
        .find(|record| record.binding_id == binding_id)
        .ok_or_else(|| V3ApiError::not_found_at(state, "binding not found"))?;
    reject_meaningless_binding_mutation(state, record)?;

    let updated = {
        let db = state.db.lock();
        db.update_credential_binding(binding_id, input.model_scope.as_ref(), input.enabled)
            .map_err(|error| V3ApiError::invalid_request_at(state, error.to_string()))?
    };
    let dynamic_providers = state.dynamic_providers();
    let custom_runtimes = {
        let db = state.db.lock();
        db.list_custom_account_runtimes()
            .map_err(V3ApiError::internal)?
    };
    let dynamic_by_id = dynamic_providers
        .iter()
        .map(|runtime| (runtime.id.as_str(), runtime))
        .collect();
    let custom_by_id = custom_runtimes
        .iter()
        .map(|runtime| (runtime.account_id.as_str(), runtime))
        .collect();
    let (connection_id, endpoints) =
        assigned_endpoints(&record.account, &dynamic_by_id, &custom_by_id);
    state.bump_settings_revision();
    Ok(BindingPatchResult {
        revision: ControlRevision::from_state(state),
        binding: project_binding_dto(record, &connection_id, &endpoints, &updated),
    })
}

fn reject_meaningless_binding_mutation(
    state: &CoreState,
    record: &IdentityAccountRecord,
) -> Result<(), V3ApiError> {
    if record.account.id == CPA_ACCOUNT_ID {
        return Err(V3ApiError::invalid_request_at(
            state,
            "CPA Subscription Pool settings must use the external-integration endpoint",
        ));
    }
    if record.account.is_zen_free() || record.account.credential_kind == CredentialKind::None {
        return Err(V3ApiError::invalid_request_at(
            state,
            "anonymous and no-auth bindings cannot be edited",
        ));
    }
    Ok(())
}
