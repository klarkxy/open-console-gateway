//! POST `/onboarding/commit` — idempotent CAS write for a user-defined
//! Provider connection and/or its first extra Key.

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use chrono::Utc;
use hmac::{Hmac, Mac};
use ocg_domain::connection::{LegacyConnectionKind, connection_id_for_legacy, target_id_for};
use ocg_domain::dynamic::DynamicAuthKind;
use ocg_domain::ids::CUSTOM_PROVIDER_ID;
use sha2::Sha256;

use crate::dashboard_v3::dynamic_providers::{
    first_account_key, runtime_from_definition, validate_wire_definition,
};
use crate::dashboard_v3::{
    ControlRevision, ProviderDefinitionModel, ProviderModelUpstreamOverride, V3ApiError,
    check_expectation, parse_mutation_json,
};
use crate::db::NewDashboardOperation;
use crate::dynamic::{DynamicProviderRuntime, collides_with_known_id, normalize_preset_id};
use crate::models::{
    Account as ModelAccount, AccountType as ModelAccountType, normalize_account_notes,
};
use crate::provider::BUILTIN_PROVIDERS;
use crate::state::CoreState;
use serde::Serialize;

use super::types::{
    OnboardingAuthorization, OnboardingCommitRequest, OnboardingCommitResult, OnboardingConnection,
    OnboardingConnectionNew, OnboardingTarget, StoredOnboardingCommitResult,
};

const DIGEST_KEY_SETTING: &str = "dashboard_operation_digest_key";
const OPERATION_KIND: &str = "onboarding_commit";
const CUSTOM_HTTP_TEMPLATE: &str = "custom-http";
const BUILTIN_OR_CUSTOM_MESSAGE: &str = "builtin and Custom API connections add Keys on Accounts";

type HmacSha256 = Hmac<Sha256>;

pub(super) async fn commit(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<OnboardingCommitResult>, V3ApiError> {
    let input = parse_mutation_json::<OnboardingCommitRequest>(&body)?;
    if uuid::Uuid::parse_str(&input.operation_id).is_err() {
        return Err(V3ApiError::invalid_request_at(
            &state,
            "operationId must be a UUID",
        ));
    }
    commit_locked(&state, input).map(Json)
}

fn commit_locked(
    state: &CoreState,
    input: OnboardingCommitRequest,
) -> Result<OnboardingCommitResult, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    let digest = payload_digest(state, &input)?;
    if let Some(existing) = {
        let db = state.db.lock();
        db.find_dashboard_operation(&input.operation_id)
            .map_err(V3ApiError::internal)?
    } {
        if existing.payload_digest != digest {
            return Err(V3ApiError::operation_payload_mismatch(
                state,
                "operationId was reused with a different payload",
            ));
        }
        return replay_stored(state, &existing.result_json);
    }

    check_expectation(state, &input.expectation)?;
    match input.connection {
        OnboardingConnection::New(connection) => commit_new(
            state,
            &input.operation_id,
            &digest,
            connection,
            input.authorization,
            input.targets,
        ),
        OnboardingConnection::Existing(connection) => commit_existing(
            state,
            &input.operation_id,
            &digest,
            &connection.connection_id,
            input.authorization,
            input.targets,
        ),
    }
}

fn commit_new(
    state: &CoreState,
    operation_id: &str,
    digest: &str,
    connection: OnboardingConnectionNew,
    authorization: Option<OnboardingAuthorization>,
    targets: Vec<OnboardingTarget>,
) -> Result<OnboardingCommitResult, V3ApiError> {
    if targets.is_empty() {
        return Err(V3ApiError::invalid_request_at(
            state,
            "new connections require at least one target",
        ));
    }
    let auth_kind = DynamicAuthKind::from(connection.auth_kind);
    let now = Utc::now();
    let mut definition = validate_wire_definition(
        uuid::Uuid::new_v4().to_string(),
        connection.name,
        connection.endpoint_url,
        connection.upstream_protocol,
        auth_kind,
        to_definition_models(targets),
    )
    .map_err(|error| V3ApiError::invalid_request_at(state, error.to_string()))?;
    definition.preset_id = preset_from_template_id(state, &connection.template_id)?;
    let existing = state.dynamic_providers();
    if collides_with_known_id(&definition.id, &existing) {
        return Err(V3ApiError::conflict_at(
            state,
            "generated provider id collided; retry",
        ));
    }

    let first_account = first_account_for_new(
        state,
        auth_kind,
        &definition.id,
        &definition.name,
        &authorization,
        now,
    )?;
    let runtime = runtime_from_definition(definition, now, now);
    finish_new(state, operation_id, digest, runtime, first_account)
}

