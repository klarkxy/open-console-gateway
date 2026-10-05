use super::{OwnedEnvelope, rebind_proven_envelope};

fn document(
    generation: u64,
    port: u16,
    auth_dir: &str,
    hop: &str,
    ready: &str,
    token: &str,
    origin: &str,
) -> String {
    format!(
        "\
host: \"127.0.0.1\"
port: {port}
auth-dir: \"{auth_dir}\"
api-keys:
  - \"{hop}\"
note: keep-bytes
openai-compatibility:
  - name: opencode
    api-key: provider-secret
ocg:
  process-generation: \"{generation}\"
  projection-revision: \"3\"
  ready-key: \"{ready}\"
  policy:
    url: \"{origin}/_internal/ocg/cpa-policy\"
    token: \"{token}\"
    origin: \"{origin}\"
  oauth-bindings:
    - name: binding-keep
"
    )
}

fn envelope(
    generation: u64,
    port: u16,
    auth_dir: &str,
    hop: &str,
    ready: &str,
    token: &str,
    origin: &str,
) -> OwnedEnvelope {
    OwnedEnvelope {
        process_generation: generation,
        listen_port: port,
        auth_dir: auth_dir.to_string(),
        policy_url: format!("{origin}/_internal/ocg/cpa-policy"),
        policy_token: token.to_string(),
        policy_origin: origin.to_string(),
        ready_key: ready.to_string(),
        hop_secret: hop.to_string(),
    }
}

#[test]
fn owned_envelope_rebind_keeps_exact_bytes_when_the_envelope_matches() {
    let yaml = document(
        11,
        18080,
        "/tmp/cpa/auth",
        "hop-secret",
        "ready-secret",
        "policy-secret",
        "http://127.0.0.1:9",
    );
    let digest = crate::cpa_projection::wire_digest(&yaml);
    let rebound = rebind_proven_envelope(
        &yaml,
        11,
        3,
        &digest,
        &envelope(
            11,
            18080,
            "/tmp/cpa/auth",
            "hop-secret",
            "ready-secret",
            "policy-secret",
            "http://127.0.0.1:9",
        ),
    )
    .expect("matching envelope");
    assert_eq!(rebound.yaml, yaml);
    assert_eq!(rebound.wire_digest, digest);
    assert_eq!(rebound.generation, 11);
    assert_eq!(rebound.revision, 3);
}

#[test]
fn owned_envelope_rebind_changes_only_the_owned_fields() {
    let yaml = document(
        11,
        18080,
        "/tmp/source/cpa/auth",
        "hop-secret",
        "ready-secret",
        "policy-secret",
        "http://127.0.0.1:9",
    );
    let digest = crate::cpa_projection::wire_digest(&yaml);
    let rebound = rebind_proven_envelope(
        &yaml,
        11,
        3,
        &digest,
        &envelope(
            22,
            18081,
            "/tmp/restored/cpa/auth",
            "hop-next",
            "ready-next",
            "policy-next",
            "http://127.0.0.1:10",
        ),
    )
    .expect("rebound envelope");
    assert_ne!(rebound.yaml, yaml);
    assert_eq!(rebound.generation, 22);
    assert_eq!(rebound.revision, 3);
    assert_eq!(
        rebound.wire_digest,
        crate::cpa_projection::wire_digest(&rebound.yaml)
    );
    assert!(rebound.yaml.contains("provider-secret"));
    assert!(rebound.yaml.contains("binding-keep"));
    assert!(rebound.yaml.contains("projection-revision"));
    assert!(rebound.yaml.contains("\"3\"") || rebound.yaml.contains("3"));
    assert!(rebound.yaml.contains("/tmp/restored/cpa/auth"));
    assert!(rebound.yaml.contains("hop-next"));
    assert!(!rebound.yaml.contains("hop-secret"));
    assert!(!rebound.yaml.contains("/tmp/source/cpa/auth"));
    let parsed: serde_yaml_ng::Value = serde_yaml_ng::from_str(&rebound.yaml).unwrap();
    let revision = parsed
        .get("ocg")
        .and_then(|ocg| ocg.get("projection-revision"))
        .and_then(serde_yaml_ng::Value::as_str);
    assert_eq!(revision, Some("3"));
}

#[test]
fn owned_envelope_rebind_rejects_a_mismatched_proof() {
    let yaml = document(
        11,
        18080,
        "/tmp/cpa/auth",
        "hop-secret",
        "ready-secret",
        "policy-secret",
        "http://127.0.0.1:9",
    );
    let error = rebind_proven_envelope(
        &yaml,
        11,
        3,
        &"ab".repeat(32),
        &envelope(
            22,
            18081,
            "/tmp/other/cpa/auth",
            "hop-next",
            "ready-next",
            "policy-next",
            "http://127.0.0.1:10",
        ),
    )
    .unwrap_err();
    assert!(error.to_string().contains("does not match"));
}

#[test]
fn rebound_yaml_debug_redacts_private_projection() {
    let yaml = document(
        11,
        18080,
        "/tmp/cpa/auth",
        "hop-secret",
        "ready-secret",
        "policy-secret",
        "http://127.0.0.1:9",
    );
    let digest = crate::cpa_projection::wire_digest(&yaml);
    let rebound = rebind_proven_envelope(
        &yaml,
        11,
        3,
        &digest,
        &envelope(
            11,
            18080,
            "/tmp/cpa/auth",
            "hop-secret",
            "ready-secret",
            "policy-secret",
            "http://127.0.0.1:9",
        ),
    )
    .expect("matching envelope");
    let formatted = format!("{rebound:?}");
    assert!(formatted.contains("[redacted]"));
    assert!(formatted.contains(&digest));
    assert!(!formatted.contains("provider-secret"));
    assert!(!formatted.contains("hop-secret"));
    assert!(!formatted.contains("ready-secret"));
    assert!(!formatted.contains("policy-secret"));
    assert!(!formatted.contains(&yaml));
}
