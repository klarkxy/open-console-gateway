use super::super::types::{
    AccountUpstreamProtocol, ProviderDefinitionAuthKind, ProviderDefinitionTestRequest,
};
use super::test_provider;
use crate::crypto::StaticKeyCipher;
use crate::db::Database;
use crate::state::CoreState;
use crate::state::CoreStateInner;
use axum::body::Bytes;
use axum::extract::State;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;

struct Draft {
    state: Option<CoreState>,
    dir: PathBuf,
}

impl Draft {
    fn open(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "ocg-draft-provider-test-{label}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Database::open(dir.clone()).unwrap();
        let cipher = Arc::new(StaticKeyCipher::new("draft-provider-test"));
        let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());
        Self {
            state: Some(state),
            dir,
        }
    }

    fn state(&self) -> CoreState {
        self.state.as_ref().unwrap().clone()
    }
}

impl Drop for Draft {
    fn drop(&mut self) {
        self.state.take();
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

fn request(
    endpoint: &str,
    auth: ProviderDefinitionAuthKind,
    key: Option<&str>,
    public_model: &str,
) -> Bytes {
    Bytes::from(
        serde_json::to_vec(&ProviderDefinitionTestRequest {
            endpoint_url: endpoint.to_string(),
            upstream_protocol: AccountUpstreamProtocol::ChatCompletions,
            auth_kind: auth,
            public_model: public_model.to_string(),
            upstream_model: "vendor/opus".into(),
            key: key.map(str::to_string),
        })
        .unwrap(),
    )
}

#[tokio::test]
async fn draft_provider_test_does_not_open_a_connection() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!(
        "http://{}/v1/chat/completions",
        listener.local_addr().unwrap()
    );
    let (arrived_tx, mut arrived_rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move {
        if listener.accept().await.is_ok() {
            let _ = arrived_tx.send(());
        }
    });
    let draft = Draft::open("local");
    let state = draft.state();
    let before = state.settings_revision();
    let secret = "sk-draft-must-not-send";

    let refused = test_provider(
        State(state.clone()),
        request(
            &endpoint,
            ProviderDefinitionAuthKind::Bearer,
            Some(secret),
            "lab-opus",
        ),
    )
    .await
    .expect_err("a draft has no saved credential");
    let refused_text = format!("{refused:?}");
    assert!(
        refused_text.contains("preconditionFailed"),
        "{refused_text}"
    );
    assert!(refused_text.contains("model test"), "{refused_text}");
    assert!(!refused_text.contains(secret), "{refused_text}");

    let anonymous = test_provider(
        State(state.clone()),
        request(
            &endpoint,
            ProviderDefinitionAuthKind::None,
            None,
            "lab-opus",
        ),
    )
    .await
    .expect_err("no-auth draft still has no saved binding");
    assert!(
        format!("{anonymous:?}").contains("preconditionFailed"),
        "{anonymous:?}"
    );

    let missing_key = test_provider(
        State(state.clone()),
        request(
            &endpoint,
            ProviderDefinitionAuthKind::Bearer,
            None,
            "lab-opus",
        ),
    )
    .await
    .expect_err("keyed auth still requires a key in the draft body");
    let missing_text = format!("{missing_key:?}");
    assert!(missing_text.contains("invalidRequest"), "{missing_text}");
    assert!(missing_text.contains("key is required"), "{missing_text}");

    let empty_model = test_provider(
        State(state.clone()),
        request(
            &endpoint,
            ProviderDefinitionAuthKind::Bearer,
            Some(secret),
            "  ",
        ),
    )
    .await
    .expect_err("local model validation remains");
    let empty_text = format!("{empty_model:?}");
    assert!(empty_text.contains("invalidRequest"), "{empty_text}");
    assert!(!empty_text.contains(secret), "{empty_text}");

    assert_eq!(state.settings_revision(), before);
    assert!(
        state.db.lock().list_dynamic_providers().unwrap().is_empty(),
        "a draft test must not create a provider"
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), arrived_rx.recv())
            .await
            .is_err(),
        "draft test must not connect"
    );
    task.abort();
}
