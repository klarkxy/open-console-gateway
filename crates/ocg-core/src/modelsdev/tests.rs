use super::*;
use axum::{Router, http::HeaderMap, routing::get};
use ocg_domain::destination::{
    AdapterKind, AuthScheme, CatalogModel, Destination, HttpProtocolRoute, LegacyDestinationRef,
    ModelResolution, Protocol, sealed_capabilities,
};

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

fn wires(metadata: &ModelMetadata) -> Vec<(String, String)> {
    metadata
        .reasoning_efforts
        .clone()
        .unwrap_or_default()
        .into_iter()
        .collect()
}

fn destination_for(routes: &[(&str, Protocol)]) -> Destination {
    destination_for_adapter(routes, AdapterKind::Http)
}

fn destination_for_adapter(routes: &[(&str, Protocol)], adapter: AdapterKind) -> Destination {
    let protocol_routes = routes
        .iter()
        .map(|(url, protocol)| HttpProtocolRoute {
            protocol: *protocol,
            endpoint_url: (*url).to_string(),
            auth_scheme: AuthScheme::Bearer,
        })
        .collect::<Vec<_>>();
    let protocols = protocol_routes
        .iter()
        .map(|route| route.protocol)
        .collect::<Vec<_>>();
    Destination {
        id: "route-one".into(),
        legacy: LegacyDestinationRef::Dynamic("test".into()),
        adapter,
        name: "test".into(),
        brand_family: None,
        base_url: protocol_routes
            .first()
            .map(|route| route.endpoint_url.clone()),
        protocols,
        protocol_routes,
        auth_scheme: AuthScheme::Bearer,
        model_resolution: ModelResolution::PublicAndUpstream,
        catalog: Vec::new(),
        capabilities: sealed_capabilities(adapter),
        plan: None,
        max_credentials: None,
        observer_credential_id: None,
        enabled: true,
    }
}

fn catalog_model(public_model: &str, upstream_model: &str, protocols: &[Protocol]) -> CatalogModel {
    CatalogModel {
        public_model: public_model.to_string(),
        upstream_model: upstream_model.to_string(),
        protocols: protocols.to_vec(),
        preferred: protocols.first().copied(),
        enabled: true,
        upstream_override: None,
    }
}

fn lookup_routes(
    catalog: &ModelsDevCatalog,
    routes: &[(&str, Protocol)],
    model_id: &str,
) -> Option<ModelMetadata> {
    let protocols = routes
        .iter()
        .map(|(_, protocol)| *protocol)
        .collect::<Vec<_>>();
    let destination = destination_for(routes);
    let model = catalog_model(model_id, model_id, &protocols);
    lookup(catalog, &destination, &model)
}

fn lookup_at(catalog: &ModelsDevCatalog, url: &str, model_id: &str) -> Option<ModelMetadata> {
    lookup_at_adapter(catalog, url, model_id, AdapterKind::Http)
}

fn lookup_at_adapter(
    catalog: &ModelsDevCatalog,
    url: &str,
    model_id: &str,
    adapter: AdapterKind,
) -> Option<ModelMetadata> {
    let routes = [(url, Protocol::ChatCompletions)];
    let destination = destination_for_adapter(&routes, adapter);
    let model = catalog_model(model_id, model_id, &[Protocol::ChatCompletions]);
    lookup(catalog, &destination, &model)
}

#[test]
fn api_rows_are_normalized_and_unsupported_modalities_dropped() {
    let catalog = parse_api(sample_api());
    // The same id under two providers keeps only common guarantees: the
    // minimum limits, the modality intersection, and the conservative bool.
    let vision = &catalog.models["vision-model"];
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
    let catalog = parse_api(sample_api());
    let pdf_only = &catalog.models["pdf-only"];
    assert_eq!(pdf_only.input_modalities, None);
    assert_eq!(pdf_only.output_modalities.as_ref().unwrap(), &["text"]);
    assert_eq!(pdf_only.context_window, Some(1000));
}

