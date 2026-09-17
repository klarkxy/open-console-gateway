from pathlib import Path
def edit(path, old, new):
    p=Path(path);s=p.read_text();assert s.count(old)==1,(path,s.count(old),old[:100]);p.write_text(s.replace(old,new))
p=Path('crates/ocg-core/src/gateway/forwarder.rs');s=p.read_text()
s=s.replace('    Platform(PlatformAttemptPrice),\n    Unpriced,','    Platform(PlatformAttemptPrice),\n    OfficialApi(crate::official_api::OfficialAttemptPrice),\n    Unpriced,',1)
s=s.replace('            Self::Platform(price) => price.estimate(),','''            Self::Platform(price) => price.estimate(),
            Self::OfficialApi(price) => {
                let amount = (model == price.model).then(|| price.amount(prompt_tokens, completion_tokens, cached_tokens, cache_creation_tokens)).flatten();
                let usd = amount.filter(|_| price.sheet.kind.currency() == "USD");
                crate::kernel::pricing::PricingEstimate {
                    raw_cost_usd: usd, quota_debit: None, effective_paid_cost_usd: None,
                    cost: usd, pricing_revision_id: Some(price.sheet.revision.clone()),
                    quota_multiplier: None, local_adjustment_multiplier: None,
                    cost_state: if usd.is_some() { "priced" } else if amount.is_some() { "unknown" } else { "unpriced" },
                }
            }''',1)
s=s.replace('            Self::Platform(price) => price.provenance(),','            Self::Platform(price) => price.provenance(),\n            Self::OfficialApi(price) => Some(&price.sheet.revision),',1)
s=s.replace('            Self::Platform(_) => Some(crate::provider::CUSTOM_PROVIDER_ID),','            Self::Platform(_) => Some(crate::provider::CUSTOM_PROVIDER_ID),\n            Self::OfficialApi(price) => Some(&price.provider_id),',1)
s=s.replace('    platform_price: Option<PlatformAttemptPrice>,','    platform_price: Option<PlatformAttemptPrice>,\n    official_price: Option<crate::official_api::OfficialAttemptPrice>,',1)
s=s.replace('            platform_price: None,','            platform_price: None,\n            official_price: None,')
needle='    attempt_context.set_client_key(client_key_id, state);';assert s.count(needle)==1
s=s.replace(needle,'    let pricing_snapshot = bind_official_attempt_price(state, account, plan, dynamics, &mut attempt_context, pricing_snapshot);\n'+needle)
s=s.replace('    apply_platform_native_attribution(&mut attribution, context, metrics);','''    apply_platform_native_attribution(&mut attribution, context, metrics);
    if let Some(price) = &context.official_price
        && matches!(metrics.cost_state, "priced" | "unknown")
        && metrics.pricing_provider_id.as_deref() == Some(price.provider_id.as_str())
        && metrics.pricing_revision_id.as_deref() == Some(price.sheet.revision.as_str())
        && let Some(amount) = price.amount(metrics.prompt_tokens, metrics.completion_tokens, metrics.cached_tokens, metrics.cache_creation_tokens) {
        attribution.native_cost_value = Some(amount);
        attribution.native_cost_unit = Some(price.sheet.kind.currency().into());
        attribution.native_cost_currency = Some(price.sheet.kind.currency().into());
    }''',1)
