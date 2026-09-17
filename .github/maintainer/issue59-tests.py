from pathlib import Path
def write(path,text):
 p=Path(path);assert not p.exists(),path;p.parent.mkdir(parents=True,exist_ok=True);p.write_text(text)
write('crates/ocg-core/src/official_api/tests.rs',r'''use super::*;
use chrono::Duration;
use serde_json::json;

pub(crate) fn runtime(kind:OfficialApiKind)->DynamicProviderRuntime {
    DynamicProviderRuntime {preset_id:Some(kind.id().into()),id:"11111111-1111-1111-1111-111111111159".into(),name:"Official fixture".into(),
        endpoint_url:match kind {OfficialApiKind::Deepseek=>"https://api.deepseek.com/chat/completions",OfficialApiKind::Zhipu=>"https://open.bigmodel.cn/api/paas/v4/chat/completions"}.into(),
        upstream_protocol:UpstreamProtocolKind::ChatCompletions,auth_kind:DynamicAuthKind::Bearer,mappings:vec![],
        created_at:at(),updated_at:at(),origin:ocg_domain::provider::ProviderOrigin::Preset,offering:"api".into()}
}
fn at()->DateTime<Utc>{DateTime::parse_from_rfc3339("2026-09-17T01:00:00Z").unwrap().with_timezone(&Utc)}
const DEEPSEEK:&str=include_str!("../../tests/fixtures/official-api/deepseek-pricing.html");
const ZHIPU:&str=include_str!("../../tests/fixtures/official-api/zhipu-pricing.md");

#[test]
fn official_api_financial_capability_requires_provenance_and_fixed_destination(){
    for kind in [OfficialApiKind::Deepseek,OfficialApiKind::Zhipu]{
        let r=runtime(kind); assert_eq!(kind_for_runtime(&r),Some(kind));
        let mut edited=r.clone();edited.endpoint_url="https://api.deepseek.com.attacker.test/chat/completions".into();assert_eq!(kind_for_runtime(&edited),None);
        edited=r.clone();edited.preset_id=None;assert_eq!(kind_for_runtime(&edited),None);
        edited=r.clone();edited.auth_kind=DynamicAuthKind::None;assert_eq!(kind_for_runtime(&edited),None);
        edited=r.clone();edited.offering="plan".into();assert_eq!(kind_for_runtime(&edited),None);
        edited=r.clone();edited.endpoint_url.push_str("?secret=x");assert_eq!(kind_for_runtime(&edited),None);
    }
    assert!(!route_is_official(OfficialApiKind::Zhipu,"https://open.bigmodel.cn/api/coding/paas/v4/chat/completions",UpstreamProtocolKind::ChatCompletions));
    assert!(!route_is_official(OfficialApiKind::Deepseek,"http://api.deepseek.com/chat/completions",UpstreamProtocolKind::ChatCompletions));
}
#[test]
fn official_api_parsers_preserve_units_and_reject_partial_or_ambiguous_prices(){
    let ds=pricing::parse(OfficialApiKind::Deepseek,DEEPSEEK,at()).unwrap();assert_eq!(ds.rows.len(),8);
    let flash=ds.rows.iter().find(|r|r.model=="deepseek-flash"&&r.period=="peak").unwrap();assert_eq!(flash.input_per_million,0.3);assert_eq!(flash.currency,"USD");
    for doc in ["", "<table></table>", &DEEPSEEK.replace("$1.32","missing"), &DEEPSEEK.replace("Monday through Friday","Every day"), &format!("{DEEPSEEK}{DEEPSEEK}")] {
        assert!(pricing::parse(OfficialApiKind::Deepseek,doc,at()).is_err());
    }
    let glm=pricing::parse(OfficialApiKind::Zhipu,ZHIPU,at()).unwrap();assert_eq!(glm.rows.len(),3);
    assert_eq!(glm.rows[0].currency,"CNY");assert_eq!(glm.rows[0].output_per_million,28.0);
    assert!(!glm.rows.iter().any(|r|r.model=="glm-5.1"||r.model=="glm-4v"));
    assert_eq!(glm.rows[2].input_per_million,0.0);
    for doc in ["", &ZHIPU.replace("元/百万 Tokens","美元/千 Tokens"), &ZHIPU.replace("| 8 | 28 |","| ? | 28 |"), &ZHIPU.replace("### 视觉理解", "| GLM-5.3 | 1M | 8 | 28 | 限时免费 | 2 |\n### 视觉理解")] {
        assert!(pricing::parse(OfficialApiKind::Zhipu,doc,at()).is_err());
    }
    let mut corrupt=ds.clone();corrupt.rows[0].input_per_million=999.0;assert!(pricing::validate(&corrupt).is_err());
}
#[test]
fn official_api_money_estimates_are_exactly_scoped_cached_and_time_bounded(){
    let mut price=OfficialAttemptPrice{provider_id:"fixture".into(),sheet:pricing::seed(OfficialApiKind::Deepseek),model:"deepseek-flash".into(),at:at()};
    assert!((price.amount(1_000_000,100_000,100_000,0).unwrap()-0.3906).abs()<1e-12);
    price.at+=Duration::hours(3);assert!((price.amount(1_000_000,100_000,100_000,0).unwrap()-0.1953).abs()<1e-12);
    price.at=DateTime::parse_from_rfc3339("2026-09-19T01:00:00Z").unwrap().with_timezone(&Utc);assert!(!peak_at(price.at));
    assert!(price.amount(100,100,101,0).is_none());assert!(price.amount(100,100,0,1).is_none());assert!(price.amount(-1,100,0,0).is_none());
    price.model="unknown".into();assert!(price.amount(100,100,0,0).is_none());
    price.model="deepseek-flash".into();price.at=price.sheet.valid_until;assert!(price.amount(100,100,0,0).is_none());
    let glm=OfficialAttemptPrice{provider_id:"other".into(),sheet:pricing::seed(OfficialApiKind::Zhipu),model:"glm-5.3".into(),at:at()};assert_eq!(glm.amount(1_000,1_000,0,0),Some(0.036));
}
#[test]
fn official_api_balances_keep_total_and_components_separate_and_missing_unknown(){
    let body=json!({"is_available":true,"balance_infos":[{"currency":"CNY","total_balance":"12.5","granted_balance":"2.5","topped_up_balance":"10"},{"currency":"USD","total_balance":"-0.1","granted_balance":"0","topped_up_balance":"-0.1"}]});
    let rows=balance::parse(&serde_json::to_vec(&body).unwrap(),at()).unwrap();assert_eq!(rows.len(),2);assert_eq!(rows[0].total,12.5);assert_eq!(rows[1].total,-0.1);
    for invalid in [json!({}),json!({"is_available":false,"balance_infos":[]}),json!({"is_available":true,"balance_infos":[{"currency":"CNY","total_balance":"NaN","granted_balance":"0","topped_up_balance":"1"}]}),json!({"is_available":true,"balance_infos":[body["balance_infos"][0].clone(),body["balance_infos"][0].clone()]})] {
        assert!(balance::parse(&serde_json::to_vec(&invalid).unwrap(),at()).is_err());
    }
}

#[tokio::test]
async fn official_api_http_never_follows_redirects_and_bounds_headerless_bodies(){
    use axum::{Router,body::Body,response::IntoResponse,routing::get};
    use std::sync::{Arc,atomic::{AtomicUsize,Ordering}};
    let leaks=Arc::new(AtomicUsize::new(0));let leaked=leaks.clone();
    let router=Router::new()
        .route("/redirect",get(||async{(axum::http::StatusCode::FOUND,[("Location","/leak")],"redirect")}))
        .route("/leak",get(move||{let leaks=leaked.clone();async move{leaks.fetch_add(1,Ordering::SeqCst);"leak"}}))
        .route("/oversize",get(||async{Body::from(vec![b'x';128*1024])}))
        .route("/chunked",get(||async{Body::from_stream(futures_util::stream::iter((0..3).map(|_|Ok::<_,std::convert::Infallible>(bytes::Bytes::from(vec![b'x';32*1024])))))}))
        .route("/rejected",get(||async{(axum::http::StatusCode::UNAUTHORIZED,"sk-sensitive-fixture").into_response()}));
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let addr=listener.local_addr().unwrap();
    let server=tokio::spawn(async move{axum::serve(listener,router).await.unwrap()});
    let config=crate::models::AppConfig{proxy_mode:crate::models::ProxyMode::Direct,..Default::default()};
    for path in ["redirect","oversize","chunked","rejected"] {
        let _guard=install_official_api_endpoint_for_test(591,BALANCE_URL,&format!("http://{addr}/{path}")).unwrap();
        let error=balance::fetch(&config,"sk-sensitive-fixture",591,at).await.unwrap_err().to_string();
        assert!(!error.contains("sk-sensitive-fixture"));assert_eq!(leaks.load(Ordering::SeqCst),0);
    }
    server.abort();
}

#[tokio::test]
#[ignore="explicit public documentation compatibility check; no live Key or inference"]
async fn live_official_api_public_price_documents_parse(){
    let config=crate::models::AppConfig{proxy_mode:crate::models::ProxyMode::Direct,..Default::default()};
    for kind in [OfficialApiKind::Deepseek,OfficialApiKind::Zhipu] {
        let bytes=balance::fetch_bytes(&config,kind.pricing_url(),None,2*1024*1024,0).await.unwrap();
        let sheet=pricing::parse(kind,std::str::from_utf8(&bytes).unwrap(),Utc::now()).unwrap();
        assert!(!sheet.rows.is_empty());assert_eq!(sheet.source_url,kind.pricing_url());
    }
}
''')
write('crates/ocg-core/tests/fixtures/official-api/deepseek-pricing.html','''<!-- Synthetic structural fixture. Facts checked 2026-09-17 against api-docs.deepseek.com/quick_start/pricing/ -->
<p>UTC 01:00 - 04:00 and 06:00 - 10:00, Monday through Friday.</p>
<table><tr><th>MODEL</th><th>deepseek-flash (1)</th><th>deepseek-v4-pro (2)</th></tr>
<tr><td>PRICING</td><td>1M INPUT TOKENS (CACHE HIT)</td><td>OFF-PEAK</td><td>$0.003</td><td>$0.022</td></tr>
<tr><td>PEAK</td><td>$0.006</td><td>$0.044</td></tr>
<tr><td>1M INPUT TOKENS (CACHE MISS)</td><td>OFF-PEAK</td><td>$0.15</td><td>$0.66</td></tr>
<tr><td>PEAK</td><td>$0.3</td><td>$1.32</td></tr>
<tr><td>1M OUTPUT TOKENS</td><td>OFF-PEAK</td><td>$0.6</td><td>$1.98</td></tr>
<tr><td>PEAK</td><td>$1.2</td><td>$3.96</td></tr></table>
<p>deepseek-v4-flash and deepseek-v4-flash-vision-exp are billed at the Flash price.</p>
''')
write('crates/ocg-core/tests/fixtures/official-api/zhipu-pricing.md','''<!-- Synthetic fixture using factual rates from docs.bigmodel.cn/cn/guide/start/pricing.md on 2026-09-17. -->
# 模型价格，元/百万 Tokens
| 模型名称 | 上下文 | 输入单价 | 输出单价 | 缓存存储 | 缓存命中 |
| --- | --- | --- | --- | --- | --- |
| GLM-5.3 | 1M | 8 | 28 | 限时免费 | 2 |
| GLM-5.3-Flash | 1M | 0.8 | 2.8 | 限时免费 | 0.23 |
| GLM-4.7-Flash | 200K | 免费 | 免费 | 免费 | 免费 |
| GLM-5.1 | 输入大于32K | 8 | 28 | 限时免费 | 2 |
| GLM-5.1 | 输入不超过32K | 6 | 24 | 限时免费 | 1.3 |
### 视觉理解
| GLM-4V | 8K | 50 | 50 | 免费 | 不支持 |
''')
write('crates/ocg-core/tests/dashboard_v4_official_api.rs',r'''//! Public financial endpoints: fixed source, selected Key, CAS and old evidence retention.
use chrono::{DateTime,Duration,Utc};
use ocg_core::official_api::{BALANCE_URL,DEEPSEEK_PRICING_URL,ZHIPU_PRICING_URL,install_official_api_endpoint_for_test};
use reqwest::{Method,StatusCode};
use serde_json::{Value,json};
use std::collections::{HashMap,VecDeque};
#[path="fixtures/dashboard_v3/harness.rs"] mod harness;
#[allow(dead_code)]
#[path="fixtures/fake_upstream.rs"] mod fake_upstream;
use harness::{V3Harness,start_loopback,start_public};
use fake_upstream::{FakeReply,start_fake_upstream,start_fake_upstream_with_delay};
const KEY:&str="sk-official-test-only";
const BALANCE:&str=r#"{"is_available":true,"balance_infos":[{"currency":"CNY","total_balance":"12.5","granted_balance":"2.5","topped_up_balance":"10"},{"currency":"USD","total_balance":"2","granted_balance":"0","topped_up_balance":"2"}]}"#;
const DS_PRICES:&str=include_str!("fixtures/official-api/deepseek-pricing.html");
const GLM_PRICES:&str=include_str!("fixtures/official-api/zhipu-pricing.md");
fn now()->DateTime<Utc>{DateTime::parse_from_rfc3339("2026-09-17T01:00:00Z").unwrap().with_timezone(&Utc)}
fn cas(h:&V3Harness)->Value{json!({"expectedRevision":h.state.settings_revision(),"processGeneration":h.state.process_generation()})}
async fn send(h:&V3Harness,method:Method,path:&str,body:Value)->(StatusCode,Value){
 let r=h.client.request(method,format!("http://127.0.0.1:{}/dashboard/api/v4{path}",h.handle.port)).json(&body).send().await.unwrap();
 let status=r.status();let body=r.json::<Value>().await.unwrap();(status,body)
}
async fn create(h:&V3Harness,kind:&str)->(String,String){
 let (endpoint,model)=if kind=="deepseek"{("https://api.deepseek.com/chat/completions","deepseek-flash")}else{("https://open.bigmodel.cn/api/paas/v4/chat/completions","glm-5.3")};
 let mut body=cas(h);body.as_object_mut().unwrap().extend(json!({"presetId":kind,"name":"Official API fixture","endpointUrl":endpoint,"upstreamProtocol":"chat_completions","authKind":"bearer","key":KEY,"models":[{"publicModel":"official-model","upstreamModel":model}]}).as_object().unwrap().clone());
 let r=h.client.post(format!("{}/providers",h.v3_base)).json(&body).send().await.unwrap();let status=r.status();let body=r.json::<Value>().await.unwrap();assert_eq!(status,StatusCode::OK,"{body}");
 let provider=body["provider"]["id"].as_str().unwrap().to_string();
 let account=h.state.db.lock().list_accounts().unwrap().into_iter().find(|a|a.provider_id==provider).unwrap().id;
 (provider,account)
}
fn clock(h:&V3Harness,at:DateTime<Utc>){h.state.usage_sync.set_clock_for_test(move||at);}
fn assert_safe(body:&Value){let text=body.to_string();assert!(!text.contains(KEY));assert!(!text.contains("keyCipher"));assert!(!text.contains("key_cipher"));assert!(!text.contains("Authorization"));}

#[tokio::test]
async fn official_api_balance_refresh_is_selected_key_only_and_retains_evidence_on_failure(){
 let h=start_loopback("official-balance").await;clock(&h,now());let (provider,id)=create(&h,"deepseek").await;
 let (base,calls,_stop)=start_fake_upstream(HashMap::from([(KEY.into(),VecDeque::from([FakeReply{status:200,body:BALANCE},FakeReply{status:500,body:KEY}]))])).await;
 let _guard=install_official_api_endpoint_for_test(h.state.process_generation(),BALANCE_URL,&format!("{base}/user/balance")).unwrap();
 let before=h.state.db.lock().get_account(&id).unwrap().unwrap();
 let path=format!("/accounts/{id}/official-api");
 let (status,body)=send(&h,Method::GET,&path,json!({})).await;assert_eq!(status,StatusCode::OK,"{body}");assert_eq!(body["balances"],json!([]));assert_eq!(body["balanceAvailable"],true);assert!(calls.lock().unwrap().is_empty());
 let (status,body)=send(&h,Method::POST,&format!("{path}/balance"),cas(&h)).await;assert_eq!(status,StatusCode::OK,"{body}");assert_safe(&body);assert_eq!(body["providerId"],provider);
 assert_eq!(body["balances"][0]["total"],12.5);assert_eq!(body["balances"][0]["granted"],2.5);assert_eq!(body["balances"][1]["currency"],"USD");
 assert_eq!(calls.lock().unwrap().len(),1);let call=calls.lock().unwrap()[0].clone();assert_eq!(call.method,Method::GET);assert_eq!(call.path,"/user/balance");assert_eq!(call.authorization.as_deref(),Some(&*format!("Bearer {KEY}")));assert!(call.cookie.is_none());assert!(call.body.is_empty());
 let (status,_)=send(&h,Method::POST,&format!("{path}/balance"),cas(&h)).await;assert_eq!(status,StatusCode::TOO_MANY_REQUESTS);assert_eq!(calls.lock().unwrap().len(),1);
 clock(&h,now()+Duration::seconds(16));
 let (status,error)=send(&h,Method::POST,&format!("{path}/balance"),cas(&h)).await;assert_eq!(status,StatusCode::BAD_GATEWAY);assert_safe(&error);
 let (_,after)=send(&h,Method::GET,&path,json!({})).await;assert_eq!(after["balances"],body["balances"]);
 let account=h.state.db.lock().get_account(&id).unwrap().unwrap();assert_eq!(account.cooldown_until,before.cooldown_until);assert_eq!(account.auth_error,before.auth_error);assert_eq!(account.enabled,before.enabled);
 h.state.db.lock().update_account(&id,&Default::default(),Some(&h.state.encrypt_key("rotated").unwrap()),None).unwrap();
 let (_,after)=send(&h,Method::GET,&path,json!({})).await;assert_eq!(after["balances"],json!([]),"old-Key balance must not be presented for a new Key");
 h.stop();
}

#[tokio::test]
async fn official_api_price_refresh_is_keyless_scoped_and_failure_keeps_previous_snapshot(){
 for (kind,source,fixture,currency) in [("deepseek",DEEPSEEK_PRICING_URL,DS_PRICES,"USD"),("zhipu",ZHIPU_PRICING_URL,GLM_PRICES,"CNY")]{
  let h=start_loopback("official-prices").await;clock(&h,now());let (provider,id)=create(&h,kind).await;
  let (base,calls,_stop)=start_fake_upstream(HashMap::from([("".into(),VecDeque::from([FakeReply{status:200,body:fixture},FakeReply{status:200,body:"schema changed"}]))])).await;
  let _guard=install_official_api_endpoint_for_test(h.state.process_generation(),source,&format!("{base}/pricing")).unwrap();
  let path=format!("/providers/{provider}/official-api/pricing");let (_,seed)=send(&h,Method::GET,&path,json!({})).await;assert_eq!(seed["prices"]["rows"][0]["currency"],currency);assert!(calls.lock().unwrap().is_empty());
  let (status,fresh)=send(&h,Method::POST,&path,cas(&h)).await;assert_eq!(status,StatusCode::OK,"{fresh}");assert_safe(&fresh);assert_eq!(fresh["prices"]["sourceUrl"],source);assert_ne!(fresh["prices"]["revision"],seed["prices"]["revision"]);
  assert!(calls.lock().unwrap()[0].authorization.is_none());assert!(calls.lock().unwrap()[0].cookie.is_none());
  let (status,failed)=send(&h,Method::POST,&path,cas(&h)).await;assert_eq!(status,StatusCode::BAD_GATEWAY,"{failed}");
  let (_,persisted)=send(&h,Method::GET,&path,json!({})).await;assert_eq!(persisted["prices"],fresh["prices"]);
  if kind=="zhipu" {let (status,body)=send(&h,Method::POST,&format!("/accounts/{id}/official-api/balance"),cas(&h)).await;assert_eq!(status,StatusCode::BAD_REQUEST,"{body}");assert_eq!(calls.lock().unwrap().len(),2);}
  h.stop();
 }
}

#[tokio::test]
async fn official_api_stale_cas_and_revoked_destination_send_nothing(){
 let h=start_loopback("official-cas").await;let (_,id)=create(&h,"deepseek").await;
 let (base,calls,_stop)=start_fake_upstream(HashMap::from([(KEY.into(),VecDeque::from([FakeReply{status:200,body:BALANCE}]))])).await;
 let _guard=install_official_api_endpoint_for_test(h.state.process_generation(),BALANCE_URL,&format!("{base}/balance")).unwrap();
 let path=format!("/accounts/{id}/official-api/balance");
 for invalid in [json!({}),json!({"expectedRevision":0,"processGeneration":0})]{let (status,_)=send(&h,Method::POST,&path,invalid).await;assert!(status.is_client_error());}
 let binding=h.state.db.lock().list_inference_bindings().unwrap().into_iter().find(|b|b.account_id==id).unwrap();
 let (status,body)=send(&h,Method::PATCH,&format!("/bindings/{}",binding.binding_id),{
  let mut v=cas(&h);v["allowedEndpointIds"]=json!([]);v["allowedOrigins"]=json!([]);v
 }).await;assert_eq!(status,StatusCode::OK,"{body}");
 let (status,body)=send(&h,Method::POST,&path,cas(&h)).await;assert_eq!(status,StatusCode::BAD_REQUEST,"{body}");assert!(calls.lock().unwrap().is_empty());h.stop();
}

#[tokio::test]
async fn official_api_changed_key_during_io_rejects_late_balance(){
 let h=start_loopback("official-race").await;let (_,id)=create(&h,"deepseek").await;
 let (base,calls,_stop)=start_fake_upstream_with_delay(HashMap::from([(KEY.into(),VecDeque::from([FakeReply{status:200,body:BALANCE}]))]),std::time::Duration::from_millis(150)).await;
 let _guard=install_official_api_endpoint_for_test(h.state.process_generation(),BALANCE_URL,&format!("{base}/balance")).unwrap();
 let url=format!("http://127.0.0.1:{}/dashboard/api/v4/accounts/{id}/official-api/balance",h.handle.port);let client=h.client.clone();let expectation=cas(&h);
 let task=tokio::spawn(async move{client.post(url).json(&expectation).send().await.unwrap()});
 tokio::time::timeout(std::time::Duration::from_secs(5),async{while calls.lock().unwrap().is_empty(){tokio::time::sleep(std::time::Duration::from_millis(2)).await;}}).await.unwrap();
 let encrypted=h.state.encrypt_key("new-key").unwrap();h.state.db.lock().update_account(&id,&Default::default(),Some(&encrypted),None).unwrap();h.state.bump_settings_revision();
 assert_eq!(task.await.unwrap().status(),StatusCode::CONFLICT);assert!(h.state.db.lock().list_credit_balances(&id).unwrap().is_empty());h.stop();
}

#[tokio::test]
async fn official_api_financial_routes_require_dashboard_session(){
 let h=start_public("official-auth").await;
 for (method,path) in [(Method::GET,"/accounts/id/official-api"),(Method::POST,"/accounts/id/official-api/balance"),(Method::GET,"/providers/id/official-api/pricing"),(Method::POST,"/providers/id/official-api/pricing")]{
  let (status,body)=send(&h,method,path,cas(&h)).await;assert_eq!(status,StatusCode::UNAUTHORIZED,"{body}");assert_safe(&body);
 }
 h.stop();
}
''')
