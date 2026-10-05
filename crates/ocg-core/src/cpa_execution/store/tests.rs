use super::{OAuthPresence, parse, rebase_archived_execution_record};

fn base_record() -> String {
    r#"{
        "version": 1,
        "childGeneration": "1",
        "appliedGeneration": "0",
        "desiredRevision": "2",
        "appliedRevision": "0",
        "desiredDigest": "",
        "appliedDigest": "",
        "artifactSha256": "",
        "listenPort": 0,
        "applyStatus": "not_prepared",
        "desiredRunning": false,
        "publicOrigin": "",
        "ownedOrigin": "",
        "policyReady": false,
        "unavailable": false,
        "desiredAuth": [],
        "appliedAuth": [],
        "oauth": [{
            "relativePath": "codex.json",
            "authId": "codex.json",
            "credentialId": "cid",
            "credentialVersion": "4",
            "materialRevision": "material",
            "providerId": "codex",
            "registrationEpoch": "0",
            "models": ["gpt-5"]
        }]
    }"#
    .to_string()
}

#[test]
fn old_json_defaults_missing_routes_and_migrates_the_executor_label() {
    let record = parse(&base_record()).expect("old record");
    assert!(record.desired_routes.is_empty());
    assert!(record.applied_routes.is_empty());
    assert_eq!(record.oauth[0].provider_id, "cpa");
    assert_eq!(record.oauth[0].native_provider, "codex");
    assert_eq!(record.oauth[0].presence, OAuthPresence::Present);
    assert!(record.oauth[0].recovery.is_empty());
    assert!(record.oauth[0].raw_provider_label.is_empty());
    assert!(record.oauth[0].native_mode.is_empty());
    assert!(record.oauth[0].reported_base.is_empty());
    assert!(record.host_capabilities.is_empty());
    let mut with_caps = base_record();
    with_caps = with_caps.replacen(
        "\"oauth\"",
        "\"hostCapabilities\": [\"validated-protocol-pin-v1\"], \"oauth\"",
        1,
    );
    let round_trip = parse(&with_caps).expect("capabilities round trip");
    assert_eq!(
        round_trip.host_capabilities,
        vec!["validated-protocol-pin-v1".to_string()]
    );

    let snake = base_record().replace(
        "\"oauth\"",
        "\"desired_routes\": [], \"applied_routes\": [], \"oauth\"",
    );
    let aliased = parse(&snake).expect("snake case routes");
    assert!(aliased.desired_routes.is_empty());
}

#[test]
fn a_malformed_route_or_null_vector_poisons_the_record() {
    let null_routes = base_record().replace("\"oauth\"", "\"desiredRoutes\": null, \"oauth\"");
    assert!(parse(&null_routes).is_err());
    let broken = base_record().replace(
        "\"oauth\"",
        "\"desiredRoutes\": [{\"authId\": \"only\"}], \"oauth\"",
    );
    assert!(parse(&broken).is_err());
    let leading_zero = base_record().replace(
        "\"oauth\"",
        "\"desiredRoutes\": [{
            \"authId\": \"codex.json\",
            \"credentialId\": \"cid\",
            \"credentialVersion\": \"01\",
            \"bindingId\": \"binding\",
            \"materialFingerprint\": \"material\",
            \"routingRank\": 1,
            \"routes\": [],
            \"fingerprint\": \"fp\"
        }], \"oauth\"",
    );
    assert!(parse(&leading_zero).is_err());
}

#[test]
fn decimal_route_versions_and_unknown_presence_fail_closed() {
    let route = base_record().replace(
        "\"oauth\"",
        "\"desiredRoutes\": [{
            \"authId\": \"codex.json\",
            \"credentialId\": \"cid\",
            \"credentialVersion\": \"2\",
            \"bindingId\": \"binding\",
            \"materialFingerprint\": \"material\",
            \"routingRank\": 1,
            \"routes\": [{
                \"publicModel\": \"gpt-5\",
                \"upstreamModel\": \"gpt-5\",
                \"protocol\": \"responses\",
                \"endpointId\": \"endpoint\",
                \"origin\": \"https://chatgpt.com\",
                \"endpointFingerprint\": \"abcd\",
                \"validationOnly\": false
            }],
            \"fingerprint\": \"fp\"
        }], \"oauth\"",
    );
    let record = parse(&route).expect("decimal version");
    assert_eq!(record.desired_routes[0].credential_version, 2);
    assert_eq!(record.desired_routes[0].routes[0].protocol, "responses");
    assert!(record.desired_routes[0].routes[0].native_targets.is_empty());
    let unknown = base_record().replace(
        "\"models\": [\"gpt-5\"]",
        "\"models\": [\"gpt-5\"], \"presence\": \"revived\"",
    );
    assert!(parse(&unknown).is_err());
    let product = base_record().replace("\"providerId\": \"codex\"", "\"providerId\": \"cpa\"");
    let kept = parse(&product).expect("product provider");
    assert_eq!(kept.oauth[0].provider_id, "cpa");
    assert!(kept.oauth[0].native_provider.is_empty());
}

