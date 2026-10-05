use super::super::store::{OAuthPresence, Record};
use super::*;
use crate::provider::CPA_PROVIDER_ID;

fn selection() -> artifact::SyntheticSelection {
    artifact::synthetic_selection().expect("compile platform")
}

fn accept_selected(
    body: &str,
    record: &Record,
    selected: &artifact::SyntheticSelection,
) -> Result<AcceptedReady, ExecutionError> {
    accept_ready_for_lock(
        body,
        record,
        &secrets(),
        &selected.lock,
        selected.platform,
        selected.variant,
    )
}

fn restore_selected(
    body: &str,
    record: &Record,
    selected: &artifact::SyntheticSelection,
) -> Result<AcceptedReady, ExecutionError> {
    accept_restored_for_lock(
        body,
        record,
        &secrets(),
        &selected.lock,
        selected.platform,
        selected.variant,
    )
}

fn secrets() -> super::super::Secrets {
    super::super::Secrets {
        hop: super::super::Secret("hop-secret-value".into()),
        policy: super::super::Secret("policy-secret-value".into()),
        ready: super::super::Secret("ready-secret-value".into()),
    }
}

fn ready_record(sha: &str) -> Record {
    let mut record = Record::empty();
    record.child_generation = 6;
    record.desired_revision = 8;
    record.applied_revision = 3;
    record.applied_generation = 4;
    record.desired_digest = "ab".repeat(32);
    record.applied_digest = "cd".repeat(32);
    record.artifact_sha256 = sha.to_string();
    record
}

fn applied_body(record: &Record) -> String {
    let mut value = serde_json::from_str::<serde_json::Value>(&synthetic_ready(record)).unwrap();
    value["processGeneration"] = serde_json::json!(record.applied_generation.to_string());
    value["appliedProjectionRevision"] = serde_json::json!(record.applied_revision.to_string());
    value["desiredProjectionRevision"] = serde_json::json!(record.applied_revision.to_string());
    value["appliedProjectionDigest"] = serde_json::json!(record.applied_digest);
    value["desiredProjectionDigest"] = serde_json::json!(record.applied_digest);
    value.to_string()
}

#[test]
fn synthetic_ready_and_accept_ready_share_the_required_capability_names() {
    let selected = selection();
    let mut record = Record::empty();
    record.child_generation = 1;
    record.desired_revision = 1;
    record.desired_digest = "aa".repeat(32);
    record.artifact_sha256 = selected.selected_sha.clone();
    let body = synthetic_ready(&record);
    assert!(body.contains(&record.artifact_sha256));
    assert!(!body.contains(artifact::PINNED_SHA256));
    assert_eq!(REQUIRED_CAPABILITIES.len(), 16);
    assert!(body.contains("native-refresh-registration-fence-v1"));
    assert!(body.contains("validated-protocol-pin-v1"));
    assert!(body.contains("validation-only-routes-v1"));
    assert!(body.contains("absolute-request-deadline-v1"));
    assert!(body.contains("native-final-endpoint-pin-v1"));
    let accepted = accept_selected(&body, &record, &selected).expect("current ready");
    assert!(accepted.oauth.is_empty());
    assert_eq!(accepted.capabilities.len(), 16);
    assert_eq!(
        accepted.capabilities,
        REQUIRED_CAPABILITIES
            .iter()
            .map(|name| (*name).to_string())
            .collect::<Vec<_>>()
    );
    let mut value = serde_json::from_str::<serde_json::Value>(&body).unwrap();
    value["artifact"]["capabilities"]
        .as_array_mut()
        .unwrap()
        .pop();
    let error = accept_selected(&value.to_string(), &record, &selected).expect_err("short set");
    assert!(
        error
            .to_string()
            .contains("ready capability set is incomplete")
    );
}

#[test]
fn accept_restored_matches_the_applied_tuple_and_rejects_the_failed_one() {
    let selected = selection();
    let record = ready_record(&selected.selected_sha);
    let restored = applied_body(&record);
    assert!(restored_projection_matches(&restored, &record));
    restore_selected(&restored, &record, &selected).expect("previous applied ready");
    let failed = synthetic_ready(&record);
    assert!(!restored_projection_matches(&failed, &record));
    let error = restore_selected(&failed, &record, &selected).expect_err("failed desired body");
    assert!(error.to_string().contains("cpa_apply_failed"));
}