pos=s.index('\nfn apply_platform_native_attribution(')
s=s[:pos]+'''
fn bind_official_attempt_price(
    state: &CoreState, account: &Account, plan: &RequestPlan,
    dynamics: &[crate::dynamic::DynamicProviderRuntime], context: &mut ForwardAttemptContext,
    original: RequestPricingSnapshot,
) -> RequestPricingSnapshot {
    if !matches!(original, RequestPricingSnapshot::Unpriced)
        || platform_request_has_variable_cost(&plan.body, plan.service_tier.as_deref()) {
        return original;
    }
    let Ok(body) = serde_json::from_slice::<Value>(&plan.body) else { return original; };
    // Hosted tools have charges outside token pricing; ordinary function tools do not.
    if body.get("tools").and_then(Value::as_array).is_some_and(|tools| tools.iter().any(|tool| {
        tool.get("type").and_then(Value::as_str).is_some_and(|kind| !matches!(kind, "function" | "custom"))
    })) || body.get("web_search_options").is_some() { return original; }
    let Some(runtime) = dynamics.iter().find(|runtime| runtime.id == account.provider_id) else { return original; };
    let Some(kind) = crate::official_api::kind_for_runtime(runtime) else { return original; };
    let Some(endpoint) = plan.custom_route.as_ref().map(|route| route.endpoint_url.as_str()) else { return original; };
    let protocol = match plan.upstream {
        ApiFormat::ChatCompletions => crate::provider::UpstreamProtocolKind::ChatCompletions,
        ApiFormat::Responses => crate::provider::UpstreamProtocolKind::Responses,
        ApiFormat::Messages => crate::provider::UpstreamProtocolKind::Messages,
        ApiFormat::Gemini => return original,
    };
    if !crate::official_api::route_is_official(kind, endpoint, protocol) { return original; }
    let Ok(sheet) = state.db.lock().official_api_prices(&runtime.id, kind) else { return original; };
    let price = crate::official_api::OfficialAttemptPrice {
        provider_id: runtime.id.clone(), sheet, model: plan.model.clone(), at: state.sample_gateway_clock().0,
    };
    context.official_price = Some(price.clone());
    RequestPricingSnapshot::OfficialApi(price)
}
''' + s[pos:];p.write_text(s)
p=Path('crates/ocg-core/src/gateway/forwarder/tests.rs');s=p.read_text();s=s.replace('        platform_price: None,','        platform_price: None,\n        official_price: None,');p.write_text(s+'''
#[test]
fn official_api_attempt_pricing_is_native_frozen_and_never_attaches_to_foreign_routes() {
    use crate::official_api::{OfficialApiKind, pricing};
    let (dir,state)=test_state("official-api-cost");
    for (kind,model,amount,currency) in [(OfficialApiKind::Deepseek,"deepseek-flash",0.0015,"USD"),(OfficialApiKind::Zhipu,"glm-5.3",0.036,"CNY")] {
        let runtime=crate::official_api::tests::runtime(kind);
        let mut account=custom_account(&state);account.provider_id=runtime.id.clone();
        let body=Bytes::from(serde_json::to_vec(&json!({"model":model,"messages":[{"role":"user","content":"hello"}]})).unwrap());
        let parsed=crate::gateway::protocol::parse_client_request(ApiFormat::ChatCompletions,body).unwrap();
        let mut plan=crate::gateway::protocol::materialize_parsed_request(&parsed,&crate::gateway::protocol::MaterializeSpec {
            client_model:model.into(),upstream_model:model.into(),resolved_alias:None,channel:UpstreamChannel::Go,upstream_base_override:None,original_model:None,
            forced_upstream:Some(ApiFormat::ChatCompletions),custom_route:Some(CustomRouteSpec{endpoint_url:runtime.endpoint_url.clone()})
        }).unwrap();
        let now=chrono::DateTime::parse_from_rfc3339("2026-09-17T01:00:00Z").unwrap().with_timezone(&Utc);
        // Exercise the shared estimator and real log-finalization path at a
        // deterministic observation time, rather than relying on CI wall time.
        let frozen=crate::official_api::OfficialAttemptPrice{provider_id:runtime.id.clone(),sheet:pricing::seed(kind),model:model.into(),at:now};
        let price=RequestPricingSnapshot::OfficialApi(frozen.clone());
        let metrics=pricing_metrics(&price,model,1000,1000,0,0,None);
        assert_eq!(metrics.quota_debit,None);assert_eq!(metrics.effective_paid_cost_usd,None);
        if currency=="CNY" {assert_eq!(metrics.raw_cost_usd,None);}else{assert_eq!(metrics.raw_cost_usd,Some(amount));}
        let mut context=attempt_context(model);context.provider_id=Some(runtime.id.clone());context.official_price=Some(frozen.clone());
        let id=DbAttemptSink::new(&state.db.lock()).insert(&account,model,"success",Some(200),metrics.clone(),None,&context,None).unwrap();
        let native=state.db.lock().forward_log_native_attribution(id).unwrap().unwrap();assert!((native.native_cost_value.unwrap()-amount).abs()<1e-12);assert_eq!(native.native_cost_currency.as_deref(),Some(currency));
        // Finalizing a stream keeps the captured prices, not a later sheet.
        DbAttemptSink::new(&state.db.lock()).finalize(id,"success",Some(200),metrics,None,None,&context).unwrap();
        assert_eq!(state.db.lock().forward_log_native_attribution(id).unwrap().unwrap().native_cost_value,Some(amount));
        let mut positive=attempt_context(model);
        assert!(matches!(bind_official_attempt_price(&state,&account,&plan,std::slice::from_ref(&runtime),&mut positive,RequestPricingSnapshot::Unpriced),RequestPricingSnapshot::OfficialApi(_)));
        assert!(positive.official_price.is_some());
        for endpoint in ["https://attacker.test/chat/completions","http://127.0.0.1:9/chat/completions"] {
            plan.custom_route=Some(CustomRouteSpec{endpoint_url:endpoint.into()});
            let mut context=attempt_context(model);
            assert!(matches!(bind_official_attempt_price(&state,&account,&plan,std::slice::from_ref(&runtime),&mut context,RequestPricingSnapshot::Unpriced),RequestPricingSnapshot::Unpriced));
            assert!(context.official_price.is_none());
        }
        plan.custom_route=Some(CustomRouteSpec{endpoint_url:runtime.endpoint_url.clone()});
        plan.body=Bytes::from_static(br#"{"tools":[{"type":"web_search"}]}"#);
        let mut context=attempt_context(model);
        assert!(matches!(bind_official_attempt_price(&state,&account,&plan,std::slice::from_ref(&runtime),&mut context,RequestPricingSnapshot::Unpriced),RequestPricingSnapshot::Unpriced));
        context.official_price=Some(frozen.clone());
        let missing=metadata_metrics(&price,None,"usage_missing");
        let id=DbAttemptSink::new(&state.db.lock()).insert(&account,model,"success_no_usage",Some(200),missing,None,&context,None).unwrap();
        assert!(state.db.lock().forward_log_native_attribution(id).unwrap().unwrap().native_cost_value.is_none());
    }
    drop(state);let _=fs::remove_dir_all(dir);
}
''')
edit('crates/ocg-core/src/gateway/protocol.rs','''                    .pointer("/prompt_tokens_details/cached_tokens")
                    .and_then(Value::as_u64)''','''                    .pointer("/prompt_tokens_details/cached_tokens")
                    .or_else(|| usage.get("prompt_cache_hit_tokens"))
                    .and_then(Value::as_u64)''')
p=Path('crates/ocg-core/src/gateway/protocol/tests.rs');p.write_text(p.read_text()+'''
#[test]
fn official_api_deepseek_cache_counter_is_preserved() {
    let body=serde_json::json!({"usage":{"prompt_tokens":1000,"completion_tokens":50,"prompt_cache_hit_tokens":900,"prompt_cache_miss_tokens":100}});
    let counts=extract_usage(ApiFormat::ChatCompletions,&body,Some("deepseek-flash"));
    assert_eq!(counts.input_tokens,1000);assert_eq!(counts.cached_tokens,900);assert_eq!(counts.output_tokens,50);
}
''')