fn finish_new(
    state: &CoreState,
    operation_id: &str,
    digest: &str,
    runtime: DynamicProviderRuntime,
    first_account: Option<ModelAccount>,
) -> Result<OnboardingCommitResult, V3ApiError> {
    let connection_id =
        connection_id_for_legacy(LegacyConnectionKind::DynamicProvider, &runtime.id);
    let credential_id = first_account.as_ref().map(|account| account.id.clone());
    let target_ids = runtime
        .mappings
        .iter()
        .map(|mapping| target_id_for(&connection_id, &mapping.public_model).to_string())
        .collect::<Vec<_>>();
    let stored = StoredOnboardingCommitResult {
        connection_id: connection_id.to_string(),
        credential_id: credential_id.clone(),
        target_ids: target_ids.clone(),
    };
    let operation = ledger_row(operation_id, digest, &stored)?;
    let snapshot = {
        let db = state.db.lock();
        db.commit_onboarding_new(&runtime, first_account.as_ref(), &operation)
            .map_err(|error| V3ApiError::invalid_request_at(state, error.to_string()))?
    };
    state.install_dynamic_providers_snapshot(snapshot);
    Ok(committed_result(state, stored))
}

fn commit_existing(
    state: &CoreState,
    operation_id: &str,
    digest: &str,
    connection_id: &str,
    authorization: Option<OnboardingAuthorization>,
    targets: Vec<OnboardingTarget>,
) -> Result<OnboardingCommitResult, V3ApiError> {
    if !targets.is_empty() {
        return Err(V3ApiError::invalid_request_at(
            state,
            "existing connections cannot change model targets on onboarding commit",
        ));
    }
    let runtime = resolve_existing_dynamic(state, connection_id)?;
    if !runtime.auth_kind.requires_key() {
        return Err(V3ApiError::invalid_request_at(
            state,
            "no-auth provider already has a singleton account",
        ));
    }
    let OnboardingAuthorization::ApiKey(api_key) = authorization.ok_or_else(|| {
        V3ApiError::invalid_request_at(
            state,
            "existing connections require an api_key authorization",
        )
    })?
    else {
        return Err(V3ApiError::invalid_request_at(
            state,
            "existing connections require an api_key authorization",
        ));
    };
    let secret = api_key.secret_input.trim();
    if secret.is_empty() {
        return Err(V3ApiError::invalid_request_at(
            state,
            "secretInput is required",
        ));
    }
    let now = Utc::now();
    let account_name = api_key
        .account_label
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(runtime.name.as_str())
        .to_string();
    let notes = match api_key.notes.as_deref() {
        Some(value) => normalize_account_notes(value)
            .map_err(|error| V3ApiError::invalid_request_at(state, error.to_string()))?,
        None => None,
    };
    let key_cipher = first_account_key(state, runtime.auth_kind, Some(secret))?;
    let account = dynamic_provider_account(
        runtime.auth_kind,
        &runtime.id,
        account_name,
        key_cipher,
        notes,
        now,
    );
    let stored = StoredOnboardingCommitResult {
        connection_id: connection_id.to_string(),
        credential_id: Some(account.id.clone()),
        target_ids: Vec::new(),
    };
    let operation = ledger_row(operation_id, digest, &stored)?;
    {
        let db = state.db.lock();
        db.commit_onboarding_existing_account(&account, &operation)
            .map_err(|error| V3ApiError::invalid_request_at(state, error.to_string()))?;
    }
    state.bump_settings_revision();
    Ok(committed_result(state, stored))
}

fn first_account_for_new(
    state: &CoreState,
    auth_kind: DynamicAuthKind,
    provider_id: &str,
    runtime_name: &str,
    authorization: &Option<OnboardingAuthorization>,
    now: chrono::DateTime<Utc>,
) -> Result<Option<ModelAccount>, V3ApiError> {
    match authorization {
        None => {
            if auth_kind.requires_key() {
                return Ok(None);
            }
            let key_cipher = first_account_key(state, auth_kind, None)?;
            Ok(Some(dynamic_provider_account(
                auth_kind,
                provider_id,
                runtime_name.to_string(),
                key_cipher,
                None,
                now,
            )))
        }
        Some(OnboardingAuthorization::None {}) => {
            if auth_kind.requires_key() {
                return Err(V3ApiError::invalid_request_at(
                    state,
                    "authorization none is only valid when authKind is none",
                ));
            }
            let key_cipher = first_account_key(state, auth_kind, None)?;
            Ok(Some(dynamic_provider_account(
                auth_kind,
                provider_id,
                runtime_name.to_string(),
                key_cipher,
                None,
                now,
            )))
        }
        Some(OnboardingAuthorization::ApiKey(api_key)) => {
            if !auth_kind.requires_key() {
                return Err(V3ApiError::invalid_request_at(
                    state,
                    "api_key authorization is not valid when authKind is none",
                ));
            }
            let secret = api_key.secret_input.trim();
            if secret.is_empty() {
                return Err(V3ApiError::invalid_request_at(
                    state,
                    "secretInput is required",
                ));
            }
            let account_name = api_key
                .account_label
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(runtime_name)
                .to_string();
            let notes = match api_key.notes.as_deref() {
                Some(value) => normalize_account_notes(value)
                    .map_err(|error| V3ApiError::invalid_request_at(state, error.to_string()))?,
                None => None,
            };
            let key_cipher = first_account_key(state, auth_kind, Some(secret))?;
            Ok(Some(dynamic_provider_account(
                auth_kind,
                provider_id,
                account_name,
                key_cipher,
                notes,
                now,
            )))
        }
    }
}

