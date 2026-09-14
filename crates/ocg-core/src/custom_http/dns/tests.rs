use super::*;
use crate::custom_http::{
    CustomHttpError, build_custom_http_client, build_custom_http_client_with_dns_resolver,
};
use crate::models::{AppConfig, ProxyMode};
use crate::provider::UpstreamAuthScheme;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::header::HeaderMap;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const METADATA_V4: Ipv4Addr = Ipv4Addr::new(169, 254, 169, 254);
const AWS_IMDS_V6: Ipv6Addr = Ipv6Addr::new(0xfd00, 0xec2, 0, 0, 0, 0, 0, 0x254);

struct StaticResolver {
    answers: HashMap<String, Vec<SocketAddr>>,
}

impl StaticResolver {
    fn one(host: &str, addrs: Vec<SocketAddr>) -> Arc<dyn Resolve> {
        let mut answers = HashMap::new();
        answers.insert(host.to_ascii_lowercase(), addrs);
        Arc::new(Self { answers })
    }
}

impl Resolve for StaticResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().trim_end_matches('.').to_ascii_lowercase();
        let addrs = self
            .answers
            .get(&host)
            .cloned()
            .unwrap_or_else(|| panic!("injected inner resolver has no answers for {host}"));
        Box::pin(async move { Ok(Box::new(addrs.into_iter()) as Addrs) })
    }
}

fn addr(ip: IpAddr) -> SocketAddr {
    SocketAddr::new(ip, 0)
}

fn test_config(mode: ProxyMode, proxy_url: &str) -> AppConfig {
    AppConfig {
        proxy_mode: mode,
        proxy_url: proxy_url.to_string(),
        connect_timeout_secs: 5,
        ..AppConfig::default()
    }
}

fn guarded_recording(inner: Arc<dyn Resolve>) -> (Arc<dyn Resolve>, Arc<DestinationResolveLog>) {
    let log = Arc::new(DestinationResolveLog::default());
    let resolver = recording_resolver(guarded_destination_resolver(inner), Arc::clone(&log));
    (resolver, log)
}

async fn collect_resolve(
    resolver: &dyn Resolve,
    host: &str,
) -> Result<Vec<SocketAddr>, Box<dyn std::error::Error + Send + Sync>> {
    let name = Name::from_str(host).expect("synthetic DNS name");
    Ok(resolver.resolve(name).await?.collect())
}

async fn serve_http(hits: Arc<AtomicUsize>) -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback listener");
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            hits.fetch_add(1, Ordering::SeqCst);
            let mut buf = vec![0_u8; 4096];
            let _ = stream.read(&mut buf).await;
            let body = "ok";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
        }
    });
    addr
}

async fn serve_counting_proxy(hits: Arc<AtomicUsize>) -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("proxy listener");
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            hits.fetch_add(1, Ordering::SeqCst);
            let mut buf = vec![0_u8; 4096];
            let _ = stream.read(&mut buf).await;
            let body = "proxy";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
        }
    });
    addr
}

async fn send_isolated(
    client: &crate::custom_http::CustomHttpClient,
    url: reqwest::Url,
) -> Result<reqwest::Response, CustomHttpError> {
    client
        .send_isolated(
            reqwest::Method::GET,
            url,
            UpstreamAuthScheme::Bearer,
            "sk-test-dns-guard",
            HeaderMap::new(),
            None,
            None,
        )
        .await
}

fn assert_no_secret(error: &CustomHttpError) {
    let text = format!("{error:?} {error}");
    assert!(
        !text.contains("sk-test-dns-guard"),
        "error must not echo the synthetic key: {text}"
    );
}

#[test]
fn shared_ip_policy_blocks_metadata_link_local_unspecified_multicast_and_aws_v6() {
    assert!(is_blocked_custom_ip(IpAddr::V4(METADATA_V4)));
    assert!(is_blocked_custom_ip(IpAddr::V4(Ipv4Addr::UNSPECIFIED)));
    assert!(is_blocked_custom_ip(IpAddr::V4(Ipv4Addr::BROADCAST)));
    assert!(is_blocked_custom_ip(IpAddr::V4(Ipv4Addr::new(
        224, 0, 0, 1
    ))));
    assert!(is_blocked_custom_ip(IpAddr::V6(Ipv6Addr::UNSPECIFIED)));
    assert!(is_blocked_custom_ip(IpAddr::V6(Ipv6Addr::new(
        0xfe80, 0, 0, 0, 0, 0, 0, 1
    ))));
    assert!(is_blocked_custom_ip(IpAddr::V6(AWS_IMDS_V6)));
    assert!(is_blocked_custom_ip(IpAddr::V6(Ipv6Addr::new(
        0, 0, 0, 0, 0, 0xffff, 0xa9fe, 0xa9fe
    ))));
    assert!(!is_blocked_custom_ip(IpAddr::V4(Ipv4Addr::LOCALHOST)));
    assert!(!is_blocked_custom_ip(IpAddr::V4(Ipv4Addr::new(
        10, 0, 0, 1
    ))));
    assert!(!is_blocked_custom_ip(IpAddr::V4(Ipv4Addr::new(
        192, 168, 1, 8
    ))));
    assert!(!is_blocked_custom_ip(IpAddr::V6(Ipv6Addr::LOCALHOST)));
}

