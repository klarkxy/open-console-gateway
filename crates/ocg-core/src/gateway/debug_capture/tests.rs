use super::*;
use axum::http::HeaderValue;

#[test]
fn complete_content_preserves_large_messages_tools_images_and_removes_credentials() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "authorization",
        HeaderValue::from_static("Bearer unusual-credential"),
    );
    headers.insert("cookie", HeaderValue::from_static("session=private"));
    headers.insert("x-custom-token", HeaderValue::from_static("other-secret"));
    headers.insert("x-repeat", HeaderValue::from_static("first"));
    headers.append("x-repeat", HeaderValue::from_static("second"));
    let text = "中".repeat(3000);
    let body = json!({"messages":[{"content":text}], "image":"data:image/png;base64,AAAA", "tools":[{"parameters":{"properties":{"token":{"type":"string"}}}}], "metadata":{"api_key":"hidden", "echo":"unusual-credential"}});
    let record = capture_record(
        "ocg-test",
        "upstream",
        2,
        "https://user:pass@example.com/v1/messages?key=hidden&test=ok",
        &headers,
        &serde_json::to_vec(&body).unwrap(),
        &["upstream-secret".into()],
    );
    assert_eq!(record["body"]["messages"], body["messages"]);
    assert_eq!(record["body"]["image"], body["image"]);
    assert_eq!(record["body"]["tools"], body["tools"]);
    assert_eq!(record["attempt"], 2);
    assert_eq!(record["capture_status"], "complete_redacted");
    let encoded = record.to_string();
    for secret in [
        "unusual-credential",
        "other-secret",
        "session=private",
        "hidden",
        "user:pass",
    ] {
        assert!(!encoded.contains(secret), "{secret}");
    }
    assert_eq!(
        record["headers"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| v["name"] == "x-repeat")
            .count(),
        2
    );
}

#[test]
fn malformed_input_is_explicitly_omitted() {
    let record = capture_record(
        "ocg-test",
        "client",
        0,
        "/v1/messages",
        &HeaderMap::new(),
        b"{secret",
        &[],
    );
    assert_eq!(record["capture_status"], "invalid_json_omitted");
    assert_eq!(record["body"]["bytes"], 7);
    assert!(!record.to_string().contains("{secret"));
}

#[tokio::test]
async fn capture_is_atomic_disabled_is_noop_and_write_failure_is_reported() {
    let root = std::env::temp_dir().join(format!("ocg-capture-test-{}", uuid::Uuid::new_v4()));
    let capture = DebugCapture {
        directory: Some(root.clone()),
        writer: Arc::new(parking_lot::Mutex::new(())),
    };
    let trace = RequestTrace::new();
    capture
        .save(
            &trace,
            "client",
            0,
            "/v1/messages",
            &HeaderMap::new(),
            Bytes::from_static(b"{}"),
            &[],
        )
        .await
        .unwrap();
    let files: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .map(Result::unwrap)
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| owned_filename(name) && name.ends_with(".json"))
        })
        .collect();
    assert_eq!(files.len(), 1);
    assert!(owned_filename(&files[0].file_name().to_string_lossy()));
    let record: Value = serde_json::from_slice(&std::fs::read(files[0].path()).unwrap()).unwrap();
    assert_eq!(record["request_id"], trace.request_id);
    assert!(
        !files[0]
            .file_name()
            .to_string_lossy()
            .starts_with(&trace.request_id)
    );
    let kept = std::fs::read(files[0].path()).unwrap();
    let blocked = DebugCapture {
        directory: Some(files[0].path()),
        writer: capture.writer.clone(),
    };
    assert!(
        blocked
            .save(
                &trace,
                "client",
                0,
                "/",
                &HeaderMap::new(),
                Bytes::new(),
                &[]
            )
            .await
            .is_err()
    );
    assert_eq!(std::fs::read(files[0].path()).unwrap(), kept);
    let disabled = DebugCapture {
        directory: None,
        writer: capture.writer.clone(),
    };
    disabled
        .save(
            &trace,
            "client",
            0,
            "/",
            &HeaderMap::new(),
            Bytes::new(),
            &[],
        )
        .await
        .unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn retention_recognizes_only_owned_capture_names() {
    assert!(owned_filename(&format!(
        "ocg-{}-100-client-0.json",
        uuid::Uuid::new_v4()
    )));
    assert!(!owned_filename("ocg-user-data-client-0.json"));
    assert!(!owned_filename("notes.json"));
}