#[test]
fn reasoning_options_effort_values_become_selector_levels() {
    let catalog = parse_api(
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
            },
            "budget-only": {
                "reasoning": true,
                "reasoning_options": [{"type": "budget_tokens", "min": 1024}]
            },
            "bare": {"reasoning": true}
        }}
    }"#,
    );
    let thinking = &catalog.models["thinking"];
    let efforts = thinking.reasoning_efforts.as_ref().unwrap();
    assert_eq!(efforts.get("off").map(String::as_str), Some("none"));
    assert_eq!(efforts.get("low").map(String::as_str), Some("low"));
    assert_eq!(efforts.get("high").map(String::as_str), Some("high"));
    assert_eq!(efforts.get("xhigh").map(String::as_str), Some("xhigh"));
    assert_eq!(efforts.len(), 4);
    assert_eq!(thinking.reasoning, Some(true));
    // Toggle, budget, and a bare true carry no selectable wire level.
    assert_eq!(catalog.models["toggle-only"].reasoning_efforts, None);
    assert_eq!(catalog.models["budget-only"].reasoning_efforts, None);
    assert_eq!(catalog.models["bare"].reasoning, Some(true));
    assert_eq!(catalog.models["bare"].reasoning_efforts, None);
}

#[test]
fn effort_values_outside_the_selector_table_are_dropped() {
    let catalog = parse_api(
        br#"{
        "lab": {"models": {
            "alien": {
                "reasoning": true,
                "reasoning_options": [{"type": "effort", "values": ["low", "turbo"]}]
            }
        }}
    }"#,
    );
    let efforts = catalog.models["alien"].reasoning_efforts.as_ref().unwrap();
    assert_eq!(efforts.get("low").map(String::as_str), Some("low"));
    assert_eq!(efforts.len(), 1);
}

#[test]
fn duplicate_ids_keep_only_agreeing_effort_spellings() {
    let catalog = parse_api(br#"{
        "a": {"models": {"m": {"reasoning": true, "reasoning_options": [{"type": "effort", "values": ["low", "high"]}]}}},
        "b": {"models": {"m": {"reasoning": true, "reasoning_options": [{"type": "effort", "values": ["high", "max"]}]}}}
    }"#);
    let efforts = catalog.models["m"].reasoning_efforts.as_ref().unwrap();
    let pairs: Vec<_> = efforts
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    assert_eq!(pairs, [("high", "high")]);
}

#[test]
fn invalid_payloads_have_no_model_rows() {
    for body in [b"not json".as_slice(), br#"{"provider":{"models":[]}}"#] {
        let catalog = parse_api(body);
        assert!(
            catalog.models.is_empty(),
            "{}",
            String::from_utf8_lossy(body)
        );
        assert!(!has_usable_offerings(&catalog));
    }
}

#[test]
fn lookup_prefers_exact_upstream_then_tail_then_public() {
    let mut catalog = ModelsDevCatalog {
        fetched_at: Some(Utc::now()),
        ..Default::default()
    };
    for (id, context) in [("org/model", 1000_u64), ("model", 2000), ("public", 3000)] {
        catalog.models.insert(
            id.to_string(),
            ModelMetadata {
                context_window: Some(context),
                ..Default::default()
            },
        );
    }
    // No provider index: the flat map stays readable and is already stale.
    assert!(catalog.offerings.is_none());
    assert!(!catalog.is_fresh(Utc::now()));
    let destination = destination_for(&[(
        "https://proxy.example/v1/chat/completions",
        Protocol::ChatCompletions,
    )]);
    let found = |public_model: &str, upstream_model: &str| {
        let model = catalog_model(public_model, upstream_model, &[Protocol::ChatCompletions]);
        lookup(&catalog, &destination, &model)
            .unwrap()
            .context_window
    };
    assert_eq!(found("public", "org/model"), Some(1000));
    assert_eq!(found("public", "other/model"), Some(2000));
    assert_eq!(found("public", "unlisted"), Some(3000));
    let missing = catalog_model("alias", "unlisted", &[Protocol::ChatCompletions]);
    assert!(lookup(&catalog, &destination, &missing).is_none());
}