#[test]
fn filter_drops_blocked_answers_and_keeps_mixed_remainder() {
    let blocked_only = filter_resolved_destination_addrs([
        addr(IpAddr::V4(METADATA_V4)),
        addr(IpAddr::V6(AWS_IMDS_V6)),
    ]);
    let blocked_err = blocked_only.expect_err("blocked-only answers");
    assert!(
        blocked_err.to_string().contains(BLOCKED_RESOLUTION),
        "{blocked_err}"
    );

    let mixed = filter_resolved_destination_addrs([
        addr(IpAddr::V4(METADATA_V4)),
        addr(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        addr(IpAddr::V6(AWS_IMDS_V6)),
    ])
    .unwrap();
    assert_eq!(mixed, vec![addr(IpAddr::V4(Ipv4Addr::LOCALHOST))]);

    let lan =
        filter_resolved_destination_addrs([addr(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)))]).unwrap();
    assert_eq!(lan[0].ip(), IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)));
}

#[tokio::test]
async fn guarded_resolve_rejects_injected_blocked_answers_specifically() {
    let inner = StaticResolver::one(
        "imds.test",
        vec![addr(IpAddr::V4(METADATA_V4)), addr(IpAddr::V6(AWS_IMDS_V6))],
    );
    let unguarded = collect_resolve(inner.as_ref(), "imds.test")
        .await
        .expect("inner answers are injectable");
    assert_eq!(
        unguarded,
        vec![addr(IpAddr::V4(METADATA_V4)), addr(IpAddr::V6(AWS_IMDS_V6))]
    );

    let guarded = guarded_destination_resolver(inner);
    let error = collect_resolve(guarded.as_ref(), "imds.test")
        .await
        .expect_err("guard must reject blocked-only answers");
    assert!(
        error.to_string().contains(BLOCKED_RESOLUTION),
        "guard rejection must be specific, got {error}"
    );
}

#[tokio::test]
async fn unguarded_inner_does_not_look_like_a_guard_rejection() {
    let inner = StaticResolver::one("imds.test", vec![addr(IpAddr::V4(METADATA_V4))]);
    let log = Arc::new(DestinationResolveLog::default());
    let recorded = recording_resolver(Arc::clone(&inner), Arc::clone(&log));
    let _ = collect_resolve(recorded.as_ref(), "imds.test").await;
    assert!(
        !log.rejections()
            .iter()
            .any(|message| message.contains(BLOCKED_RESOLUTION)),
        "removing the guard must not still satisfy assert_guard_rejected: {:?}",
        log.rejections()
    );
}

#[tokio::test]
async fn injected_inner_metadata_is_rejected_by_guard_before_connect() {
    for (label, host, addrs) in [
        ("A", "imds.test", vec![addr(IpAddr::V4(METADATA_V4))]),
        (
            "AAAA-and-fe80",
            "imds6.test",
            vec![
                addr(IpAddr::V6(AWS_IMDS_V6)),
                addr(IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1))),
            ],
        ),
    ] {
        let hits = Arc::new(AtomicUsize::new(0));
        let mock = serve_http(hits.clone()).await;
        let inner = StaticResolver::one(host, addrs);
        let (resolver, log) = guarded_recording(inner);
        let client = build_custom_http_client_with_dns_resolver(
            &test_config(ProxyMode::Direct, ""),
            resolver,
        )
        .unwrap();
        let url = reqwest::Url::parse(&format!("http://{host}:{}/v1", mock.port())).unwrap();
        let error = match send_isolated(&client, url).await {
            Err(error) => error,
            Ok(_) => panic!("guarded metadata {label} must not connect"),
        };
        log.assert_guard_rejected(host);
        assert_no_secret(&error);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(hits.load(Ordering::SeqCst), 0, "{label} zero hits");
    }
}

