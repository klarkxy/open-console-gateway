use super::super::types::{OnboardingAuthorizationApiKey, OnboardingConnectionExisting};
use super::*;
use crate::dashboard_v3::{
    AccountUpstreamProtocol, MutationExpectation, ProviderDefinitionAuthKind,
};
use axum::response::IntoResponse;

fn commit_locked(
    state: &CoreState,
    input: OnboardingCommitRequest,
) -> Result<OnboardingCommitResult, V3ApiError> {
    commit_recorded(state, input, &mut |_, _| {})
}

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
            protocol_routes: None,
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
        mode: None,
        authorize_current_endpoint: false,
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
    assert!(!canonical.contains("mode"));
    assert!(!canonical.contains("authorizeCurrentEndpoint"));
}

#[test]
fn digest_bytes_include_mode_and_authorize_flag_only_when_set() {
    let mut draft = sample_request(3, 9, "sk-canonical");
    draft.mode = Some(OnboardingCommitMode::Draft);
    let mut complete = sample_request(3, 9, "sk-canonical");
    complete.mode = Some(OnboardingCommitMode::Complete);
    let mut authorized = sample_request(3, 9, "sk-canonical");
    authorized.mode = Some(OnboardingCommitMode::Complete);
    authorized.authorize_current_endpoint = true;
    let baseline = digest_payload_bytes(&sample_request(3, 9, "sk-canonical")).unwrap();
    let draft_bytes = digest_payload_bytes(&draft).unwrap();
    let complete_bytes = digest_payload_bytes(&complete).unwrap();
    let authorized_bytes = digest_payload_bytes(&authorized).unwrap();
    assert_ne!(baseline, draft_bytes);
    assert_ne!(draft_bytes, complete_bytes);
    assert_ne!(complete_bytes, authorized_bytes);
    let draft_json = String::from_utf8(draft_bytes).unwrap();
    assert!(draft_json.contains("\"mode\":\"draft\""));
    assert!(!draft_json.contains("authorizeCurrentEndpoint"));
    let authorized_json = String::from_utf8(authorized_bytes).unwrap();
    assert!(authorized_json.contains("\"authorizeCurrentEndpoint\":true"));
}

#[test]
fn historical_receipt_replays_account_id_as_credential_id() {
    let json = r#"{"connectionId":"conn","credentialId":"legacy-account-id","targetIds":["t1"]}"#;
    let stored: StoredOnboardingCommitResult = serde_json::from_str(json).unwrap();
    assert_eq!(stored.credential_id.as_deref(), Some("legacy-account-id"));
    assert_eq!(stored.account_id, None);
}