fn canonical_fixture() -> ModelsDevCatalog {
    parse_api(
        br#"{
        "openai": {"models": {
            "gpt-5.2": {"reasoning": true, "limit": {"context": 400000, "output": 128000}, "reasoning_options": [{"type": "effort", "values": ["low", "medium", "high", "xhigh"]}]},
            "gpt-5.3-codex": {"reasoning": true, "reasoning_options": [{"type": "effort", "values": ["low", "high", "xhigh"]}]},
            "o3": {"reasoning": true, "reasoning_options": [{"type": "effort", "values": ["low", "medium", "high"]}]}
        }},
        "proxy": {"api": "https://proxy.test/v1", "models": {
            "gpt-5.2": {"canonical_model_id": "openai/gpt-5.2"},
            "gpt-5.3-codex": {"canonical_model_id": "openai/gpt-5.3-codex"},
            "o3": {"canonical_model_id": "openai/o3"},
            "private-model": {"reasoning": true, "reasoning_options": [{"type": "effort", "values": ["low", "high"]}]}
        }},
        "narrow": {"api": "https://narrow.test/v1", "models": {
            "gpt-5.2": {
                "canonical_model_id": "openai/gpt-5.2",
                "reasoning": true,
                "reasoning_options": [{"type": "effort", "values": ["low"]}]
            },
            "empty-tiers": {
                "reasoning": true,
                "reasoning_options": [{"type": "effort", "values": []}]
            }
        }},
        "unrelated": {"api": "https://other.test/v1", "models": {
            "different": {"limit": {"context": 1000}}
        }}
    }"#,
    )
}

#[test]
fn generic_routes_keep_canonical_choices_when_other_providers_omit_the_row() {
    let catalog = canonical_fixture();
    let generic = "https://custom.example/v1/chat/completions";
    assert_eq!(
        wires(&lookup_at(&catalog, generic, "gpt-5.2").unwrap()),
        vec![
            ("high".into(), "high".into()),
            ("low".into(), "low".into()),
            ("medium".into(), "medium".into()),
            ("xhigh".into(), "xhigh".into()),
        ]
    );
    assert_eq!(
        lookup_at(&catalog, generic, "gpt-5.2")
            .unwrap()
            .context_window,
        Some(400000)
    );
    assert_eq!(
        wires(&lookup_at(&catalog, generic, "gpt-5.3-codex").unwrap()),
        vec![
            ("high".into(), "high".into()),
            ("low".into(), "low".into()),
            ("xhigh".into(), "xhigh".into()),
        ]
    );
    assert_eq!(
        wires(&lookup_at(&catalog, generic, "o3").unwrap()),
        vec![
            ("high".into(), "high".into()),
            ("low".into(), "low".into()),
            ("medium".into(), "medium".into()),
        ]
    );
}

#[test]
fn known_provider_rows_are_terminal_for_narrowed_missing_and_empty_tiers() {
    let catalog = canonical_fixture();
    let narrow = "https://narrow.test/v1/chat/completions";
    assert_eq!(
        wires(&lookup_at(&catalog, narrow, "gpt-5.2").unwrap()),
        vec![("low".into(), "low".into())]
    );
    assert_eq!(
        lookup_at(&catalog, narrow, "gpt-5.3-codex"),
        Some(ModelMetadata::default())
    );
    let empty = lookup_at(&catalog, narrow, "empty-tiers").unwrap();
    assert_eq!(empty.reasoning, Some(true));
    assert_eq!(empty.reasoning_efforts, Some(BTreeMap::new()));

    let proxy = "https://proxy.test/v1/chat/completions";
    assert_eq!(
        lookup_at(&catalog, proxy, "gpt-5.2"),
        Some(ModelMetadata::default())
    );
    assert_eq!(
        wires(&lookup_at(&catalog, proxy, "private-model").unwrap()),
        vec![("high".into(), "high".into()), ("low".into(), "low".into()),]
    );
    assert_eq!(
        lookup_at(
            &catalog,
            "https://api.openai.com/v1/chat/completions",
            "private-model"
        ),
        Some(ModelMetadata::default())
    );
}