#[test]
fn missing_native_targets_decode_empty_and_bad_targets_poison_the_record() {
    let route = base_record().replace(
        "\"oauth\"",
        "\"desiredRoutes\": [{
            \"authId\": \"codex.json\",
            \"credentialId\": \"cid\",
            \"credentialVersion\": \"2\",
            \"bindingId\": \"binding\",
            \"materialFingerprint\": \"material\",
            \"routingRank\": 1,
            \"routes\": [{
                \"publicModel\": \"gpt-5\",
                \"upstreamModel\": \"gpt-5\",
                \"protocol\": \"chat_completions\",
                \"endpointId\": \"80bcfcea-1f42-5a5e-81f8-3abd801b2aea\",
                \"origin\": \"https://chatgpt.com\",
                \"endpointFingerprint\": \"1897faf097db8edfa5c0c6765abb12be180ed7aff633203298e6c0c28fcb16e5\",
                \"validationOnly\": false,
                \"nativeTargets\": [{
                    \"pin\": {
                        \"protocol\": \"chat_completions\",
                        \"endpointId\": \"80bcfcea-1f42-5a5e-81f8-3abd801b2aea\",
                        \"origin\": \"https://chatgpt.com\",
                        \"endpointFingerprint\": \"1897faf097db8edfa5c0c6765abb12be180ed7aff633203298e6c0c28fcb16e5\",
                        \"httpMethod\": \"POST\"
                    },
                    \"generationKinds\": [\"execute\", \"refresh-resend\"]
                }]
            }],
            \"fingerprint\": \"fp\"
        }], \"oauth\"",
    );
    let record = parse(&route).expect("stored pin");
    assert_eq!(record.desired_routes[0].routes[0].native_targets.len(), 1);
    assert_eq!(
        record.desired_routes[0].routes[0].native_targets[0]
            .pin
            .http_method,
        "POST"
    );
    let null_targets = route.replace(
        "\"nativeTargets\": [{",
        "\"nativeTargets\": null, \"ignored\": [{",
    );
    assert!(parse(&null_targets).is_err());
    let default_port = route.replace("https://chatgpt.com", "https://chatgpt.com:443");
    assert!(parse(&default_port).is_err());
    let bad_mode = base_record().replace(
        "\"models\": [\"gpt-5\"]",
        "\"models\": [\"gpt-5\"], \"nativeMode\": \"custom\"",
    );
    assert!(parse(&bad_mode).is_err());
}

fn hex64(seed: &str) -> String {
    seed.chars().next().unwrap().to_string().repeat(64)
}

fn accepted_snapshot(auth: &str, routes: &str) -> String {
    format!(
        r#"{{
            "generation": "4",
            "revision": "2",
            "wireDigest": "{digest}",
            "auth": [{auth}],
            "routes": [{routes}],
            "artifactSha256": "{artifact}",
            "listenPort": 8085,
            "ownedOrigin": "http://127.0.0.1:8085",
            "publicOrigin": "http://127.0.0.1:9"
        }}"#,
        digest = hex64("a"),
        artifact = hex64("b"),
        auth = auth,
        routes = routes,
    )
}

fn with_snapshot(snapshot: &str) -> String {
    base_record().replace(
        "\"oauth\"",
        &format!("\"previousAccepted\": {snapshot}, \"oauth\""),
    )
}

#[test]
fn accepted_snapshot_distinguishes_unknown_from_an_empty_plane() {
    let missing = parse(&base_record()).expect("historical record");
    assert!(missing.previous_accepted.is_none());
    let explicit = with_snapshot("null");
    let absent = parse(&explicit).expect("null snapshot");
    assert!(absent.previous_accepted.is_none());

    let empty = with_snapshot(&accepted_snapshot("", ""));
    let record = parse(&empty).expect("empty accepted plane");
    let snapshot = record.previous_accepted.clone().expect("empty snapshot");
    assert!(snapshot.auth.is_empty());
    assert!(snapshot.routes.is_empty());
    assert_eq!(snapshot.generation, 4);
    assert_eq!(snapshot.revision, 2);
    assert_eq!(snapshot.listen_port, 8085);
    let encoded = serde_json::to_string(&super::raw_from(&record)).expect("encode");
    let again = parse(&encoded).expect("round trip");
    assert_eq!(again.previous_accepted, record.previous_accepted);
    assert!(again.previous_accepted.is_some());
}