#[tokio::test]
async fn http_capture_covers_chat_and_gemini_without_capturing_unauthorized_bodies() {
    use crate::crypto::{KeyCipher, StaticKeyCipher};
    let root = std::env::temp_dir().join(format!("ocg-capture-http-{}", uuid::Uuid::new_v4()));
    let db = crate::db::Database::open(root.clone()).unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("test"));
    let mut inner = crate::state::CoreStateInner::new(db, root.clone(), cipher).unwrap();
    let captures = root.join("captures");
    inner.debug_capture = DebugCapture {
        directory: Some(captures.clone()),
        writer: Arc::new(parking_lot::Mutex::new(())),
    };
    let state = Arc::new(inner);
    let mut config = state.config();
    config.gateway_key = "capture-test-key".into();
    state.set_config(config).unwrap();
    let router = super::super::inference_router_with_body_limit(state.clone(), 1024)
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
            .unwrap();
    });
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let response = client
        .post(format!("http://{addr}/v1/chat/completions"))
        .json(&json!({"messages": "private"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    assert!(!captures.exists());
    for path in [
        "/v1/chat/completions",
        "/v1beta/models/missing:generateContent",
    ] {
        let response = client
            .post(format!("http://{addr}{path}"))
            .bearer_auth("capture-test-key")
            .json(&json!({"private_text": "保留正文"}))
            .send()
            .await
            .unwrap();
        let id = response
            .headers()
            .get("x-ocg-request-id")
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(response.status(), 400);
        let record = record_for_request(&captures, id);
        let name = capture_names(&captures)
            .into_iter()
            .find(|name| {
                let text = std::fs::read_to_string(captures.join(name)).unwrap_or_default();
                text.contains(id)
            })
            .unwrap();
        assert!(owned_filename(&name));
        assert!(!name.starts_with(id));
        assert_eq!(record["body"]["private_text"], "保留正文");
        assert_eq!(record["uri"], path);
        assert!(!record.to_string().contains("capture-test-key"));
        assert!(
            state
                .db
                .lock()
                .query_gateway_logs(50, Some(id))
                .unwrap()
                .is_empty()
        );
    }
    let response = client
        .post(format!("http://{addr}/v1/messages"))
        .bearer_auth("capture-test-key")
        .body(vec![b' '; 1025])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 413);
    assert_eq!(capture_names(&captures).len(), 2);
    stop.send(()).unwrap();
    server.await.unwrap();
    drop(state);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn credential_containers_and_custom_headers_are_redacted() {
    let mut headers = HeaderMap::new();
    headers.insert("x-private-key", HeaderValue::from_static("header-secret"));
    let body = json!({"private_key": "private", "password": ["array-secret"], "authorization": {"value":"object-secret"}, "metadata":{"secretKey":"nested-secret"}, "tools":[{"input_schema":{"properties":{"token":{"type":"string"}}}}]});
    let record = capture_record(
        "ocg-test",
        "client",
        0,
        "/v1/messages",
        &headers,
        &serde_json::to_vec(&body).unwrap(),
        &[],
    );
    for secret in [
        "header-secret",
        "array-secret",
        "object-secret",
        "nested-secret",
    ] {
        assert!(!record.to_string().contains(secret));
    }
    assert_eq!(record["body"]["private_key"], "<redacted>");
    assert_eq!(record["body"]["tools"], body["tools"]);
}

#[test]
fn padded_auth_values_are_redacted_from_body_echoes() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "authorization",
        HeaderValue::from_static("Bearer   opaque-credential  "),
    );
    headers.insert(
        "x-api-key",
        HeaderValue::from_static(" another-credential "),
    );
    let record = capture_record(
        "ocg-test",
        "client",
        0,
        "/",
        &headers,
        br#"{"message":"opaque-credential another-credential"}"#,
        &[],
    );
    assert!(!record.to_string().contains("opaque-credential"));
    assert!(!record.to_string().contains("another-credential"));
}