#[test]
fn canonical_conflicts_and_dangling_targets_stay_conservative() {
    let conflict = parse_api(
        br#"{
        "left": {"api": "https://left.test/v1", "models": {
            "m": {
                "canonical_model_id": "target-a/m",
                "limit": {"context": 3000, "output": 100},
                "reasoning": true,
                "reasoning_options": [{"type": "effort", "values": ["low", "high"]}]
            }
        }},
        "right": {"api": "https://right.test/v1", "models": {
            "m": {
                "canonical_model_id": "target-b/m",
                "limit": {"context": 4000, "output": 100},
                "reasoning": true,
                "reasoning_options": [{"type": "effort", "values": ["high", "max"]}]
            }
        }},
        "target-a": {"models": {"m": {
            "limit": {"context": 9000, "output": 100},
            "reasoning": true,
            "reasoning_options": [{"type": "effort", "values": ["low", "high", "xhigh"]}]
        }}},
        "target-b": {"models": {"m": {
            "limit": {"context": 1000, "output": 100},
            "reasoning": true,
            "reasoning_options": [{"type": "effort", "values": ["high", "max"]}]
        }}}
    }"#,
    );
    let found = lookup_at(&conflict, "https://custom.example/v1/chat/completions", "m").unwrap();
    // Conflicting links keep the common facts of every row with this id.
    // That is narrower than either canonical target.
    assert_eq!(found.context_window, Some(1000));
    assert_eq!(wires(&found), vec![("high".into(), "high".into())]);

    let dangling = parse_api(
        br#"{
        "proxy": {"api": "https://proxy.test/v1", "models": {
            "m": {
                "canonical_model_id": "missing/m",
                "reasoning": true,
                "reasoning_options": [{"type": "effort", "values": ["low"]}]
            }
        }}
    }"#,
    );
    assert_eq!(
        wires(&lookup_at(&dangling, "https://custom.example/v1/chat/completions", "m").unwrap()),
        vec![("low".into(), "low".into())]
    );
}

#[test]
fn namespace_reads_the_named_row_without_following_its_canonical_link() {
    let catalog = parse_api(
        br#"{
        "openai": {"models": {"gpt-5.2": {
            "canonical_model_id": "other/gpt-5.2",
            "reasoning": true,
            "reasoning_options": [{"type": "effort", "values": ["low"]}]
        }}},
        "other": {"models": {"gpt-5.2": {
            "reasoning": true,
            "reasoning_options": [{"type": "effort", "values": ["low", "high"]}]
        }}}
    }"#,
    );
    assert_eq!(
        wires(
            &lookup_at(
                &catalog,
                "https://custom.example/v1/chat/completions",
                "openai/gpt-5.2"
            )
            .unwrap()
        ),
        vec![("low".into(), "low".into())]
    );
}

#[test]
fn provider_match_requires_scheme_host_port_and_path_boundary() {
    let catalog = parse_api(
        br#"{
        "lab": {"api": "https://lab.test/v1", "models": {
            "m": {
                "canonical_model_id": "canon/m",
                "reasoning": true,
                "reasoning_options": [{"type": "effort", "values": ["low"]}]
            }
        }},
        "canon": {"api": "https://canon.test/v1", "models": {
            "m": {"reasoning": true, "reasoning_options": [{"type": "effort", "values": ["low", "high"]}]}
        }}
    }"#,
    );
    let baseline = vec![("high".into(), "high".into()), ("low".into(), "low".into())];
    assert_eq!(
        wires(&lookup_at(&catalog, "https://lab.test/v1/chat/completions", "m").unwrap()),
        vec![("low".into(), "low".into())]
    );
    for url in [
        "https://other.test/v1/chat/completions",
        "http://lab.test/v1/chat/completions",
        "https://lab.test:8443/v1/chat/completions",
        "https://user:pass@lab.test/v1/chat/completions",
        "https://lab.test/v1extra/chat/completions",
    ] {
        assert_eq!(
            wires(&lookup_at(&catalog, url, "m").unwrap()),
            baseline,
            "{url}"
        );
    }
}