#[test]
fn accepted_snapshot_poisons_duplicate_invalid_and_truncated_planes() {
    let duplicate = accepted_snapshot(
        r#"{
            "authId": "same",
            "credentialId": "cred-a",
            "credentialVersion": "1",
            "bindingId": "bind-a",
            "materialRevision": "material",
            "providerId": "opencode",
            "registrationEpoch": "0"
        }, {
            "authId": "same",
            "credentialId": "cred-b",
            "credentialVersion": "1",
            "bindingId": "bind-b",
            "materialRevision": "material",
            "providerId": "opencode",
            "registrationEpoch": "0"
        }"#,
        "",
    );
    assert!(parse(&with_snapshot(&duplicate)).is_err());

    let get_method = accepted_snapshot(
        "",
        r#"{
            "authId": "auth-a",
            "credentialId": "cred-a",
            "credentialVersion": "1",
            "bindingId": "bind-a",
            "materialFingerprint": "material",
            "routingRank": 1,
            "fingerprint": "route-keep",
            "routes": [{
                "publicModel": "public",
                "upstreamModel": "upstream",
                "protocol": "chat_completions",
                "endpointId": "endpoint-a",
                "origin": "https://lab.example",
                "endpointFingerprint": "abababababababababababababababababababababababababababababababab",
                "validationOnly": false,
                "nativeTargets": [{
                    "pin": {
                        "protocol": "chat_completions",
                        "endpointId": "endpoint-a",
                        "origin": "https://lab.example",
                        "endpointFingerprint": "abababababababababababababababababababababababababababababababab",
                        "httpMethod": "GET"
                    },
                    "generationKinds": ["execute"]
                }]
            }]
        }"#,
    );
    assert!(parse(&with_snapshot(&get_method)).is_err());
    assert!(parse(&with_snapshot(r#"{"generation":"4"}"#)).is_err());
}

#[test]
fn accepted_snapshot_rejects_a_shared_keyed_identity_contradiction() {
    let matching = accepted_snapshot(
        r#"{
            "authId": "shared",
            "credentialId": "cred-a",
            "credentialVersion": "1",
            "bindingId": "bind-a",
            "materialRevision": "material-a",
            "providerId": "opencode",
            "registrationEpoch": "0"
        }"#,
        r#"{
            "authId": "shared",
            "credentialId": "cred-a",
            "credentialVersion": "1",
            "bindingId": "bind-a",
            "materialFingerprint": "material-a",
            "routingRank": 1,
            "fingerprint": "route-keep",
            "routes": []
        }"#,
    );
    parse(&with_snapshot(&matching)).expect("matching keyed identity");

    let contradiction = accepted_snapshot(
        r#"{
            "authId": "shared",
            "credentialId": "cred-a",
            "credentialVersion": "1",
            "bindingId": "bind-a",
            "materialRevision": "material-a",
            "providerId": "opencode",
            "registrationEpoch": "0"
        }"#,
        r#"{
            "authId": "shared",
            "credentialId": "cred-b",
            "credentialVersion": "2",
            "bindingId": "bind-b",
            "materialFingerprint": "material-b",
            "routingRank": 1,
            "fingerprint": "route-keep",
            "routes": []
        }"#,
    );
    assert!(parse(&with_snapshot(&contradiction)).is_err());
}

#[test]
fn accepted_snapshot_keeps_native_only_routes_and_unknown_none() {
    assert!(parse(&base_record()).unwrap().previous_accepted.is_none());
    let native_only = accepted_snapshot(
        "",
        r#"{
            "authId": "codex.json",
            "credentialId": "native-cred",
            "credentialVersion": "2",
            "bindingId": "native-bind",
            "materialFingerprint": "material",
            "routingRank": 1,
            "fingerprint": "native-keep",
            "routes": [{
                "publicModel": "gpt-5",
                "upstreamModel": "gpt-5",
                "protocol": "chat_completions",
                "endpointId": "80bcfcea-1f42-5a5e-81f8-3abd801b2aea",
                "origin": "https://chatgpt.com",
                "endpointFingerprint": "1897faf097db8edfa5c0c6765abb12be180ed7aff633203298e6c0c28fcb16e5",
                "validationOnly": false,
                "nativeTargets": [{
                    "pin": {
                        "protocol": "chat_completions",
                        "endpointId": "80bcfcea-1f42-5a5e-81f8-3abd801b2aea",
                        "origin": "https://chatgpt.com",
                        "endpointFingerprint": "1897faf097db8edfa5c0c6765abb12be180ed7aff633203298e6c0c28fcb16e5",
                        "httpMethod": "POST"
                    },
                    "generationKinds": ["execute"]
                }]
            }]
        }"#,
    );
    let record = parse(&with_snapshot(&native_only)).expect("native-only routes");
    let snapshot = record.previous_accepted.expect("native snapshot");
    assert!(snapshot.auth.is_empty());
    assert_eq!(snapshot.routes.len(), 1);
    assert_eq!(snapshot.routes[0].auth_id, "codex.json");
}

