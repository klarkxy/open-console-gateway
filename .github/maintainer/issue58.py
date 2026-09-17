from pathlib import Path

def replace(path, old, new):
    p = Path(path); s = p.read_text(); assert s.count(old) == 1, (path, s.count(old), old[:100]); p.write_text(s.replace(old, new))

replace('crates/ocg-core/src/gateway/materialize.rs', '''/// are not rejected against the Go protocol table. Pure builtin resolutions
/// keep their normal early validation.''', '''/// are not rejected against the Go protocol table. A Go mapping admitted by a
/// refreshed catalog may also lack a checked-in profile: defer its protocol to
/// the effective per-candidate contract instead of vetoing it for diagnostics.
/// Known builtin profiles keep their normal early validation.''')
replace('crates/ocg-core/src/gateway/materialize.rs', '''    preserve_client.then_some(client)
}

fn mapping_preserves_client_wire''', '''    preserve_client.then_some(match client {
        // Gemini is client-only. This diagnostic conversion does not select
        // the actual upstream, which still comes from the candidate contract.
        ApiFormat::Gemini => ApiFormat::ChatCompletions,
        client => client,
    })
}

fn mapping_preserves_client_wire''')
replace('crates/ocg-core/src/gateway/materialize.rs', '''        || mapping_is_ollama_cloud(mapping)
}''', '''        || mapping_is_ollama_cloud(mapping)
        || (mapping.is_opencode_go()
            && crate::kernel::protocol::model_protocol(&mapping.upstream_model).is_none())
}''')
p = Path('crates/ocg-core/src/gateway/materialize/tests.rs')
p.write_text(p.read_text() + '''
#[test]
fn diagnostic_plan_does_not_veto_a_refreshed_go_model_missing_a_static_profile() {
    for name in ["muse-spark-1.3-contributor", "omen-alpha", "future-go-model"] {
        let go = vec![name.to_string()];
        let resolved = alias::resolve_with_runtime_catalogs(name, RuntimeCatalogs { go: &go, ..Default::default() }).unwrap();
        for client in [ApiFormat::ChatCompletions, ApiFormat::Responses, ApiFormat::Messages] {
            assert_eq!(diagnostic_forced_upstream(&resolved, client), Some(client));
        }
        assert_eq!(diagnostic_forced_upstream(&resolved, ApiFormat::Gemini), Some(ApiFormat::ChatCompletions));
        assert!(alias::resolve_with_runtime_catalogs(name, RuntimeCatalogs::default()).is_err());
    }
    let go = vec!["grok-4.6".to_string()];
    let known = alias::resolve_with_runtime_catalogs("grok-4.6", RuntimeCatalogs { go: &go, ..Default::default() }).unwrap();
    assert_eq!(diagnostic_forced_upstream(&known, ApiFormat::Responses), None);
}
''')
Path('crates/ocg-core/tests/gateway_go_catalog_models.rs').write_text('''//! Runtime catalog models must reach their selected upstream through the public gateway.
use axum::http::StatusCode;
use chrono::Utc;
use ocg_core::dashboard_v3::OfficialProtocolBaseline;
use ocg_core::provider::{OPENCODE_GO_BASE_URL, OPENCODE_PROVIDER_ID, UpstreamProtocolKind};
use ocg_core::provider_contracts::{CATALOG_SOURCE_OPENCODE_MODELS, ContractScope, ProtocolOverrideState};
use ocg_core::state::CoreStateInner;
use serde_json::{Value, json};
#[path = "fixtures/gateway_fallback.rs"]
mod fixture;
use fixture::*;

fn set_catalog(state: &CoreStateInner, model: &str, protocol: UpstreamProtocolKind, enabled: bool) {
    let scope = ContractScope::provider(OPENCODE_PROVIDER_ID);
    let now = Utc::now();
    let models = vec![model.to_string()];
    {
        let db = state.db.lock();
        db.set_contract_catalog(&scope, &models, Some(now), CATALOG_SOURCE_OPENCODE_MODELS, OPENCODE_GO_BASE_URL, now).unwrap();
        // This is a deterministic docs fixture, not a live upstream capability claim.
        db.apply_official_protocol_baseline(&scope, &models, &OfficialProtocolBaseline::mapped([(model, protocol)]), now).unwrap();
        db.set_model_protocol_overrides(&scope, &[(model.to_string(), protocol, if enabled { ProtocolOverrideState::ForceOn } else { ProtocolOverrideState::ForceOff })], now).unwrap();
    }
    state.reload_provider_contracts().unwrap();
}

#[tokio::test]
async fn issue58_refreshed_go_models_reach_correct_upstream_and_preserve_client_name() {
    for (model, protocol, expected_path, response) in [
        ("muse-spark-1.3-contributor", UpstreamProtocolKind::Responses, "/v1/responses", ok_responses()),
        ("omen-alpha", UpstreamProtocolKind::ChatCompletions, "/v1/chat/completions", ok()),
        ("future-go-model", UpstreamProtocolKind::Messages, "/v1/messages", ok_messages()),
    ] {
        let p = PreparedFallback::go(&[("key-1", &[response.clone(), response.clone(), response.clone(), response])], &["key-1"]).await;
        set_catalog(&p.state, model, protocol, true);
        let h = p.bind().await;
        for path in ["/v1/responses", "/v1/chat/completions", "/v1/messages"] {
            let (status, body) = h.protocol(path, model).await;
            assert_eq!(status, StatusCode::OK, "{model} {path}: {body}");
            assert_eq!(body["model"], model);
        }
        let (status, body) = gemini_call(h.port, model).await;
        assert_eq!(status, StatusCode::OK, "{model} Gemini: {body}");
        let calls = h.calls.lock().unwrap();
        assert_eq!(calls.len(), 4);
        for call in calls.iter() {
            assert_eq!(call.path, expected_path);
            let body: Value = serde_json::from_str(&call.body).unwrap();
            assert_eq!(body["model"], model);
            assert_eq!(call.key, "key-1");
        }
    }
}

#[tokio::test]
async fn issue58_unlisted_disabled_and_removed_models_never_send_upstream() {
    let p = PreparedFallback::go(&[("key-1", &[ok()])], &["key-1"]).await;
    set_catalog(&p.state, "omen-alpha", UpstreamProtocolKind::ChatCompletions, false);
    let h = p.bind().await;
    for model in ["omen-alpha", "model-not-in-catalog"] {
        let (status, _) = h.protocol("/v1/chat/completions", model).await;
        assert_ne!(status, StatusCode::OK);
    }
    assert!(h.calls.lock().unwrap().is_empty());
    set_catalog(&h.state, "omen-alpha", UpstreamProtocolKind::ChatCompletions, true);
    assert_eq!(h.protocol("/v1/chat/completions", "omen-alpha").await.0, StatusCode::OK);
    set_catalog(&h.state, "replacement-model", UpstreamProtocolKind::ChatCompletions, true);
    let (status, _) = h.protocol("/v1/chat/completions", "omen-alpha").await;
    assert_ne!(status, StatusCode::OK);
    assert_eq!(h.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn issue58_protocol_constraints_are_still_enforced_by_actual_candidate() {
    let p = PreparedFallback::go(&[("key-1", &[ok()])], &["key-1"]).await;
    set_catalog(&p.state, "omen-alpha", UpstreamProtocolKind::ChatCompletions, true);
    let h = p.bind().await;
    let response = loopback_client().post(format!("http://127.0.0.1:{}/v1/responses", h.port))
        .bearer_auth("gw-test")
        .json(&json!({"model":"omen-alpha","input":"hello","store":true}))
        .send().await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(h.calls.lock().unwrap().is_empty());
}
''')
for path, text in [
('docs/user/routing.md', '\n### Newly discovered OpenCode Go models\n\nRefresh the model catalog on Providers, then explicitly enable newly discovered models and select the supported upstream protocol. Models in the saved Go catalog use their effective model contract even when no checked-in alias/protocol profile exists. Diagnostic planning cannot reject such models merely for being new. This does not enable unknown, disabled or removed models, and does not probe protocols during inference. A local catalog/protocol test is not proof that a live account has access to the model.\n'),
('docs/user/routing.zh-CN.md', '\n### OpenCode Go 新发现的模型\n\n在供应商页刷新模型目录后，明确启用新发现的模型并选择其支持的上游协议。已保存到 Go 目录的模型按有效模型合约处理，即使代码中没有静态别名或协议条目，诊断计划也不会仅因为它是新模型而拒绝请求。未发现、禁用或已删除的模型不会因此获得路由资格，推理时也不会试探协议。本地目录与协议测试不代表真实账号已获得模型权限。\n')]:
    p=Path(path); s=p.read_text(); p.write_text(s.replace('\n## ', text+'\n## ', 1))