#[test]
fn model_provider_api_overrides_the_provider_and_stays_terminal() {
    let catalog = parse_api(
        br#"{
        "root": {"api": "https://root.test/v1", "models": {
            "m": {
                "api": "https://wrong.test/v1",
                "provider": {"npm": "@ai-sdk/openai-compatible", "api": "https://model.test/v1"},
                "canonical_model_id": "canonical/m",
                "reasoning": true,
                "reasoning_options": [{"type": "effort", "values": ["low"]}]
            }
        }},
        "canonical": {"api": "https://canonical.test/v1", "models": {
            "m": {"reasoning": true, "reasoning_options": [{"type": "effort", "values": ["low", "high"]}]}
        }}
    }"#,
    );
    let stored = &catalog.offerings.as_ref().unwrap().providers["root"].models["m"];
    assert_eq!(stored.api.as_ref().unwrap().host, "model.test");
    assert_eq!(
        wires(&lookup_at(&catalog, "https://model.test/v1/chat/completions", "m").unwrap()),
        vec![("low".into(), "low".into())]
    );
    assert_eq!(
        lookup_at(&catalog, "https://root.test/v1/chat/completions", "m"),
        Some(ModelMetadata::default())
    );
    assert_eq!(
        wires(&lookup_at(&catalog, "https://wrong.test/v1/chat/completions", "m").unwrap()),
        vec![("high".into(), "high".into()), ("low".into(), "low".into()),]
    );
}

#[test]
fn each_protocol_route_contributes_verified_facts_or_its_baseline() {
    let catalog = parse_api(
        br#"{
        "known": {"api": "https://known.test/v1", "models": {
            "m": {"reasoning": true, "reasoning_options": [{"type": "effort", "values": ["low", "high"]}]}
        }},
        "proxy": {"api": "https://proxy.test/v1", "models": {
            "m": {"canonical_model_id": "canon/m"}
        }},
        "canon": {"api": "https://canon.test/v1", "models": {
            "m": {"reasoning": true, "reasoning_options": [{"type": "effort", "values": ["low"]}]}
        }}
    }"#,
    );
    let found = lookup_routes(
        &catalog,
        &[
            (
                "https://known.test/v1/chat/completions",
                Protocol::ChatCompletions,
            ),
            ("https://unknown.test/v1/responses", Protocol::Responses),
        ],
        "m",
    )
    .unwrap();
    assert_eq!(wires(&found), vec![("low".into(), "low".into())]);

    // A known route with no row is terminal. An unknown route with no
    // baseline contributes unknown as well, so nothing is invented.
    assert_eq!(
        lookup_routes(
            &catalog,
            &[
                (
                    "https://known.test/v1/chat/completions",
                    Protocol::ChatCompletions,
                ),
                ("https://unknown.test/v1/responses", Protocol::Responses),
            ],
            "missing-model",
        ),
        Some(ModelMetadata::default())
    );
    assert!(
        lookup_at(
            &catalog,
            "https://custom.example/v1/chat/completions",
            "missing-model"
        )
        .is_none()
    );
}

#[test]
fn most_specific_provider_path_without_the_model_is_terminal() {
    let catalog = parse_api(
        br#"{
        "broad": {"api": "https://host.test/v1", "models": {
            "m": {"reasoning": true, "reasoning_options": [{"type": "effort", "values": ["low", "high"]}]}
        }},
        "restricted": {"api": "https://host.test/v1/limited", "models": {
            "other": {"reasoning": true, "reasoning_options": [{"type": "effort", "values": ["low"]}]}
        }},
        "silent": {"api": "https://host.test/v1/quiet", "models": {}},
        "noise": {"models": {}}
    }"#,
    );
    let providers = &catalog.offerings.as_ref().unwrap().providers;
    assert!(providers["silent"].models.is_empty());
    assert!(providers["silent"].api.is_some());
    assert!(!providers.contains_key("noise"));
    assert_eq!(
        wires(&lookup_at(&catalog, "https://host.test/v1/chat/completions", "m").unwrap()),
        vec![("high".into(), "high".into()), ("low".into(), "low".into()),]
    );
    assert_eq!(
        lookup_at(
            &catalog,
            "https://host.test/v1/limited/chat/completions",
            "m"
        ),
        Some(ModelMetadata::default())
    );
    assert_eq!(
        lookup_at(&catalog, "https://host.test/v1/quiet/chat/completions", "m"),
        Some(ModelMetadata::default())
    );
}