#[test]
fn parse_rejects_an_oversized_execution_record() {
    let mut huge = base_record();
    huge.push_str(&" ".repeat(super::MAX_CONFIG_BYTES + 1));
    assert!(huge.len() > super::MAX_CONFIG_BYTES);
    assert!(parse(&huge).is_err());
    let oversized = rebase_archived_execution_record(&huge, "a", "b", None, None);
    assert!(oversized.is_err());
}

fn proof_yaml(auth_dir: &str, generation: u64, revision: u64) -> String {
    format!(
        "\
host: \"127.0.0.1\"
port: 8085
auth-dir: \"{auth_dir}\"
api-keys:
  - \"hop\"
ocg:
  process-generation: \"{generation}\"
  projection-revision: \"{revision}\"
  ready-key: \"ready\"
  policy:
    url: \"http://127.0.0.1:9/_internal/ocg/cpa-policy\"
    token: \"policy\"
    origin: \"http://127.0.0.1:9\"
"
    )
}

fn execution_json(applied_digest: &str, desired_digest: &str, previous: Option<&str>) -> String {
    let previous = previous
        .map(|value| format!("\"previousAccepted\": {value}"))
        .unwrap_or_else(|| "\"observer\": \"keep\"".to_string());
    format!(
        r#"{{
            "version": 1,
            "childGeneration": "11",
            "appliedGeneration": "11",
            "desiredRevision": "9",
            "appliedRevision": "3",
            "desiredDigest": "{desired_digest}",
            "appliedDigest": "{applied_digest}",
            "artifactSha256": "{artifact}",
            "listenPort": 8085,
            "applyStatus": "applied",
            "desiredRunning": true,
            "publicOrigin": "http://127.0.0.1:9",
            "ownedOrigin": "http://127.0.0.1:8085",
            "policyReady": true,
            "unavailable": false,
            "desiredAuth": [],
            "appliedAuth": [],
            "desiredRoutes": [],
            "appliedRoutes": [],
            "oauth": [],
            "observer": "keep",
            {previous}
        }}"#,
        artifact = hex64("b"),
    )
}

#[test]
fn accepted_snapshot_rebase_updates_only_the_matching_tuple() {
    let current = proof_yaml("/source/cpa/auth", 11, 3);
    let moved = proof_yaml("/restored/cpa/auth", 11, 3);
    let current_digest = crate::cpa_projection::wire_digest(&current);
    let moved_digest = crate::cpa_projection::wire_digest(&moved);
    let desired = hex64("c");
    let previous_yaml = proof_yaml("/source/cpa/auth", 4, 2);
    let previous_moved = proof_yaml("/restored/cpa/auth", 4, 2);
    let previous = accepted_snapshot("", "");
    let previous = previous.replace(
        &hex64("a"),
        &crate::cpa_projection::wire_digest(&previous_yaml),
    );
    let json = execution_json(&current_digest, &desired, Some(&previous));
    let updated = rebase_archived_execution_record(
        &json,
        &current,
        &moved,
        Some(&previous_yaml),
        Some(&previous_moved),
    )
    .expect("rebase")
    .expect("digest changed");
    let value: serde_json::Value = serde_json::from_str(&updated).unwrap();
    assert_eq!(value["appliedDigest"], moved_digest);
    assert_eq!(value["desiredDigest"], desired);
    assert_eq!(value["observer"], "keep");
    assert_eq!(
        value["previousAccepted"]["wireDigest"],
        crate::cpa_projection::wire_digest(&previous_moved)
    );
    assert!(value.get("previousAccepted").is_some());
    parse(&updated).expect("rebased record");

    let unknown = execution_json(&current_digest, &desired, None);
    let kept = rebase_archived_execution_record(
        &unknown,
        &current,
        &moved,
        Some("projection:\n  generation: 7\n"),
        Some("projection:\n  generation: 7\n"),
    )
    .expect("current match");
    let kept = kept.expect("applied digest");
    let value: serde_json::Value = serde_json::from_str(&kept).unwrap();
    assert!(value.get("previousAccepted").is_none());
    assert_eq!(value["appliedDigest"], moved_digest);
    assert_eq!(value["desiredDigest"], desired);

    let desired_only = execution_json(&hex64("d"), &current_digest, None);
    let untouched = rebase_archived_execution_record(&desired_only, &current, &moved, None, None)
        .expect("desired is not accepted");
    assert!(untouched.is_none());

    let err = rebase_archived_execution_record("{", &current, &moved, None, None).unwrap_err();
    assert!(err.to_string().contains("could not be read"));
    let skipped = rebase_archived_execution_record(
        &json,
        "projection:\n  generation: 7\n",
        "projection:\n  generation: 8\n",
        None,
        None,
    )
    .expect("non-canonical yaml");
    assert!(skipped.is_none());
}