#[test]
fn retention_bounds_completed_files_and_preserves_lookalikes() {
    let root = std::env::temp_dir().join(format!("ocg-retention-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let id = uuid::Uuid::new_v4();
    let lookalike = format!("ocg-{id}-notes-client-backup.json");
    assert!(!owned_filename(&lookalike));
    std::fs::write(root.join(&lookalike), "keep").unwrap();
    for i in 0..MAX_FILES {
        std::fs::write(root.join(format!("ocg-{id}-{i}-client-0.json")), "{}").unwrap();
    }
    write_capture(&root, &format!("ocg-{id}-1001-upstream-1.json"), b"{}").unwrap();
    assert_eq!(regular_owned_count(&root), MAX_FILES);
    assert_eq!(
        std::fs::read_to_string(root.join(lookalike)).unwrap(),
        "keep"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn upstream_capture_removes_both_client_and_upstream_credentials() {
    let mut client = HeaderMap::new();
    client.insert(
        "authorization",
        HeaderValue::from_static("Bearer gateway-secret"),
    );
    let mut upstream = HeaderMap::new();
    upstream.insert(
        "authorization",
        HeaderValue::from_static("Bearer provider-secret"),
    );
    let mut secrets = authentication_secrets(&client);
    secrets.push("provider-secret".into());
    let record = capture_record(
        "ocg-test",
        "upstream",
        1,
        "https://example.com/v1/messages",
        &upstream,
        br#"{"messages":[{"content":"gateway-secret provider-secret"}]}"#,
        &secrets,
    );
    assert!(!record.to_string().contains("gateway-secret"));
    assert!(!record.to_string().contains("provider-secret"));
}

#[test]
fn schema_named_metadata_cannot_bypass_redaction() {
    let body = json!({"metadata":{"properties":{"api_key":"hidden1"},"definitions":{"password":"hidden2"},"$defs":{"token":"hidden3"}}});
    let record = capture_record(
        "ocg-test",
        "client",
        0,
        "/",
        &HeaderMap::new(),
        &serde_json::to_vec(&body).unwrap(),
        &[],
    );
    for secret in ["hidden1", "hidden2", "hidden3"] {
        assert!(!record.to_string().contains(secret));
    }
}

#[test]
fn debug_requests_stay_off_unless_the_value_is_one() {
    assert!(requests_enabled(Some("1")));
    assert!(requests_enabled(Some(" 1 ")));
    for value in [
        None,
        Some(""),
        Some("0"),
        Some("true"),
        Some("yes"),
        Some("2"),
    ] {
        assert!(!requests_enabled(value), "{value:?}");
    }
    assert!(
        !DebugCapture {
            directory: None,
            writer: Arc::new(parking_lot::Mutex::new(())),
        }
        .enabled()
    );
}

#[test]
fn huge_body_is_omitted_without_a_raw_prefix_or_partial_json() {
    let token = "RAWPREFIX-SECRET-do-not-store";
    let mut body = vec![b'x'; MAX_ENCODED_BYTES + token.len()];
    body[..token.len()].copy_from_slice(token.as_bytes());
    let record = capture_record(
        "ocg-test",
        "client",
        0,
        "/v1/messages",
        &HeaderMap::new(),
        &body,
        &[],
    );
    assert_eq!(record["capture_status"], "body_omitted_too_large");
    assert_eq!(record["body"]["bytes"], body.len());
    assert_eq!(
        record["body"]["sha256"],
        crate::redaction::sha256_hex(&body)
    );
    let encoded = serde_json::to_vec_pretty(&record).unwrap();
    assert!(encoded.len() <= MAX_ENCODED_BYTES);
    assert!(
        !encoded
            .windows(token.len())
            .any(|window| window == token.as_bytes())
    );
    serde_json::from_slice::<Value>(&encoded).unwrap();

    let mut wide = Vec::new();
    wide.push(b'[');
    wide.extend_from_slice(format!("\"{token}\"").as_bytes());
    for _ in 0..700_000 {
        wide.push(b',');
        wide.push(b'0');
    }
    wide.push(b']');
    assert!(wide.len() <= MAX_ENCODED_BYTES);
    let encoded = build_capture_bytes(
        "ocg-test",
        "client",
        0,
        "/v1/messages",
        &HeaderMap::new(),
        &wide,
        &[],
    )
    .unwrap();
    assert!(encoded.len() <= MAX_ENCODED_BYTES);
    assert!(
        !encoded
            .windows(token.len())
            .any(|window| window == token.as_bytes())
    );
    let record: Value = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(record["capture_status"], "body_omitted_too_large");
    assert!(record["body"].is_object());
    assert_eq!(record["body"]["bytes"], wide.len());
}

#[tokio::test]
async fn unsafe_stage_and_request_id_never_become_filenames() {
    let root = temp_dir("names");
    let capture = DebugCapture {
        directory: Some(root.clone()),
        writer: Arc::new(parking_lot::Mutex::new(())),
    };
    let mut trace = RequestTrace::new();
    trace.request_id = "../user-supplied-id".into();
    assert_eq!(
        capture
            .save(
                &trace,
                "../upstream",
                0,
                "/v1/messages",
                &HeaderMap::new(),
                Bytes::from_static(b"{}"),
                &[],
            )
            .await,
        Err("unsafe_stage".into())
    );
    assert!(std::fs::read_dir(&root).unwrap().next().is_none());
    capture
        .save(
            &trace,
            "client",
            0,
            "/v1/messages",
            &HeaderMap::new(),
            Bytes::from_static(b"{}"),
            &[],
        )
        .await
        .unwrap();
    let names = capture_names(&root);
    assert_eq!(names.len(), 1);
    assert!(owned_filename(&names[0]));
    assert!(!names[0].contains("user-supplied"));
    assert!(!names[0].contains(".."));
    let record: Value =
        serde_json::from_slice(&std::fs::read(root.join(&names[0])).unwrap()).unwrap();
    assert_eq!(record["request_id"], "../user-supplied-id");
    remove_tree(&root);
}

#[test]
fn invalid_filenames_are_rejected_without_creating_files() {
    let root = temp_dir("invalid-names");
    std::fs::create_dir_all(&root).unwrap();
    let outside = root.join("outside.txt");
    std::fs::write(&outside, b"keep").unwrap();
    let id = uuid::Uuid::new_v4();
    for name in [
        "../outside.txt",
        "notes.json",
        &format!("ocg-{id}-notes-client-backup.json"),
        &format!("ocg-{id}-1-client-0.json/extra"),
        &format!("ocg-{id}-1-other-0.json"),
        "ocg-not-a-uuid-1-client-0.json",
        "",
    ] {
        assert!(!owned_filename(name), "{name}");
        assert!(write_capture(&root, name, b"{}").is_err(), "{name}");
    }
    assert_eq!(std::fs::read(&outside).unwrap(), b"keep");
    assert_eq!(capture_names(&root).len(), 0);
    remove_tree(&root);
}

#[test]
fn bounds_prune_only_owned_files_for_count_age_and_total_bytes() {
    let root = temp_dir("bounds");
    std::fs::create_dir_all(&root).unwrap();
    let id = uuid::Uuid::new_v4();
    let foreign = root.join("notes.json");
    std::fs::write(&foreign, b"keep-foreign").unwrap();
    let link_target = root.join("link-target.txt");
    std::fs::write(&link_target, b"keep-link").unwrap();
    let linked = root.join(format!("ocg-{id}-4-client-1.json"));
    let have_link = try_file_reparse(&linked, &link_target);
    let lookalike = root.join(format!("ocg-{id}-notes-client-backup.json"));
    std::fs::write(&lookalike, b"keep-lookalike").unwrap();
    let nested = root.join(format!("ocg-{id}-9-client-0.json"));
    std::fs::create_dir(&nested).unwrap();
    std::fs::write(nested.join("inside"), b"keep-nested").unwrap();
    let old = root.join(format!("ocg-{id}-1-client-0.json"));
    std::fs::write(&old, b"{}").unwrap();
    set_age(&old, 8);
    for index in 0..MAX_FILES {
        std::fs::write(
            root.join(format!("ocg-{id}-{index}-upstream-0.json")),
            b"{}",
        )
        .unwrap();
    }
    let fresh = format!("ocg-{id}-{}-client-1.json", MAX_FILES + 10);
    write_capture(&root, &fresh, b"{}").unwrap();
    assert!(!old.exists());
    assert_eq!(std::fs::read(&foreign).unwrap(), b"keep-foreign");
    if have_link {
        assert_eq!(std::fs::read(&link_target).unwrap(), b"keep-link");
        assert!(crate::fs_privacy::metadata_is_reparse(
            &std::fs::symlink_metadata(&linked).unwrap()
        ));
    }
    assert_eq!(std::fs::read(&lookalike).unwrap(), b"keep-lookalike");
    assert_eq!(
        std::fs::read(nested.join("inside")).unwrap(),
        b"keep-nested"
    );
    assert_eq!(regular_owned_count(&root), MAX_FILES);

    let bytes_root = temp_dir("bytes");
    std::fs::create_dir_all(&bytes_root).unwrap();
    let foreign_bytes = bytes_root.join("foreign.bin");
    std::fs::write(&foreign_bytes, b"keep-bytes").unwrap();
    let older = bytes_root.join(format!("ocg-{id}-0-client-0.json"));
    let newer = bytes_root.join(format!("ocg-{id}-59-client-0.json"));
    for index in 0..60 {
        sparse_file(
            &bytes_root.join(format!("ocg-{id}-{index}-client-0.json")),
            MAX_ENCODED_BYTES as u64,
        );
    }
    set_age(&older, 1);
    let added = format!("ocg-{id}-60-client-0.json");
    write_capture(&bytes_root, &added, b"{}").unwrap();
    assert!(!older.exists());
    assert!(newer.exists());
    assert!(bytes_root.join(&added).exists());
    let total: u64 = list_owned(&bytes_root)
        .unwrap()
        .iter()
        .map(|file| file.len)
        .sum();
    assert!(total <= MAX_TOTAL_BYTES);
    assert_eq!(std::fs::read(&foreign_bytes).unwrap(), b"keep-bytes");
    if have_link {
        let _ = std::fs::remove_file(&linked);
    }
    remove_tree(&root);
    remove_tree(&bytes_root);
}

#[test]
fn linked_directories_and_foreign_partials_are_left_alone() {
    let root = temp_dir("links");
    let real = root.join("real");
    std::fs::create_dir_all(&real).unwrap();
    std::fs::write(real.join("marker"), b"keep-target").unwrap();
    let link = root.join("link");
    assert!(
        make_directory_reparse(&link, &real),
        "could not create a directory symlink or junction"
    );
    let name = format!("ocg-{}-1-client-0.json", uuid::Uuid::new_v4());
    assert!(write_capture(&link, &name, b"{}").is_err());
    assert_eq!(std::fs::read(real.join("marker")).unwrap(), b"keep-target");
    assert!(!real.join(&name).exists());
    assert!(startup_maintain(&link.join("debug-requests")).is_err());
    assert!(!real.join("debug-requests").exists());

    let plain = root.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let partial = plain.join("foreign.partial");
    std::fs::write(&partial, b"foreign-partial").unwrap();
    let interrupted = plain.join(&name).with_extension("partial");
    std::fs::write(&interrupted, b"interrupted-owned-write").unwrap();
    write_capture(&plain, &name, b"{}").unwrap();
    assert_eq!(std::fs::read(&partial).unwrap(), b"foreign-partial");
    assert!(!interrupted.exists());
    assert!(plain.join(&name).exists());
    remove_reparse(&link);
    remove_tree(&root);
}

#[test]
fn new_and_existing_capture_destinations_are_private() {
    let root = temp_dir("private");
    std::fs::create_dir_all(&root).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let name = format!("ocg-{}-1-client-0.json", uuid::Uuid::new_v4());
    write_capture(&root, &name, b"{}").unwrap();
    crate::fs_privacy::permissions_are_private(&root).unwrap();
    crate::fs_privacy::permissions_are_private(&root.join(&name)).unwrap();

    let created = root.join("created");
    let created_name = format!("ocg-{}-2-upstream-0.json", uuid::Uuid::new_v4());
    write_capture(&created, &created_name, b"{}").unwrap();
    crate::fs_privacy::permissions_are_private(&created).unwrap();
    crate::fs_privacy::permissions_are_private(&created.join(&created_name)).unwrap();
    remove_tree(&root);
}

#[test]
fn contended_writer_skips_without_pruning_and_legacy_owned_files_expire() {
    let root = temp_dir("capture-contention");
    let old = root.join(format!("{}-1-client-0.json", uuid::Uuid::new_v4()));
    std::fs::write(&old, b"{}").unwrap();
    set_age(&old, 8);
    let foreign = root.join("foreign.json");
    std::fs::write(&foreign, b"keep").unwrap();
    ensure_directory(&root).unwrap();
    let held = capture_lock(&root).unwrap();
    let name = format!("ocg-{}-2-client-0.json", uuid::Uuid::new_v4());
    assert_eq!(write_capture(&root, &name, b"{}"), Err("capture_busy"));
    assert!(old.exists());
    assert!(!root.join(&name).exists());
    assert_eq!(std::fs::read(&foreign).unwrap(), b"keep");
    drop(held);
    write_capture(&root, &name, b"{}").unwrap();
    assert!(!old.exists());
    assert_eq!(capture_names(&root).len(), 1);
    assert_eq!(std::fs::read(&foreign).unwrap(), b"keep");
    // Newly created children remain usable after their parent is private.
    std::fs::write(root.join("new-foreign.txt"), b"accessible").unwrap();
    remove_tree(&root);
}

#[tokio::test]
async fn capture_failure_does_not_drop_the_call_or_foreign_files() {
    use crate::crypto::StaticKeyCipher;
    let root = temp_dir("failure");
    let marker = root.join("marker");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(&marker, b"keep").unwrap();
    let db = crate::db::Database::open(root.join("db")).unwrap();
    let mut inner = crate::state::CoreStateInner::new(
        db,
        root.join("data"),
        Arc::new(StaticKeyCipher::new("test")),
    )
    .unwrap();
    inner.debug_capture = DebugCapture {
        directory: Some(marker.clone()),
        writer: Arc::new(parking_lot::Mutex::new(())),
    };
    let state = Arc::new(inner);
    let trace = RequestTrace::new();
    capture_client(
        &state,
        &trace,
        &HeaderMap::new(),
        Bytes::from_static(br#"{"messages":[{"content":"private-body"}]}"#),
    )
    .await;
    assert_eq!(std::fs::read(&marker).unwrap(), b"keep");
    drop(state);
    remove_tree(&root);
}

fn temp_dir(label: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "ocg-debug-capture-{label}-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn regular_owned_count(directory: &std::path::Path) -> usize {
    std::fs::read_dir(directory)
        .unwrap()
        .map(Result::unwrap)
        .filter(|entry| {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                return false;
            };
            owned_filename(name)
                && std::fs::symlink_metadata(entry.path()).is_ok_and(|meta| {
                    meta.is_file() && !crate::fs_privacy::metadata_is_reparse(&meta)
                })
        })
        .count()
}

fn capture_names(directory: &std::path::Path) -> Vec<String> {
    let mut names: Vec<_> = std::fs::read_dir(directory)
        .unwrap()
        .map(Result::unwrap)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| owned_filename(name) && name.ends_with(".json"))
        .collect();
    names.sort();
    names
}

fn record_for_request(directory: &std::path::Path, id: &str) -> Value {
    for name in capture_names(directory) {
        let bytes = std::fs::read(directory.join(&name)).unwrap();
        let Ok(record) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        if record["request_id"] == id {
            return record;
        }
    }
    panic!("missing capture for {id}");
}

fn set_age(path: &std::path::Path, days: u64) {
    let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    file.set_modified(
        std::time::SystemTime::now() - std::time::Duration::from_secs(days * 24 * 60 * 60),
    )
    .unwrap();
}

fn sparse_file(path: &std::path::Path, len: u64) {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .unwrap();
    file.set_len(len).unwrap();
}

fn try_file_reparse(link: &std::path::Path, target: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).is_ok()
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(target, link).is_ok()
    }
}

fn make_directory_reparse(link: &std::path::Path, target: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        return std::os::unix::fs::symlink(target, link).is_ok();
    }
    #[cfg(windows)]
    {
        if std::os::windows::fs::symlink_dir(target, link).is_ok() {
            return true;
        }
        use std::os::windows::process::CommandExt;
        let mut command = std::process::Command::new("cmd");
        command
            .raw_arg(format!(
                "/C mklink /J \"{}\" \"{}\"",
                link.display(),
                target.display()
            ))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        command.status().is_ok_and(|status| status.success())
    }
}

fn remove_reparse(path: &std::path::Path) {
    #[cfg(unix)]
    let _ = std::fs::remove_file(path);
    #[cfg(windows)]
    let _ = std::fs::remove_dir(path);
}

fn remove_tree(path: &std::path::Path) {
    let _ = std::fs::remove_dir_all(path);
}

fn capture_record(
    id: &str,
    stage: &str,
    attempt: u32,
    uri: &str,
    headers: &HeaderMap,
    body: &[u8],
    known_secrets: &[String],
) -> Value {
    assemble_record(
        id,
        (stage, attempt),
        uri,
        headers,
        body,
        known_secrets,
        false,
    )
}