#[test]
fn ready_refs_keep_unbound_entries_and_reject_an_unsafe_path() {
    let selected = selection();
    let mut record = Record::empty();
    record.child_generation = 1;
    record.desired_revision = 1;
    record.desired_digest = "aa".repeat(32);
    record.artifact_sha256 = selected.selected_sha.clone();
    let mut value = serde_json::from_str::<serde_json::Value>(&synthetic_ready(&record)).unwrap();
    value["authRefs"] = serde_json::json!([{
        "relativePath": "codex.json",
        "provider": "cpa",
        "providerId": "cpa",
        "rawProviderLabel": "codex",
        "effectiveSubtype": "codex",
        "effectiveMode": "",
        "effectiveGenerationBase": "",
        "models": []
    }]);
    let accepted = accept_selected(&value.to_string(), &record, &selected).expect("unbound ref");
    assert_eq!(accepted.oauth.len(), 1);
    assert_eq!(accepted.oauth[0].presence, OAuthPresence::Pending);
    assert_eq!(accepted.oauth[0].provider_id, CPA_PROVIDER_ID);
    assert_eq!(accepted.oauth[0].native_provider, "codex");
    assert_eq!(accepted.oauth[0].raw_provider_label, "codex");
    assert!(accepted.oauth[0].native_mode.is_empty());
    assert_eq!(
        accepted.oauth[0].reported_base,
        "https://chatgpt.com/backend-api/codex"
    );
    assert!(accepted.oauth[0].credential_id.is_empty());

    value["authRefs"] = serde_json::json!([{
        "relativePath": "../token.json",
        "provider": "kimi"
    }]);
    let malformed =
        accept_selected(&value.to_string(), &record, &selected).expect_err("unsafe path");
    assert!(
        malformed
            .to_string()
            .contains("ready auth refs are invalid")
    );

    let mut missing = serde_json::from_str::<serde_json::Value>(&synthetic_ready(&record)).unwrap();
    missing.as_object_mut().unwrap().remove("authRefs");
    let incomplete =
        accept_selected(&missing.to_string(), &record, &selected).expect("missing refs");
    assert!(incomplete.oauth.is_empty());
}

#[test]
fn selected_digest_is_required_and_the_placeholder_is_not_authority() {
    let selected = selection();
    let record = ready_record(&selected.selected_sha);
    let body = synthetic_ready(&record);
    accept_selected(&body, &record, &selected).expect("selected digest");
    restore_selected(&applied_body(&record), &record, &selected).expect("restored selected digest");

    let mut mismatched = serde_json::from_str::<serde_json::Value>(&body).unwrap();
    mismatched["artifact"]["executableSHA256"] = serde_json::json!("cd".repeat(32));
    let mismatch = accept_selected(&mismatched.to_string(), &record, &selected)
        .expect_err("mismatched digest");
    assert!(mismatch.to_string().contains("selected digest"));

    let mut upper = serde_json::from_str::<serde_json::Value>(&body).unwrap();
    upper["artifact"]["executableSHA256"] =
        serde_json::json!(record.artifact_sha256.to_ascii_uppercase());
    assert!(accept_selected(&upper.to_string(), &record, &selected).is_err());

    let mut placeholder = record.clone();
    placeholder.artifact_sha256 = artifact::PINNED_SHA256.to_string();
    let planted = synthetic_ready(&placeholder);
    let rejected = accept_selected(&planted, &placeholder, &selected).expect_err("placeholder");
    assert!(rejected.to_string().contains("selected digest"));
    assert_eq!(accepted_capability_count(&body), 16);

    for sha in [
        "ab".repeat(32),
        selected.alternate_sha.clone(),
        selected.other_platform_sha.clone(),
    ] {
        let mut consistent = record.clone();
        consistent.artifact_sha256 = sha;
        let ready = synthetic_ready(&consistent);
        let error = accept_selected(&ready, &consistent, &selected)
            .expect_err("self-consistent untrusted digest");
        assert!(error.to_string().contains("selected digest"), "{error}");
        let restored = applied_body(&consistent);
        let restored_error = restore_selected(&restored, &consistent, &selected)
            .expect_err("restored untrusted digest");
        assert!(
            restored_error.to_string().contains("selected digest"),
            "{restored_error}"
        );
    }
}

#[test]
fn ready_and_restored_reject_a_bad_lock_before_acceptance() {
    let selected = selection();
    let record = ready_record(&selected.selected_sha);
    let body = synthetic_ready(&record);
    let restored = applied_body(&record);
    let wire = match selected.variant {
        artifact::Variant::Production => "production",
        artifact::Variant::NativeLoopbackFixture => "native-loopback-fixture",
    };

    let mut missing = serde_json::from_slice::<serde_json::Value>(&selected.lock).unwrap();
    missing["artifacts"].as_array_mut().unwrap().retain(|item| {
        item["os"].as_str() != Some(selected.platform.os) || item["variant"].as_str() != Some(wire)
    });
    assert_lock_rejected(&missing, "missing", &body, &restored, &record, &selected);

    let mut duplicated = serde_json::from_slice::<serde_json::Value>(&selected.lock).unwrap();
    let extra = duplicated["artifacts"][0].clone();
    duplicated["artifacts"].as_array_mut().unwrap().push(extra);
    assert_lock_rejected(
        &duplicated,
        "duplicated",
        &body,
        &restored,
        &record,
        &selected,
    );

    let mut invalid = serde_json::from_slice::<serde_json::Value>(&selected.lock).unwrap();
    invalid["artifacts"]
        .as_array_mut()
        .unwrap()
        .get_mut(0)
        .unwrap()
        .as_object_mut()
        .unwrap()
        .remove("sha256");
    assert_lock_rejected(
        &invalid,
        "record is invalid",
        &body,
        &restored,
        &record,
        &selected,
    );
}

