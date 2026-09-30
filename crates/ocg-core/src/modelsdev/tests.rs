use super::*;
use axum::{Router, http::HeaderMap, routing::get};

fn sample_api() -> &'static [u8] {
    br#"{
        "lab": {
            "id": "lab",
            "name": "Lab",
            "models": {
                "vision-model": {
                    "name": "Vision Model",
                    "limit": {"context": 262144, "output": 32768},
                    "modalities": {"input": ["text", "image", "pdf"], "output": ["text"]},
                    "reasoning": true,
                    "tool_call": true
                },
                "plain-model": {
                    "limit": {"context": 64000},
                    "modalities": {"input": ["text"], "output": ["text"]}
                },
                "pdf-only": {
                    "limit": {"context": 1000, "output": 100},
                    "modalities": {"input": ["pdf"], "output": ["text"]}
                }
            }
        },
        "mirror": {
            "models": {
                "vision-model": {
                    "limit": {"context": 131072, "output": 16384},
                    "modalities": {"input": ["text", "image"], "output": ["text"]},
                    "reasoning": true,
                    "tool_call": false
                }
            }
        },
        "broken": {"no_models_here": true}
    }"#
}

#[test]
fn api_rows_are_normalized_and_unsupported_modalities_dropped() {
    let rows = parse_api(sample_api());
    // The same id under two providers keeps only common guarantees: the
    // minimum limits, the modality intersection, and the conservative bool.
    let vision = &rows["vision-model"];
    assert_eq!(vision.context_window, Some(131072));
    assert_eq!(vision.max_output_tokens, Some(16384));
    assert_eq!(
        vision.input_modalities.as_ref().unwrap(),
        &["text", "image"]
    );
    assert_eq!(vision.output_modalities.as_ref().unwrap(), &["text"]);
    assert_eq!(vision.reasoning, Some(true));
    assert_eq!(vision.tool_calling, Some(false));
}

#[test]
fn unsupported_only_modality_lists_become_unknown_not_text_fallback() {
    let rows = parse_api(sample_api());
    let pdf_only = &rows["pdf-only"];
    assert_eq!(pdf_only.input_modalities, None);
    assert_eq!(pdf_only.output_modalities.as_ref().unwrap(), &["text"]);
    assert_eq!(pdf_only.context_window, Some(1000));
}

#[test]
fn reasoning_options_effort_values_become_selector_levels() {
    let rows = parse_api(
        br#"{
        "lab": {"models": {
            "thinking": {
                "reasoning": true,
                "reasoning_options": [
                    {"type": "toggle"},
                    {"type": "effort", "values": ["none", "low", "high", "xhigh"]},
                    {"type": "budget_tokens", "min": 1024}
                ]
            },
            "toggle-only": {
                "reasoning": true,
                "reasoning_options": [{"type": "toggle"}]
            }
        }}
    }"#,
    );
    let thinking = &rows["thinking"];
    let efforts = thinking.reasoning_efforts.as_ref().unwrap();
    assert_eq!(efforts.get("off").map(String::as_str), Some("none"));
    assert_eq!(efforts.get("low").map(String::as_str), Some("low"));
    assert_eq!(efforts.get("high").map(String::as_str), Some("high"));
    assert_eq!(efforts.get("xhigh").map(String::as_str), Some("xhigh"));
    assert_eq!(efforts.len(), 4);
    assert_eq!(thinking.reasoning, Some(true));
    // A toggle carries no selectable wire level: unknown, not fabricated.
    assert_eq!(rows["toggle-only"].reasoning_efforts, None);
}

#[test]
fn effort_values_outside_the_selector_table_are_dropped() {
    let rows = parse_api(
        br#"{
        "lab": {"models": {
            "alien": {
                "reasoning": true,
                "reasoning_options": [{"type": "effort", "values": ["low", "turbo"]}]
            }
        }}
    }"#,
    );
    let efforts = rows["alien"].reasoning_efforts.as_ref().unwrap();
    assert_eq!(efforts.get("low").map(String::as_str), Some("low"));
    assert_eq!(efforts.len(), 1);
}

#[test]
fn duplicate_ids_keep_only_agreeing_effort_spellings() {
    let rows = parse_api(br#"{
        "a": {"models": {"m": {"reasoning": true, "reasoning_options": [{"type": "effort", "values": ["low", "high"]}]}}},
        "b": {"models": {"m": {"reasoning": true, "reasoning_options": [{"type": "effort", "values": ["high", "max"]}]}}}
    }"#);
    let efforts = rows["m"].reasoning_efforts.as_ref().unwrap();
    let pairs: Vec<_> = efforts
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    assert_eq!(pairs, [("high", "high")]);
}

#[test]
fn invalid_payloads_yield_an_empty_catalog() {
    assert!(parse_api(b"not json").is_empty());
    assert!(parse_api(br#"{"provider":{"models":[]}}"#).is_empty());
}

#[test]
fn lookup_prefers_exact_upstream_then_tail_then_public() {
    let mut catalog = ModelsDevCatalog::default();
    for (id, context) in [("org/model", 1000_u64), ("model", 2000), ("public", 3000)] {
        catalog.models.insert(
            id.to_string(),
            ModelMetadata {
                context_window: Some(context),
                ..Default::default()
            },
        );
    }
    assert_eq!(
        lookup(&catalog, "public", "org/model")
            .unwrap()
            .context_window,
        Some(1000)
    );
    assert_eq!(
        lookup(&catalog, "public", "other/model")
            .unwrap()
            .context_window,
        Some(2000)
    );
    assert_eq!(
        lookup(&catalog, "public", "unlisted")
            .unwrap()
            .context_window,
        Some(3000)
    );
    assert!(lookup(&catalog, "alias", "unlisted").is_none());
}

#[tokio::test]
async fn fetch_is_keyless_and_parses_the_catalog() {
    let app = Router::new().route(
        "/api.json",
        get(|headers: HeaderMap| async move {
            assert!(headers.get(reqwest::header::AUTHORIZATION).is_none());
            assert!(headers.get("x-api-key").is_none());
            axum::Json(serde_json::json!({
                "lab": {"models": {"m": {"limit": {"context": 8000, "output": 1000}}}}
            }))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let catalog = fetch_catalog_at(reqwest::Client::new(), &format!("http://{addr}/api.json"))
        .await
        .unwrap();
    assert!(catalog.fetched_at.is_some());
    assert_eq!(catalog.models["m"].context_window, Some(8000));
    assert!(catalog.is_fresh(Utc::now()));
}

#[tokio::test]
async fn fetch_rejects_http_failures_and_oversized_bodies() {
    let app = Router::new().route(
        "/api.json",
        get(|| async { axum::http::StatusCode::BAD_GATEWAY }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let error = fetch_catalog_at(reqwest::Client::new(), &format!("http://{addr}/api.json"))
        .await
        .unwrap_err();
    assert!(error.contains("502"));
}
