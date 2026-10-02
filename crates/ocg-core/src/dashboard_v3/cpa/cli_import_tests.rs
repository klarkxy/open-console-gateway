use super::*;
use crate::cpa_cli_import::ImportedCredential;
use crate::crypto::StaticKeyCipher;
use crate::db::Database;
use crate::state::CoreStateInner;
use axum::http::StatusCode;
use axum::{Router, routing::get};
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Clone, Default)]
struct FakeImport {
    files: Arc<Mutex<Vec<Value>>>,
    bodies: Arc<Mutex<Vec<Value>>>,
    posts: Arc<AtomicUsize>,
    mode: usize, // 0 success, 1 saved then 500, 2 rejected 400, 3 uncertain 500
}

async fn listing(State(fake): State<FakeImport>) -> impl IntoResponse {
    (
        [("x-cpa-version", "7.2.152")],
        Json(json!({"files":fake.files.lock().clone()})),
    )
}

async fn upload(
    State(fake): State<FakeImport>,
    Query(query): Query<HashMap<String, String>>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    fake.posts.fetch_add(1, Ordering::SeqCst);
    fake.bodies.lock().push(body.clone());
    if fake.mode <= 1 {
        fake.files.lock().push(json!({"name":query["name"],"type":body["type"], "auth_index":"1", "runtime_only":false}));
    }
    let status = match fake.mode {
        0 => StatusCode::OK,
        2 => StatusCode::BAD_REQUEST,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (
        status,
        Json(json!({"error":"echoed-private-access-token", "status":"ok"})),
    )
}

fn fixture() -> (std::path::PathBuf, CoreState) {
    let dir = std::env::temp_dir().join(format!("ocg-cli-import-api-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let state = Arc::new(
        CoreStateInner::new(
            Database::open(dir.clone()).unwrap(),
            dir.clone(),
            Arc::new(StaticKeyCipher::new("import-test")),
        )
        .unwrap(),
    );
    (dir, state)
}

fn input(state: &CoreState) -> CpaCliImportRequest {
    CpaCliImportRequest {
        provider: CpaOAuthProvider::Codex,
        expectation: MutationExpectation {
            expected_revision: state.settings_revision(),
            process_generation: state.process_generation(),
        },
    }
}

fn credential() -> ImportedCredential {
    ImportedCredential {
        name: "ocg-cli-codex-test.json".into(),
        cpa_provider: "codex",
        payload: json!({"type":"codex","access_token":"private-access","refresh_token":"private-refresh"}),
    }
}

async fn server(mode: usize) -> (FakeImport, CpaClient, tokio::task::JoinHandle<()>) {
    let fake = FakeImport {
        mode,
        ..Default::default()
    };
    let app = Router::new()
        .route("/v0/management/auth-files", get(listing).post(upload))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = CpaClient::new(
        &Default::default(),
        &base,
        "management".into(),
        "inference".into(),
        false,
    )
    .unwrap();
    (fake, client, server)
}

async fn body(response: Response) -> Value {
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let bytes = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn import_uploads_once_reads_back_and_reconciles_retry_without_secret_response() {
    for mode in [0, 1] {
        let (dir, state) = fixture();
        let (fake, client, server) = server(mode).await;
        let revision = state.settings_revision();
        let result = body(cli_import_response(
            import_cli_credential(&state, &client, input(&state), credential())
                .await
                .map_err(|error| error.body.message)
                .unwrap(),
        ))
        .await;
        assert_eq!(result["outcome"], "imported");
        assert!(!result.to_string().contains("private"));
        assert_eq!(state.settings_revision(), revision + 1);
        let result = body(cli_import_response(
            import_cli_credential(&state, &client, input(&state), credential())
                .await
                .map_err(|error| error.body.message)
                .unwrap(),
        ))
        .await;
        assert_eq!(result["outcome"], "alreadyImported");
        assert_eq!(fake.posts.load(Ordering::SeqCst), 1);
        assert_eq!(state.settings_revision(), revision + 1);
        assert_eq!(fake.bodies.lock()[0]["refresh_token"], "private-refresh");
        server.abort();
        drop(state);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[tokio::test]
async fn rejected_upload_is_sanitized_and_uncertain_upload_is_explicit() {
    for mode in [2, 3] {
        let (dir, state) = fixture();
        let (_, client, server) = server(mode).await;
        let revision = state.settings_revision();
        let result = import_cli_credential(&state, &client, input(&state), credential()).await;
        if mode == 2 {
            let error = result.unwrap_err();
            assert!(
                !serde_json::to_string(&error.body)
                    .unwrap()
                    .contains("echoed-private")
            );
            assert_eq!(state.settings_revision(), revision);
        } else {
            assert_eq!(
                body(cli_import_response(
                    result.map_err(|error| error.body.message).unwrap(),
                ))
                .await["outcome"],
                "unconfirmed"
            );
            assert_eq!(state.settings_revision(), revision + 1);
        }
        server.abort();
        drop(state);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[tokio::test]
async fn import_rejects_nonlocal_and_stale_requests_before_upload() {
    let (dir, state) = fixture();
    let (fake, client, server) = server(0).await;
    let mut headers = HeaderMap::new();
    headers.insert("host", HeaderValue::from_static("localhost:9042"));
    assert!(require_local_cli_import(&state, &headers).is_err());
    state.set_dashboard_local_mode(true);
    assert!(require_local_cli_import(&state, &headers).is_ok());
    headers.insert("x-forwarded-for", HeaderValue::from_static("127.0.0.1"));
    assert!(require_local_cli_import(&state, &headers).is_err());
    let mut stale = input(&state);
    stale.expectation.expected_revision += 1;
    assert!(
        import_cli_credential(&state, &client, stale, credential())
            .await
            .is_err()
    );
    assert_eq!(fake.posts.load(Ordering::SeqCst), 0);
    assert!(
        serde_json::from_value::<CpaCliImportRequest>(
            json!({"provider":"codex","path":"secret","expectedRevision":1,"processGeneration":1})
        )
        .is_err()
    );
    server.abort();
    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn import_never_overwrites_case_variant_existing_filenames() {
    for provider in ["codex", "claude"] {
        let (dir, state) = fixture();
        let (fake, client, server) = server(0).await;
        fake.files
            .lock()
            .push(json!({"name":"OCG-CLI-CODEX-TEST.JSON", "type":provider, "auth_index":"1"}));
        let revision = state.settings_revision();
        let result = import_cli_credential(&state, &client, input(&state), credential()).await;
        if provider == "codex" {
            assert_eq!(
                body(cli_import_response(
                    result.map_err(|error| error.body.message).unwrap(),
                ))
                .await["outcome"],
                "alreadyImported"
            );
        } else {
            assert!(result.is_err());
        }
        assert_eq!(fake.posts.load(Ordering::SeqCst), 0);
        assert_eq!(state.settings_revision(), revision);
        server.abort();
        drop(state);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