fn assert_lock_rejected(
    lock: &serde_json::Value,
    needle: &str,
    body: &str,
    restored: &str,
    record: &Record,
    selected: &artifact::SyntheticSelection,
) {
    let bytes = serde_json::to_vec(lock).unwrap();
    let ready = accept_ready_for_lock(
        body,
        record,
        &secrets(),
        &bytes,
        selected.platform,
        selected.variant,
    )
    .unwrap_err()
    .to_string();
    assert!(ready.contains(needle), "{ready}");
    let restored = accept_restored_for_lock(
        restored,
        record,
        &secrets(),
        &bytes,
        selected.platform,
        selected.variant,
    )
    .unwrap_err()
    .to_string();
    assert!(restored.contains(needle), "{restored}");
}

fn accepted_capability_count(body: &str) -> usize {
    serde_json::from_str::<serde_json::Value>(body).unwrap()["artifact"]["capabilities"]
        .as_array()
        .unwrap()
        .len()
}

fn keyed_stamp() -> super::super::store::AuthStamp {
    super::super::store::AuthStamp {
        auth_id: "auth-key".into(),
        credential_id: "cred-key".into(),
        credential_version: 2,
        binding_id: "bind-key".into(),
        material_revision: "material-key".into(),
        provider_id: "provider-key".into(),
        registration_epoch: 0,
    }
}

#[test]
fn in_memory_ready_ref_records_the_keyed_registration_epoch() {
    let mut stamps = vec![keyed_stamp()];
    let body = r#"{
        "authRefs": [
            {
                "authId": "auth-key",
                "credentialId": "cred-key",
                "credentialVersion": "2",
                "materialRevision": "material-key",
                "providerId": "provider-key",
                "registrationEpoch": "2",
                "disabled": false,
                "status": "active",
                "models": ["cli-test-model"]
            },
            {
                "relativePath": "codex/token.json",
                "authId": "auth-key",
                "credentialId": "cred-key",
                "credentialVersion": "2",
                "materialRevision": "material-key",
                "providerId": "provider-key",
                "registrationEpoch": "9",
                "disabled": false,
                "status": "active",
                "models": []
            }
        ]
    }"#;
    note_keyed_registration_epochs(&mut stamps, body).expect("in-memory epoch");
    assert_eq!(stamps[0].registration_epoch, 2);

    let mut mismatched = vec![keyed_stamp()];
    let wrong_material = body.replace("material-key", "other-material");
    note_keyed_registration_epochs(&mut mismatched, &wrong_material).expect("unmatched epoch 0");
    assert_eq!(mismatched[0].registration_epoch, 0);

    let mut ambiguous = vec![keyed_stamp()];
    let doubled = r#"{
        "authRefs": [
            {
                "authId": "auth-key",
                "credentialId": "cred-key",
                "credentialVersion": "2",
                "materialRevision": "material-key",
                "providerId": "provider-key",
                "registrationEpoch": "2",
                "disabled": false,
                "status": "active",
                "models": []
            },
            {
                "authId": "auth-key",
                "credentialId": "cred-key",
                "credentialVersion": "2",
                "materialRevision": "material-key",
                "providerId": "provider-key",
                "registrationEpoch": "3",
                "disabled": false,
                "status": "active",
                "models": []
            }
        ]
    }"#;
    let error = note_keyed_registration_epochs(&mut ambiguous, doubled).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("keyed registration evidence is invalid")
    );
    assert_eq!(ambiguous[0].registration_epoch, 0);
}

fn keyed_ref(epoch: &str) -> String {
    format!(
        r#"{{
            "authId": "auth-key",
            "credentialId": "cred-key",
            "credentialVersion": "2",
            "materialRevision": "material-key",
            "providerId": "provider-key",
            "registrationEpoch": "{epoch}"
        }}"#
    )
}