fn dynamic_provider_account(
    auth_kind: DynamicAuthKind,
    provider_id: &str,
    name: String,
    key_cipher: String,
    notes: Option<String>,
    now: chrono::DateTime<Utc>,
) -> ModelAccount {
    ModelAccount {
        id: uuid::Uuid::new_v4().to_string(),
        provider_id: provider_id.to_string(),
        credential_kind: auth_kind.credential_kind(),
        quota_scope: auth_kind.quota_scope(),
        name,
        username: None,
        password_cipher: None,
        key_cipher,
        enabled: true,
        account_type: ModelAccountType::Key,
        setup_step: crate::models::AccountSetupStep::Ready,
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
        auth_error: None,
        notes,
        created_at: now,
        updated_at: now,
    }
}

fn resolve_existing_dynamic(
    state: &CoreState,
    connection_id: &str,
) -> Result<DynamicProviderRuntime, V3ApiError> {
    for runtime in state.dynamic_providers().iter() {
        if connection_id_for_legacy(LegacyConnectionKind::DynamicProvider, &runtime.id).as_str()
            == connection_id
        {
            return Ok(runtime.clone());
        }
    }
    for plan in BUILTIN_PROVIDERS {
        if connection_id_for_legacy(LegacyConnectionKind::BuiltinProvider, plan.provider_id)
            .as_str()
            == connection_id
        {
            return Err(V3ApiError::invalid_request_at(
                state,
                BUILTIN_OR_CUSTOM_MESSAGE,
            ));
        }
    }
    let accounts = state
        .db
        .lock()
        .list_accounts()
        .map_err(V3ApiError::internal)?;
    for account in accounts {
        if account.provider_id == CUSTOM_PROVIDER_ID
            && connection_id_for_legacy(LegacyConnectionKind::CustomAccount, &account.id).as_str()
                == connection_id
        {
            return Err(V3ApiError::invalid_request_at(
                state,
                BUILTIN_OR_CUSTOM_MESSAGE,
            ));
        }
    }
    Err(V3ApiError::not_found_at(state, "connection not found"))
}

fn preset_from_template_id(
    state: &CoreState,
    template_id: &str,
) -> Result<Option<String>, V3ApiError> {
    if template_id.trim() == CUSTOM_HTTP_TEMPLATE {
        return Ok(None);
    }
    normalize_preset_id(Some(template_id.to_string()))
        .map_err(|error| V3ApiError::invalid_request_at(state, error.to_string()))
}

fn to_definition_models(targets: Vec<OnboardingTarget>) -> Vec<ProviderDefinitionModel> {
    targets
        .into_iter()
        .map(|target| ProviderDefinitionModel {
            public_model: target.public_model,
            upstream_model: target.upstream_model,
            upstream_override: target.upstream_override.map(|value| {
                ProviderModelUpstreamOverride {
                    protocol: value.protocol,
                    endpoint_url: value.endpoint_url,
                }
            }),
        })
        .collect()
}

/// Semantic fields covered by the operation digest.
///
/// CAS tokens (`expectedRevision`, `processGeneration`) are not payload: a
/// client whose first response was lost typically refreshes them from
/// `GET /contract` and retries the same `operationId`. Hashing those tokens
/// would turn a legitimate replay into `operationPayloadMismatch`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OnboardingDigestPayload<'a> {
    operation_id: &'a str,
    connection: &'a OnboardingConnection,
    authorization: &'a Option<OnboardingAuthorization>,
    targets: &'a [OnboardingTarget],
}

/// Canonical JSON of the semantic payload only: `operationId`, `connection`,
/// `authorization`, and `targets`. Field order is the struct declaration
/// order. CAS tokens are excluded so retries after a lost response still
/// match the stored digest.
pub(crate) fn digest_payload_bytes(
    input: &OnboardingCommitRequest,
) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(&OnboardingDigestPayload {
        operation_id: &input.operation_id,
        connection: &input.connection,
        authorization: &input.authorization,
        targets: &input.targets,
    })
}