#[tokio::test]
async fn mixed_answers_keep_loopback_and_hit_local_mock() {
    let hits = Arc::new(AtomicUsize::new(0));
    let mock = serve_http(hits.clone()).await;
    let inner = StaticResolver::one(
        "mixed.test",
        vec![
            addr(IpAddr::V4(METADATA_V4)),
            addr(IpAddr::V4(Ipv4Addr::LOCALHOST)),
            addr(IpAddr::V6(AWS_IMDS_V6)),
        ],
    );
    let (resolver, log) = guarded_recording(inner);
    let client =
        build_custom_http_client_with_dns_resolver(&test_config(ProxyMode::Direct, ""), resolver)
            .unwrap();
    let url = reqwest::Url::parse(&format!("http://mixed.test:{}/v1", mock.port())).unwrap();
    let response = send_isolated(&client, url).await.expect("mixed remainder");
    assert_eq!(response.status().as_u16(), 200);
    log.assert_queried("mixed.test");
    assert_eq!(
        log.returned(),
        vec![vec![addr(IpAddr::V4(Ipv4Addr::LOCALHOST))]]
    );
    assert!(log.rejections().is_empty(), "{:?}", log.rejections());
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn localhost_lan_name_and_public_name_controls_reach_injected_loopback() {
    let hits = Arc::new(AtomicUsize::new(0));
    let mock = serve_http(hits.clone()).await;
    let mut answers = HashMap::new();
    let loopback = vec![addr(IpAddr::V4(Ipv4Addr::LOCALHOST))];
    answers.insert("localhost".into(), loopback.clone());
    answers.insert("ollama.lan".into(), loopback.clone());
    answers.insert("lab.example".into(), loopback);
    let inner = Arc::new(StaticResolver { answers });
    let (resolver, log) = guarded_recording(inner);
    let client =
        build_custom_http_client_with_dns_resolver(&test_config(ProxyMode::Direct, ""), resolver)
            .unwrap();
    for host in ["localhost", "ollama.lan", "lab.example"] {
        let url = reqwest::Url::parse(&format!("http://{host}:{}/v1", mock.port())).unwrap();
        let response = send_isolated(&client, url)
            .await
            .unwrap_or_else(|error| panic!("{host} should remain allowed: {error}"));
        assert_eq!(response.status().as_u16(), 200, "{host}");
        log.assert_queried(host);
    }
    assert_eq!(hits.load(Ordering::SeqCst), 3);
    assert!(log.rejections().is_empty(), "{:?}", log.rejections());
}

#[tokio::test]
async fn explicit_local_http_proxy_still_owns_destination_dns() {
    let upstream_hits = Arc::new(AtomicUsize::new(0));
    let upstream = serve_http(upstream_hits.clone()).await;
    let proxy_hits = Arc::new(AtomicUsize::new(0));
    let proxy = serve_counting_proxy(proxy_hits.clone()).await;
    let inner = StaticResolver::one("imds.test", vec![addr(IpAddr::V4(METADATA_V4))]);
    let (resolver, log) = guarded_recording(inner);
    let client = build_custom_http_client_with_dns_resolver(
        &test_config(
            ProxyMode::Manual,
            &format!("http://127.0.0.1:{}", proxy.port()),
        ),
        resolver,
    )
    .unwrap();
    let url = reqwest::Url::parse(&format!("http://imds.test:{}/v1", upstream.port())).unwrap();
    let proxied = send_isolated(&client, url)
        .await
        .expect("explicit proxy keeps routing; dest DNS is the proxy's");
    assert_eq!(proxied.status().as_u16(), 200);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(proxy_hits.load(Ordering::SeqCst), 1);
    assert_eq!(
        upstream_hits.load(Ordering::SeqCst),
        0,
        "HTTP proxy answered; dest was not locally connected"
    );
    assert!(
        !log.names()
            .iter()
            .any(|name| name.eq_ignore_ascii_case("imds.test")),
        "local guard must not be described as resolving dest through the proxy: {:?}",
        log.names()
    );
}

#[tokio::test]
async fn production_direct_client_reaches_loopback_literal_and_system_localhost_dns() {
    let hits = Arc::new(AtomicUsize::new(0));
    let mock = serve_http(hits.clone()).await;
    let client = build_custom_http_client(&test_config(ProxyMode::Direct, "")).unwrap();
    for host in ["127.0.0.1", "localhost"] {
        let url = reqwest::Url::parse(&format!("http://{host}:{}/v1", mock.port())).unwrap();
        let response = send_isolated(&client, url).await.unwrap();
        assert_eq!(response.status().as_u16(), 200);
    }
    assert_eq!(hits.load(Ordering::SeqCst), 2);
}