#[test]
fn tied_api_keeps_a_missing_model_in_the_winning_set() {
    let catalog = parse_api(
        br#"{"kimi-code-plan-cn":{"api":"https://api.kimi.com/coding/v1","models":{}},"mirror":{"api":"https://api.kimi.com/coding/v1","models":{"m":{"reasoning":true,"reasoning_options":[{"type":"effort","values":["low","high"]}]}}}}"#,
    );
    let url = "https://api.kimi.com/coding/v1/chat/completions";
    assert_eq!(
        lookup_at_adapter(&catalog, url, "m", AdapterKind::Kimi),
        Some(ModelMetadata::default())
    );
    let http = lookup_at_adapter(&catalog, url, "m", AdapterKind::Http).unwrap();
    assert_eq!(http.reasoning, None);
    assert_eq!(http.reasoning_efforts, None);
}

#[test]
fn current_offerings_round_trip_through_the_cache_document() {
    let catalog = canonical_fixture();
    let encoded = serde_json::to_string(&catalog).unwrap();
    let restored: ModelsDevCatalog = serde_json::from_str(&encoded).unwrap();
    assert_eq!(
        serde_json::to_value(&catalog).unwrap(),
        serde_json::to_value(&restored).unwrap()
    );
    let url = "https://custom.example/v1/chat/completions";
    assert_eq!(
        lookup_at(&catalog, url, "gpt-5.2"),
        lookup_at(&restored, url, "gpt-5.2")
    );
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

#[tokio::test]
async fn unusable_http_200_keeps_the_last_good_cache() {
    let good = serde_json::json!({
        "lab": {"models": {"m": {"limit": {"context": 8000, "output": 1000}}}}
    });
    let app = Router::new()
        .route(
            "/good",
            get(move || {
                let good = good.clone();
                async move { axum::Json(good) }
            }),
        )
        .route(
            "/invalid",
            get(|| async { (axum::http::StatusCode::OK, "not json") }),
        )
        .route(
            "/shape",
            get(|| async { (axum::http::StatusCode::OK, "[]") }),
        )
        .route(
            "/empty",
            get(|| async { (axum::http::StatusCode::OK, "{}") }),
        )
        .route(
            "/api-only",
            get(|| async {
                (
                    axum::http::StatusCode::OK,
                    r#"{"only":{"api":"https://host.test/v1","models":{}}}"#,
                )
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::new();
    let root = format!("http://{addr}");

    let dir = std::env::temp_dir().join(format!("ocg-modelsdev-cache-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Mutex::new(crate::db::Database::open(dir.clone()).unwrap());
    let live = RwLock::new(std::sync::Arc::new(ModelsDevCatalog::default()));
    let fetched = fetch_catalog_at(client.clone(), &format!("{root}/good"))
        .await
        .unwrap();
    install_refresh(&db, &live, Ok(fetched));
    let kept = {
        let guard = db.lock();
        serde_json::to_value(load(&guard).unwrap()).unwrap()
    };
    assert_eq!(kept["models"]["m"]["contextWindow"], 8000);
    assert_eq!(serde_json::to_value(live.read().as_ref()).unwrap(), kept);

    for path in ["/invalid", "/shape", "/empty", "/api-only"] {
        let error = fetch_catalog_at(client.clone(), &format!("{root}{path}"))
            .await
            .expect_err(path);
        assert!(error.contains("no usable offerings"), "{path}: {error}");
        install_refresh(&db, &live, Err(error));
        let stored = {
            let guard = db.lock();
            serde_json::to_value(load(&guard).unwrap()).unwrap()
        };
        assert_eq!(stored, kept, "{path}");
        assert_eq!(
            serde_json::to_value(live.read().as_ref()).unwrap(),
            kept,
            "{path}"
        );
    }
    drop(db);
    drop(live);
    std::fs::remove_dir_all(&dir).unwrap();
}