fn payload_digest(
    state: &CoreState,
    input: &OnboardingCommitRequest,
) -> Result<String, V3ApiError> {
    let canonical = digest_payload_bytes(input).map_err(V3ApiError::internal)?;
    let key = digest_key(state)?;
    let mut mac = HmacSha256::new_from_slice(&key).map_err(V3ApiError::internal)?;
    mac.update(&canonical);
    Ok(hex::encode(mac.finalize().into_bytes()))
}

fn digest_key(state: &CoreState) -> Result<[u8; 32], V3ApiError> {
    let db = state.db.lock();
    if let Some(existing) = db
        .get_setting(DIGEST_KEY_SETTING)
        .map_err(V3ApiError::internal)?
    {
        return parse_digest_key(&existing);
    }
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(V3ApiError::internal)?;
    db.set_setting(DIGEST_KEY_SETTING, &hex::encode(bytes))
        .map_err(V3ApiError::internal)?;
    Ok(bytes)
}

fn parse_digest_key(value: &str) -> Result<[u8; 32], V3ApiError> {
    let decoded = hex::decode(value).map_err(V3ApiError::internal)?;
    decoded
        .try_into()
        .map_err(|_| V3ApiError::internal("dashboard operation digest key is not 32 bytes"))
}

fn ledger_row(
    operation_id: &str,
    digest: &str,
    stored: &StoredOnboardingCommitResult,
) -> Result<NewDashboardOperation, V3ApiError> {
    Ok(NewDashboardOperation {
        operation_id: operation_id.to_string(),
        kind: OPERATION_KIND.to_string(),
        payload_digest: digest.to_string(),
        result_json: serde_json::to_string(stored).map_err(V3ApiError::internal)?,
    })
}

fn committed_result(
    state: &CoreState,
    stored: StoredOnboardingCommitResult,
) -> OnboardingCommitResult {
    OnboardingCommitResult {
        revision: ControlRevision::from_state(state),
        connection_id: stored.connection_id,
        credential_id: stored.credential_id,
        target_ids: stored.target_ids,
        replayed: false,
    }
}

fn replay_stored(
    state: &CoreState,
    result_json: &str,
) -> Result<OnboardingCommitResult, V3ApiError> {
    let stored: StoredOnboardingCommitResult =
        serde_json::from_str(result_json).map_err(V3ApiError::internal)?;
    Ok(OnboardingCommitResult {
        revision: ControlRevision::from_state(state),
        connection_id: stored.connection_id,
        credential_id: stored.credential_id,
        target_ids: stored.target_ids,
        replayed: true,
    })
}

#[cfg(test)]
mod tests {
    use super::super::types::OnboardingAuthorizationApiKey;
    use super::*;
    use crate::dashboard_v3::{
        AccountUpstreamProtocol, MutationExpectation, ProviderDefinitionAuthKind,
    };

    fn sample_request(
        expected_revision: u64,
        process_generation: u64,
        secret: &str,
    ) -> OnboardingCommitRequest {
        OnboardingCommitRequest {
            expectation: MutationExpectation {
                expected_revision,
                process_generation,
            },
            operation_id: "11111111-1111-1111-1111-111111111111".into(),
            connection: OnboardingConnection::New(OnboardingConnectionNew {
                template_id: "custom-http".into(),
                name: "Lab".into(),
                endpoint_url: "https://lab.example/v1/chat/completions".into(),
                upstream_protocol: AccountUpstreamProtocol::ChatCompletions,
                auth_kind: ProviderDefinitionAuthKind::Bearer,
            }),
            authorization: Some(OnboardingAuthorization::ApiKey(
                OnboardingAuthorizationApiKey {
                    secret_input: secret.into(),
                    account_label: Some("Primary".into()),
                    notes: None,
                },
            )),
            targets: vec![OnboardingTarget {
                public_model: "lab-opus".into(),
                upstream_model: "vendor/opus".into(),
                upstream_override: None,
            }],
        }
    }

    #[test]
    fn digest_bytes_ignore_cas_tokens_and_change_with_secret_input() {
        let first = sample_request(3, 9, "sk-canonical");
        let refreshed = sample_request(4, 9, "sk-canonical");
        let other_generation = sample_request(3, 10, "sk-canonical");
        let other_secret = sample_request(3, 9, "sk-other");
        let first_bytes = digest_payload_bytes(&first).unwrap();
        assert_eq!(first_bytes, digest_payload_bytes(&refreshed).unwrap());
        assert_eq!(
            first_bytes,
            digest_payload_bytes(&other_generation).unwrap()
        );
        assert_ne!(first_bytes, digest_payload_bytes(&other_secret).unwrap());
        let canonical = String::from_utf8(first_bytes).unwrap();
        assert!(!canonical.contains("expectedRevision"));
        assert!(!canonical.contains("processGeneration"));
        assert!(canonical.contains("sk-canonical"));
    }
}
