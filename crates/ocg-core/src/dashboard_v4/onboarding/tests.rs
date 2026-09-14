use super::super::types::{OnboardingAuthorizationApiKey, OnboardingConnectionExisting};
use super::*;
use crate::dashboard_v3::{
    AccountUpstreamProtocol, MutationExpectation, ProviderDefinitionAuthKind,
};
use axum::response::IntoResponse;

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