#[test]
fn malformed_keyed_ref_does_not_adopt_in_either_order() {
    let valid = keyed_ref("2");
    let broken = keyed_ref("nope");
    for (first, second) in [(&valid, &broken), (&broken, &valid)] {
        let mut stamps = vec![keyed_stamp()];
        stamps[0].registration_epoch = 7;
        let body = format!(r#"{{"authRefs":[{first},{second}]}}"#);
        let error = note_keyed_registration_epochs(&mut stamps, &body).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("keyed registration evidence is invalid"),
            "{body}"
        );
        assert_eq!(stamps[0].registration_epoch, 0, "{body}");
    }

    let mut mistyped = vec![keyed_stamp()];
    mistyped[0].registration_epoch = 5;
    let body = r#"{
        "authRefs": [
            {
                "relativePath": 1,
                "authId": "auth-key",
                "credentialId": "cred-key",
                "credentialVersion": "2",
                "materialRevision": "material-key",
                "providerId": "provider-key",
                "registrationEpoch": "2"
            },
            {
                "authId": "auth-key",
                "credentialId": "cred-key",
                "credentialVersion": "2",
                "materialRevision": "material-key",
                "providerId": "provider-key",
                "registrationEpoch": "4"
            }
        ]
    }"#;
    let error = note_keyed_registration_epochs(&mut mistyped, body).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("keyed registration evidence is invalid")
    );
    assert_eq!(mistyped[0].registration_epoch, 0);

    let mut clean = vec![keyed_stamp()];
    clean[0].registration_epoch = 7;
    note_keyed_registration_epochs(&mut clean, &format!(r#"{{"authRefs":[{valid}]}}"#))
        .expect("one in-memory ref");
    assert_eq!(clean[0].registration_epoch, 2);

    let mut missing = vec![keyed_stamp()];
    missing[0].registration_epoch = 7;
    let error = note_keyed_registration_epochs(&mut missing, "{}").unwrap_err();
    assert!(
        error
            .to_string()
            .contains("keyed registration evidence is missing")
    );
    assert_eq!(missing[0].registration_epoch, 0);

    let mut native_only = vec![keyed_stamp()];
    native_only[0].registration_epoch = 7;
    let native = r#"{"authRefs":[{"relativePath":"codex/token.json","authId":"auth-key","credentialId":"cred-key","credentialVersion":"2","materialRevision":"material-key","providerId":"provider-key","registrationEpoch":"9"}]}"#;
    let error = note_keyed_registration_epochs(&mut native_only, native).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("keyed registration evidence is invalid")
    );
    assert_eq!(native_only[0].registration_epoch, 0);

    let mut native_unset = vec![keyed_stamp()];
    note_keyed_registration_epochs(&mut native_unset, native)
        .expect("native ref is not an api stamp");
    assert_eq!(native_unset[0].registration_epoch, 0);
}

#[test]
fn restored_keyed_epoch_updates_only_the_accepted_plane() {
    let body = format!(r#"{{"authRefs":[{}]}}"#, keyed_ref("6"));
    let applied_digest = "a".repeat(64);
    let mut record = super::super::store::Record::empty();
    let mut applied = keyed_stamp();
    applied.registration_epoch = 1;
    let mut desired = keyed_stamp();
    desired.registration_epoch = 8;
    record.applied_auth = vec![applied];
    record.desired_auth = vec![desired];
    record.applied_revision = 4;
    record.applied_digest = applied_digest.clone();
    record.desired_revision = 9;
    record.desired_digest = "b".repeat(64);
    super::note_restored_keyed_epochs(&mut record, &body, 4, &applied_digest)
        .expect("applied plane");
    assert_eq!(record.applied_auth[0].registration_epoch, 6);
    assert_eq!(record.desired_auth[0].registration_epoch, 8);

    record.applied_auth[0].registration_epoch = 1;
    record.desired_auth[0].registration_epoch = 9;
    super::note_applied_keyed_epochs(&mut record, &body).expect("applied counters");
    assert_eq!(record.applied_auth[0].registration_epoch, 6);
    assert_eq!(record.desired_auth[0].registration_epoch, 9);

    let mut other = super::super::store::Record::empty();
    let mut applied = keyed_stamp();
    applied.registration_epoch = 1;
    let mut desired = keyed_stamp();
    desired.registration_epoch = 8;
    other.applied_auth = vec![applied];
    other.desired_auth = vec![desired];
    other.applied_revision = 4;
    other.applied_digest = applied_digest;
    let error =
        super::note_restored_keyed_epochs(&mut other, &body, 3, &"c".repeat(64)).unwrap_err();
    assert!(matches!(
        error,
        super::super::ExecutionError::RollbackUnavailable
    ));
    assert_eq!(other.applied_auth[0].registration_epoch, 1);
    assert_eq!(other.desired_auth[0].registration_epoch, 8);
}