#[tokio::test]
async fn existing_legacy_custom_connection_accepts_a_second_key_and_survives_last_key_delete() {
    use crate::crypto::{KeyCipher, StaticKeyCipher};
    use crate::db::Database;
    use crate::models::{AccountCustomConfigInput, AccountModelCapabilityInput};
    use crate::provider::UpstreamProtocolKind;
    use crate::state::CoreStateInner;
    use ocg_domain::connection::{LegacyConnectionKind, connection_id_for_legacy};
    use ocg_domain::destination::LegacyDestinationRef;
    use std::fs;
    use std::sync::Arc;

    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "ocg-onboard-custom-second-key-{}",
        uuid::Uuid::new_v4()
    ));
    fs::create_dir_all(&dir).unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> =
        Arc::new(StaticKeyCipher::new("onboard-custom-second-key"));
    let db = Database::open(dir.clone()).unwrap();
    let first = dynamic_provider_account(
        DynamicAuthKind::Bearer,
        CUSTOM_PROVIDER_ID,
        "Primary".into(),
        cipher.encrypt("sk-primary").unwrap(),
        None,
        Utc::now(),
    );
    db.create_account_with_contract(
        &first,
        Some(&AccountCustomConfigInput {
            endpoint_url: "https://legacy-custom.example/v1/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        }),
        &[AccountModelCapabilityInput {
            public_model: "legacy-public".into(),
            upstream_model: "vendor/raw".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: None,
        }],
    )
    .unwrap();
    let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());
    let projection = crate::destination_projection::load_persisted(&state.db.lock()).unwrap();
    let destination = projection
        .destinations
        .iter()
        .find(
            |row| matches!(&row.legacy, LegacyDestinationRef::CustomAccount(id) if id == &first.id),
        )
        .unwrap()
        .clone();
    assert_eq!(destination.max_credentials, None);
    let connection_id = connection_id_for_legacy(LegacyConnectionKind::CustomAccount, &first.id);
    let request = OnboardingCommitRequest {
        expectation: MutationExpectation {
            expected_revision: state.settings_revision(),
            process_generation: state.process_generation(),
        },
        operation_id: "99999999-1111-4111-8111-111111111111".into(),
        connection: OnboardingConnection::Existing(OnboardingConnectionExisting {
            connection_id: connection_id.to_string(),
            configuration: None,
        }),
        authorization: Some(OnboardingAuthorization::ApiKey(
            OnboardingAuthorizationApiKey {
                secret_input: "sk-secondary".into(),
                account_label: Some("Secondary".into()),
                notes: None,
            },
        )),
        targets: Vec::new(),
        mode: None,
        authorize_current_endpoint: false,
    };
    let result = commit_locked(&state, request)
        .map_err(|error| error.into_response().status())
        .unwrap();
    let second_id = result.account_id.unwrap();
    let after = crate::destination_projection::load_persisted(&state.db.lock()).unwrap();
    let attached: Vec<_> = after
        .credentials
        .iter()
        .filter(|credential| credential.destination_id == destination.id)
        .collect();
    assert_eq!(attached.len(), 2);
    assert!(
        attached
            .iter()
            .any(|credential| credential.legacy_account_id == first.id)
    );
    assert!(
        attached
            .iter()
            .any(|credential| credential.legacy_account_id == second_id)
    );
    let connections = match crate::dashboard_v4::connections::list_connections(
        axum::extract::State(state.clone()),
    )
    .await
    {
        Ok(value) => value.0,
        Err(_) => panic!("connection projection unexpectedly failed"),
    };
    let grouped = connections
        .connections
        .iter()
        .filter(|connection| {
            connection.legacy.kind == LegacyConnectionKind::CustomAccount
                && connection.legacy.id == first.id
        })
        .collect::<Vec<_>>();
    assert_eq!(grouped.len(), 1);
    assert_eq!(grouped[0].credential_count, 2);

    state.db.lock().delete_account(&first.id).unwrap();
    state.db.lock().delete_account(&second_id).unwrap();
    let empty = crate::destination_projection::load_persisted(&state.db.lock()).unwrap();
    assert!(
        empty
            .destinations
            .iter()
            .any(|row| row.id == destination.id)
    );
    assert!(
        empty
            .credentials
            .iter()
            .all(|row| row.destination_id != destination.id)
    );
    drop(state);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn resume_returns_stored_nondeterministic_credential_id_and_replays() {
    use crate::crypto::{KeyCipher, StaticKeyCipher};
    use crate::db::Database;
    use crate::state::CoreStateInner;
    use ocg_domain::credential::credential_id_for_legacy_account;
    use std::fs;
    use std::sync::Arc;

    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "ocg-onboard-imported-cred-{}",
        uuid::Uuid::new_v4()
    ));
    fs::create_dir_all(&dir).unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> =
        Arc::new(StaticKeyCipher::new("onboard-imported-cred"));
    let state = Arc::new(
        CoreStateInner::new(Database::open(dir.clone()).unwrap(), dir.clone(), cipher).unwrap(),
    );
    let draft = OnboardingCommitRequest {
        expectation: MutationExpectation {
            expected_revision: state.settings_revision(),
            process_generation: state.process_generation(),
        },
        operation_id: "aaaaaaaa-bbbb-4ccc-8ddd-0000000000a1".into(),
        connection: OnboardingConnection::New(OnboardingConnectionNew {
            template_id: "custom-http".into(),
            name: "Imported Cred".into(),
            endpoint_url: "https://imported-cred.example/v1/chat/completions".into(),
            upstream_protocol: AccountUpstreamProtocol::ChatCompletions,
            auth_kind: ProviderDefinitionAuthKind::Bearer,
            protocol_routes: None,
        }),
        authorization: Some(OnboardingAuthorization::ApiKey(
            OnboardingAuthorizationApiKey {
                secret_input: "sk-imported-retain".into(),
                account_label: Some("Imported".into()),
                notes: None,
            },
        )),
        targets: vec![OnboardingTarget {
            public_model: "lab-opus".into(),
            upstream_model: "vendor/opus".into(),
            upstream_override: None,
        }],
        mode: Some(OnboardingCommitMode::Draft),
        authorize_current_endpoint: false,
    };
    let drafted = commit_locked(&state, draft)
        .map_err(|error| error.into_response().status())
        .unwrap();
    let account_id = drafted.account_id.clone().unwrap();
    let derived = credential_id_for_legacy_account(&account_id).to_string();
    assert_eq!(drafted.credential_id.as_deref(), Some(derived.as_str()));
    let imported = "ffffffff-eeee-4ddd-8ccc-000000000099".to_string();
    assert_ne!(imported, derived);
    state
        .db
        .lock()
        .replace_credential_id(&account_id, &imported)
        .unwrap();
    let before = state.db.lock().list_identity_model().unwrap();
    let prior = before
        .accounts
        .iter()
        .find(|row| row.account.id == account_id)
        .unwrap()
        .clone();
    assert_eq!(prior.credential_id, imported);
    let complete = OnboardingCommitRequest {
        expectation: MutationExpectation {
            expected_revision: state.settings_revision(),
            process_generation: state.process_generation(),
        },
        operation_id: "aaaaaaaa-bbbb-4ccc-8ddd-0000000000a2".into(),
        connection: OnboardingConnection::Existing(OnboardingConnectionExisting {
            connection_id: drafted.connection_id.clone(),
            configuration: Some(OnboardingConnectionNew {
                template_id: "custom-http".into(),
                name: "Imported Cred".into(),
                endpoint_url: "https://imported-cred.example/v1/chat/completions".into(),
                upstream_protocol: AccountUpstreamProtocol::ChatCompletions,
                auth_kind: ProviderDefinitionAuthKind::Bearer,
                protocol_routes: None,
            }),
        }),
        authorization: None,
        targets: vec![OnboardingTarget {
            public_model: "lab-opus".into(),
            upstream_model: "vendor/opus".into(),
            upstream_override: None,
        }],
        mode: Some(OnboardingCommitMode::Complete),
        authorize_current_endpoint: false,
    };
    let completed = commit_locked(&state, complete.clone())
        .map_err(|error| error.into_response().status())
        .unwrap();
    assert_eq!(completed.credential_id.as_deref(), Some(imported.as_str()));
    assert_eq!(completed.account_id.as_deref(), Some(account_id.as_str()));
    let replayed = commit_locked(&state, complete)
        .map_err(|error| error.into_response().status())
        .unwrap();
    assert!(replayed.replayed);
    assert_eq!(replayed.credential_id.as_deref(), Some(imported.as_str()));
    assert_eq!(replayed.account_id.as_deref(), Some(account_id.as_str()));
    let after = state.db.lock().list_identity_model().unwrap();
    let kept = after
        .accounts
        .iter()
        .find(|row| row.account.id == account_id)
        .unwrap();
    assert_eq!(kept.credential_id, imported);
    assert_eq!(kept.identity_id, prior.identity_id);
    assert_eq!(kept.binding_id, prior.binding_id);
    drop(state);
    fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn explicit_routes_persist_before_first_and_second_key_safe_grants() {
    use super::super::types::{AuthSchemeDto, HttpProtocolRouteDto, ProtocolDto};
    use crate::crypto::StaticKeyCipher;
    use crate::db::Database;
    use crate::state::CoreStateInner;
    use std::sync::Arc;
    let dir = std::env::temp_dir().join(format!("ocg-onboard-routes-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let state = Arc::new(
        CoreStateInner::new(
            Database::open(dir.clone()).unwrap(),
            dir.clone(),
            Arc::new(StaticKeyCipher::new("routes")),
        )
        .unwrap(),
    );
    let mut input = sample_request(
        state.settings_revision(),
        state.process_generation(),
        "synthetic-key",
    );
    let OnboardingConnection::New(connection) = &mut input.connection else {
        unreachable!()
    };
    connection.protocol_routes = Some(vec![
        HttpProtocolRouteDto {
            protocol: ProtocolDto::ChatCompletions,
            endpoint_url: connection.endpoint_url.clone(),
            auth_scheme: AuthSchemeDto::Bearer,
        },
        HttpProtocolRouteDto {
            protocol: ProtocolDto::Messages,
            endpoint_url: "https://lab.example/anthropic/v1/messages".into(),
            auth_scheme: AuthSchemeDto::XApiKey,
        },
        HttpProtocolRouteDto {
            protocol: ProtocolDto::Responses,
            endpoint_url: "https://separate.example/v1/responses".into(),
            auth_scheme: AuthSchemeDto::Bearer,
        },
    ]);
    let first = commit_locked(&state, input)
        .map_err(|error| error.into_response().status())
        .unwrap();
    let before = crate::destination_projection::load_persisted(&state.db.lock()).unwrap();
    let first_credential = before
        .credentials
        .iter()
        .find(|row| Some(&row.legacy_account_id) == first.account_id.as_ref())
        .unwrap();
    let destination = before
        .destinations
        .iter()
        .find(|row| row.id == first_credential.destination_id)
        .unwrap();
    assert_eq!(destination.protocol_routes.len(), 3);
    assert_eq!(destination.catalog[0].protocols.len(), 3);
    assert!(destination.catalog[0].enabled);
    assert_eq!(first_credential.grants.allowed_endpoint_ids.len(), 2);
    assert_eq!(
        first_credential.grants.allowed_origins,
        ["https://lab.example"]
    );
    let second = OnboardingCommitRequest {
        expectation: MutationExpectation {
            expected_revision: state.settings_revision(),
            process_generation: state.process_generation(),
        },
        operation_id: uuid::Uuid::new_v4().to_string(),
        connection: OnboardingConnection::Existing(OnboardingConnectionExisting {
            connection_id: first.connection_id.clone(),
            configuration: None,
        }),
        authorization: Some(OnboardingAuthorization::ApiKey(
            OnboardingAuthorizationApiKey {
                secret_input: "second-synthetic-key".into(),
                account_label: Some("Second".into()),
                notes: None,
            },
        )),
        targets: Vec::new(),
        mode: None,
        authorize_current_endpoint: false,
    };
    let second = commit_locked(&state, second)
        .map_err(|error| error.into_response().status())
        .unwrap();
    let after = crate::destination_projection::load_persisted(&state.db.lock()).unwrap();
    let second_credential = after
        .credentials
        .iter()
        .find(|row| Some(&row.legacy_account_id) == second.account_id.as_ref())
        .unwrap();
    assert_eq!(second_credential.grants, first_credential.grants);
    let connections = crate::dashboard_v4::connections::list_connections(State(state.clone()))
        .await
        .map_err(|error| error.into_response().status())
        .unwrap()
        .0;
    let connection = connections
        .connections
        .iter()
        .find(|row| row.id == first.connection_id)
        .unwrap();
    assert_eq!(connection.endpoints.len(), 3);
    assert_eq!(connection.targets[0].endpoint_ids.len(), 3);
    drop(state);
    let reopened = Database::open(dir.clone()).unwrap();
    let persisted = crate::destination_projection::load_persisted(&reopened).unwrap();
    assert_eq!(
        persisted
            .destinations
            .iter()
            .find(|row| row.id == destination.id)
            .unwrap()
            .protocol_routes,
        destination.protocol_routes
    );
    drop(reopened);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn postcommit_publication_failure_then_replay_keeps_one_partial_receipt() {
    use crate::crypto::{KeyCipher, StaticKeyCipher};
    use crate::db::Database;
    use crate::state::CoreStateInner;
    use axum::extract::State;
    use std::sync::Arc;

    let dir = std::env::temp_dir().join(format!("ocg-onboard-postcommit-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> =
        Arc::new(StaticKeyCipher::new("onboard-postcommit"));
    let state = Arc::new(
        CoreStateInner::new(Database::open(dir.clone()).unwrap(), dir.clone(), cipher).unwrap(),
    );
    let business_id = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    let cas_id = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";

    let mut invalid = sample_request(
        state.settings_revision(),
        state.process_generation(),
        "sk-invalid",
    );
    invalid.operation_id = "not-a-uuid".into();
    let rejected = commit(State(state.clone()), body_of(&invalid))
        .await
        .unwrap_err();
    let rejected_http = error_json(rejected).await;
    assert_eq!(rejected_http["code"], "invalidRequest");
    let rows = super::super::applications::operation_receipts(&state);
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].outcome,
        crate::log_types::OperationOutcome::Rejected
    );
    assert_ne!(rows[0].operation_id, "not-a-uuid");
    assert!(rows[0].metadata.revision.is_none());

    let mut stale = sample_request(
        state.settings_revision().wrapping_add(9),
        state.process_generation(),
        "sk-stale",
    );
    stale.operation_id = cas_id.into();
    if let OnboardingConnection::New(connection) = &mut stale.connection {
        connection.name = "Rejected".into();
    }
    let cas = commit(State(state.clone()), body_of(&stale))
        .await
        .unwrap_err();
    let cas_http = error_json(cas).await;
    assert_eq!(cas_http["code"], "revisionConflict");
    assert!(
        state
            .db
            .lock()
            .list_control_plane_dynamic_providers()
            .unwrap()
            .iter()
            .all(|provider| provider.name != "Rejected")
    );
    let rows = super::super::applications::operation_receipts(&state);
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| row.operation_id != cas_id));
    assert!(
        rows.iter()
            .all(|row| row.outcome != crate::log_types::OperationOutcome::Partial)
    );

    let poisoned = state
        .db
        .lock()
        .conn
        .execute(
            "UPDATE credentials SET quota_recovery_json = '{not-json}'
             WHERE rowid = (SELECT rowid FROM credentials ORDER BY id LIMIT 1)",
            [],
        )
        .unwrap();
    assert!(poisoned >= 1);
    let mut request = sample_request(
        state.settings_revision(),
        state.process_generation(),
        "sk-publication",
    );
    request.operation_id = business_id.into();
    if let OnboardingConnection::New(connection) = &mut request.connection {
        connection.name = "After".into();
    }
    let bytes = body_of(&request);
    let failed = commit(State(state.clone()), bytes.clone())
        .await
        .unwrap_err();
    let failed_http = error_json(failed).await;
    assert_eq!(failed_http["code"], "internal");
    let message = failed_http["message"].as_str().unwrap().to_string();
    assert!(!message.is_empty());
    let ledger = state
        .db
        .lock()
        .find_dashboard_operation(business_id)
        .unwrap()
        .unwrap();
    let stored: StoredOnboardingCommitResult = serde_json::from_str(&ledger.result_json).unwrap();
    assert!(
        state
            .db
            .lock()
            .list_control_plane_dynamic_providers()
            .unwrap()
            .iter()
            .any(|provider| provider.name == "After")
    );
    let receipt_id = stored.receipt_id.clone().unwrap();
    assert_ne!(receipt_id, business_id);
    let partial = partial_receipt(&state);
    assert_eq!(partial.operation_id, receipt_id);
    assert_eq!(partial.action, "onboarding.commit");
    assert_eq!(partial.reason_code.as_deref(), Some("internal"));
    assert_eq!(
        partial.subject_id.as_deref(),
        Some(stored.connection_id.as_str())
    );
    assert_eq!(partial.metadata.completed_count, Some(1));
    assert_eq!(partial.metadata.failed_count, Some(1));
    assert!(partial.metadata.revision.is_none());
    assert!(
        partial
            .metadata
            .related_ids
            .contains(stored.credential_id.as_ref().unwrap())
    );
    assert!(
        partial
            .metadata
            .related_ids
            .contains(stored.account_id.as_ref().unwrap())
    );
    let encoded = serde_json::to_string(&partial.metadata).unwrap();
    assert!(!encoded.contains(&message));
    assert!(!encoded.contains("{not-json}"));
    assert!(!encoded.contains("sk-publication"));
    let before_replay = partial.clone();

    let replayed = commit(State(state.clone()), bytes).await.unwrap().0;
    assert!(replayed.replayed);
    assert_eq!(replayed.connection_id, stored.connection_id);
    assert_eq!(replayed.account_id, stored.account_id);
    assert!(
        serde_json::to_value(&replayed)
            .unwrap()
            .get("receiptId")
            .is_none()
    );
    let rows = super::super::applications::operation_receipts(&state);
    assert_eq!(
        rows.iter()
            .filter(|row| row.outcome == crate::log_types::OperationOutcome::Partial)
            .count(),
        1
    );
    assert_eq!(partial_receipt(&state), before_replay);
    assert!(
        state
            .db
            .lock()
            .list_control_plane_dynamic_providers()
            .unwrap()
            .iter()
            .any(|provider| provider.name == "After")
    );

    let mut mismatch = request;
    mismatch.authorization = Some(OnboardingAuthorization::ApiKey(
        OnboardingAuthorizationApiKey {
            secret_input: "sk-other-payload".into(),
            account_label: Some("Other".into()),
            notes: None,
        },
    ));
    let mismatch_error = commit(State(state.clone()), body_of(&mismatch))
        .await
        .unwrap_err();
    let mismatch_http = error_json(mismatch_error).await;
    assert_eq!(mismatch_http["code"], "operationPayloadMismatch");
    let rows = super::super::applications::operation_receipts(&state);
    assert_eq!(partial_receipt(&state), before_replay);
    assert_eq!(
        rows.iter()
            .filter(|row| row.operation_id == receipt_id)
            .count(),
        1
    );
    assert_eq!(
        rows.iter()
            .filter(|row| row.operation_id == business_id)
            .count(),
        0
    );
    let mismatch_row = rows
        .iter()
        .find(|row| {
            row.outcome == crate::log_types::OperationOutcome::Rejected
                && row.reason_code.as_deref() == Some("operation.payload.mismatch")
        })
        .unwrap();
    assert_ne!(mismatch_row.operation_id, business_id);
    assert_ne!(mismatch_row.operation_id, receipt_id);
    assert_ne!(mismatch_row.operation_id, cas_id);

    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn private_receipt_identity_survives_spelling_expiry_and_audit_reuse() {
    use crate::crypto::{KeyCipher, StaticKeyCipher};
    use crate::db::Database;
    use crate::state::CoreStateInner;
    use axum::extract::State;
    use std::sync::Arc;

    let dir = std::env::temp_dir().join(format!("ocg-onboard-identity-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> =
        Arc::new(StaticKeyCipher::new("onboard-identity"));
    let state = Arc::new(
        CoreStateInner::new(Database::open(dir.clone()).unwrap(), dir.clone(), cipher).unwrap(),
    );
    let upper = "AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA";
    let lower = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let canonical = uuid::Uuid::parse_str(upper).unwrap().to_string();
    assert_eq!(canonical, uuid::Uuid::parse_str(lower).unwrap().to_string());

    let mut stale = named_request(&state, upper, "Stale", "sk-stale-identity");
    stale.expectation.expected_revision = state.settings_revision().wrapping_add(9);
    let stale_error = commit(State(state.clone()), body_of(&stale))
        .await
        .unwrap_err();
    assert_eq!(error_json(stale_error).await["code"], "revisionConflict");
    let rejected = super::super::applications::operation_receipts(&state);
    assert_eq!(rejected.len(), 1);
    assert_eq!(
        rejected[0].outcome,
        crate::log_types::OperationOutcome::Rejected
    );
    assert_ne!(rejected[0].operation_id, upper);
    assert_ne!(rejected[0].operation_id, canonical);

    let upper_request = named_request(&state, upper, "Upper", "sk-upper");
    let upper_bytes = body_of(&upper_request);
    let upper_result = commit(State(state.clone()), upper_bytes.clone())
        .await
        .unwrap()
        .0;
    assert!(!upper_result.replayed);
    assert!(public_has_no_receipt_id(&upper_result));
    let upper_row = state
        .db
        .lock()
        .find_dashboard_operation(upper)
        .unwrap()
        .unwrap();
    assert!(
        state
            .db
            .lock()
            .find_dashboard_operation(lower)
            .unwrap()
            .is_none()
    );
    let upper_stored: StoredOnboardingCommitResult =
        serde_json::from_str(&upper_row.result_json).unwrap();
    let upper_receipt = upper_stored.receipt_id.clone().unwrap();
    assert_ne!(upper_receipt, upper);
    assert_ne!(upper_receipt, canonical);
    assert_success_ids(&state, &[&upper_receipt]);

    let lower_result = commit(
        State(state.clone()),
        body_of(&named_request(&state, lower, "Lower", "sk-lower")),
    )
    .await
    .unwrap()
    .0;
    assert!(!lower_result.replayed);
    assert!(public_has_no_receipt_id(&lower_result));
    let lower_row = state
        .db
        .lock()
        .find_dashboard_operation(lower)
        .unwrap()
        .unwrap();
    let lower_stored: StoredOnboardingCommitResult =
        serde_json::from_str(&lower_row.result_json).unwrap();
    let lower_receipt = lower_stored.receipt_id.unwrap();
    assert_ne!(lower_receipt, upper_receipt);
    assert_ne!(lower_receipt, canonical);
    assert_success_ids(&state, &[&lower_receipt, &upper_receipt]);

    let replayed = commit(State(state.clone()), upper_bytes).await.unwrap().0;
    assert!(replayed.replayed);
    assert_eq!(replayed.connection_id, upper_result.connection_id);
    assert!(public_has_no_receipt_id(&replayed));
    assert_success_ids(&state, &[&lower_receipt, &upper_receipt]);
    let upper_log = receipt_by_id(&state, &upper_receipt);
    assert_eq!(
        upper_log.outcome,
        crate::log_types::OperationOutcome::Success
    );

    let deleted = state
        .db
        .lock()
        .conn
        .execute(
            "DELETE FROM dashboard_operations WHERE operation_id = ?1",
            [upper],
        )
        .unwrap();
    assert_eq!(deleted, 1);
    assert!(
        state
            .db
            .lock()
            .find_dashboard_operation(upper)
            .unwrap()
            .is_none()
    );
    let reused = commit(
        State(state.clone()),
        body_of(&named_request(&state, upper, "Reused", "sk-reused")),
    )
    .await
    .unwrap()
    .0;
    assert!(!reused.replayed);
    assert_ne!(reused.connection_id, upper_result.connection_id);
    let reused_row = state
        .db
        .lock()
        .find_dashboard_operation(upper)
        .unwrap()
        .unwrap();
    let reused_receipt =
        serde_json::from_str::<StoredOnboardingCommitResult>(&reused_row.result_json)
            .unwrap()
            .receipt_id
            .unwrap();
    assert_ne!(reused_receipt, upper_receipt);
    assert_eq!(receipt_by_id(&state, &upper_receipt), upper_log);
    assert_eq!(
        state
            .db
            .lock()
            .list_control_plane_dynamic_providers()
            .unwrap()
            .iter()
            .filter(|provider| provider.name == "Upper" || provider.name == "Reused")
            .count(),
        2
    );

    let unrelated = commit(
        State(state.clone()),
        body_of(&named_request(
            &state,
            &upper_receipt,
            "Unrelated",
            "sk-unrelated",
        )),
    )
    .await
    .unwrap()
    .0;
    assert!(!unrelated.replayed);
    let unrelated_row = state
        .db
        .lock()
        .find_dashboard_operation(&upper_receipt)
        .unwrap()
        .unwrap();
    let unrelated_receipt =
        serde_json::from_str::<StoredOnboardingCommitResult>(&unrelated_row.result_json)
            .unwrap()
            .receipt_id
            .unwrap();
    assert_ne!(unrelated_receipt, upper_receipt);
    assert_eq!(receipt_by_id(&state, &upper_receipt), upper_log);
    assert_eq!(
        super::super::applications::operation_receipts(&state)
            .iter()
            .filter(|row| row.operation_id == upper_receipt)
            .count(),
        1
    );

    let legacy_raw = "DdDdDdDd-dddd-4ddd-8ddd-dddddddddddd";
    let legacy_request = named_request(&state, legacy_raw, "Legacy", "sk-legacy");
    commit_locked(&state, legacy_request.clone()).unwrap();
    let legacy_row = state
        .db
        .lock()
        .find_dashboard_operation(legacy_raw)
        .unwrap()
        .unwrap();
    assert_eq!(legacy_row.operation_id, legacy_raw);
    let expected = legacy_onboarding_receipt_id(
        &legacy_row.kind,
        &legacy_row.operation_id,
        &legacy_row.created_at,
    );
    assert_eq!(
        expected,
        legacy_onboarding_receipt_id(
            &legacy_row.kind,
            &legacy_row.operation_id,
            &legacy_row.created_at,
        )
    );
    assert_ne!(
        expected,
        legacy_onboarding_receipt_id(
            &legacy_row.kind,
            &legacy_raw.to_ascii_lowercase(),
            &legacy_row.created_at,
        )
    );
    assert_ne!(
        expected,
        legacy_onboarding_receipt_id(
            &legacy_row.kind,
            &legacy_row.operation_id,
            &format!("{} ", legacy_row.created_at),
        )
    );
    assert_ne!(
        expected,
        legacy_onboarding_receipt_id("other", &legacy_row.operation_id, &legacy_row.created_at)
    );
    let legacy_bytes = expected.as_bytes();
    assert_eq!(legacy_bytes[6] >> 4, 8);
    assert_eq!(legacy_bytes[8] & 0b1100_0000, 0b1000_0000);
    let mut legacy_stored: StoredOnboardingCommitResult =
        serde_json::from_str(&legacy_row.result_json).unwrap();
    legacy_stored.receipt_id = None;
    assert_eq!(receipt_uuid_for(&legacy_stored, &legacy_row), expected);
    legacy_stored.receipt_id = Some("not-a-uuid".to_string());
    assert_eq!(receipt_uuid_for(&legacy_stored, &legacy_row), expected);
    let mut legacy_json: serde_json::Value = serde_json::from_str(&legacy_row.result_json).unwrap();
    assert!(legacy_json.get("receiptId").is_some());
    legacy_json.as_object_mut().unwrap().remove("receiptId");
    state
        .db
        .lock()
        .conn
        .execute(
            "UPDATE dashboard_operations SET result_json = ?1 WHERE operation_id = ?2",
            rusqlite::params![legacy_json.to_string(), legacy_raw],
        )
        .unwrap();
    let legacy_result = commit(State(state.clone()), body_of(&legacy_request))
        .await
        .unwrap()
        .0;
    assert!(legacy_result.replayed);
    assert!(public_has_no_receipt_id(&legacy_result));
    let legacy_log = receipt_by_id(&state, &expected.to_string());
    assert_eq!(legacy_log.action, "onboarding.commit");
    assert_eq!(
        legacy_log.outcome,
        crate::log_types::OperationOutcome::Success
    );
    assert_ne!(legacy_log.operation_id, legacy_raw);
    assert_ne!(
        legacy_log.operation_id,
        uuid::Uuid::parse_str(legacy_raw).unwrap().to_string()
    );
    let again = commit(State(state.clone()), body_of(&legacy_request))
        .await
        .unwrap()
        .0;
    assert!(again.replayed);
    assert_eq!(again.connection_id, legacy_result.connection_id);
    assert_eq!(receipt_by_id(&state, &expected.to_string()), legacy_log);

    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}

fn named_request(
    state: &CoreState,
    operation_id: &str,
    name: &str,
    secret: &str,
) -> OnboardingCommitRequest {
    let mut request = sample_request(
        state.settings_revision(),
        state.process_generation(),
        secret,
    );
    request.operation_id = operation_id.to_string();
    if let OnboardingConnection::New(connection) = &mut request.connection {
        connection.name = name.to_string();
    }
    request
}

fn public_has_no_receipt_id(result: &OnboardingCommitResult) -> bool {
    serde_json::to_value(result)
        .unwrap()
        .get("receiptId")
        .is_none()
}

fn assert_success_ids(state: &CoreState, expected: &[&str]) {
    let mut expected: Vec<String> = expected.iter().map(|id| (*id).to_string()).collect();
    expected.sort();
    let mut actual: Vec<String> = super::super::applications::operation_receipts(state)
        .into_iter()
        .filter(|row| {
            row.action == "onboarding.commit"
                && row.outcome == crate::log_types::OperationOutcome::Success
        })
        .map(|row| row.operation_id)
        .collect();
    actual.sort();
    assert_eq!(actual, expected);
}

fn receipt_by_id(state: &CoreState, operation_id: &str) -> crate::log_types::OperationLog {
    super::super::applications::operation_receipts(state)
        .into_iter()
        .find(|row| row.operation_id == operation_id)
        .unwrap()
}

fn body_of(request: &OnboardingCommitRequest) -> axum::body::Bytes {
    axum::body::Bytes::from(serde_json::to_vec(request).unwrap())
}

fn partial_receipt(state: &CoreState) -> crate::log_types::OperationLog {
    super::super::applications::operation_receipts(state)
        .into_iter()
        .find(|row| row.outcome == crate::log_types::OperationOutcome::Partial)
        .unwrap()
}

async fn error_json(error: V3ApiError) -> serde_json::Value {
    use axum::response::IntoResponse;
    let response = error.into_response();
    let bytes = axum::body::to_bytes(response.into_body(), 65536)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}
