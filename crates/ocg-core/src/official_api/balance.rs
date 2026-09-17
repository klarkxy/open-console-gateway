//! Selected-Key GET to a fixed first-party URL, bounded and without redirects.
use super::*;
use anyhow::{Result, anyhow, ensure};
use futures_util::StreamExt;
use serde_json::Value;
use std::collections::BTreeSet;

pub(crate) fn parse(body: &[u8], now: DateTime<Utc>) -> Result<Vec<OfficialBalance>> {
    let value: Value = serde_json::from_slice(body)?;
    ensure!(
        value.get("is_available").is_some_and(Value::is_boolean),
        "missing balance availability"
    );
    let entries = value
        .get("balance_infos")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("missing balance entries"))?;
    ensure!(
        !entries.is_empty() && entries.len() <= 2,
        "invalid balance count"
    );
    let mut seen = BTreeSet::new();
    entries
        .iter()
        .map(|entry| {
            let currency = entry
                .get("currency")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("missing balance currency"))?;
            ensure!(
                matches!(currency, "USD" | "CNY") && seen.insert(currency),
                "invalid or duplicate balance currency"
            );
            let amount = |field: &str| -> Result<f64> {
                let s = entry
                    .get(field)
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("missing balance amount"))?;
                ensure!(
                    s.len() <= 40
                        && !s.is_empty()
                        && s.bytes()
                            .enumerate()
                            .all(|(i, b)| b.is_ascii_digit() || b == b'.' || (i == 0 && b == b'-')),
                    "invalid balance amount"
                );
                let value: f64 = s.parse()?;
                ensure!(
                    value.is_finite() && value.abs() <= 1e15,
                    "invalid balance amount"
                );
                Ok(value)
            };
            Ok(OfficialBalance {
                currency: currency.into(),
                total: amount("total_balance")?,
                granted: amount("granted_balance")?,
                topped_up: amount("topped_up_balance")?,
                observed_at: now,
            })
        })
        .collect()
}

pub(crate) async fn fetch(
    config: &crate::models::AppConfig,
    key: &str,
    generation: u64,
    now: impl FnOnce() -> DateTime<Utc>,
) -> Result<Vec<OfficialBalance>> {
    let body = fetch_bytes(config, BALANCE_URL, Some(key), 64 * 1024, generation).await?;
    parse(&body, now()).map_err(|_| anyhow!("official balance response has an unsupported schema"))
}

pub(crate) async fn fetch_bytes(
    config: &crate::models::AppConfig,
    url: &str,
    key: Option<&str>,
    limit: usize,
    generation: u64,
) -> Result<Vec<u8>> {
    let endpoint = test_endpoint(generation, url);
    let client = crate::http_client::configured_builder(config)
        .map_err(|_| anyhow!("official API client unavailable"))?
        .redirect(crate::http_client::no_redirect_policy())
        .connect_timeout(std::time::Duration::from_secs(
            config.connect_timeout_secs.clamp(5, 30),
        ))
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|_| anyhow!("official API client unavailable"))?;
    let mut request = client.get(&endpoint).header(
        reqwest::header::ACCEPT,
        "application/json,text/plain,text/html",
    );
    if let Some(key) = key {
        request = request.bearer_auth(key);
    }
    let response = request
        .send()
        .await
        .map_err(|_| anyhow!("official API request failed"))?;
    ensure!(
        response.status() == reqwest::StatusCode::OK,
        "official API returned HTTP {}",
        response.status().as_u16()
    );
    ensure!(
        response.content_length().is_none_or(|n| n <= limit as u64),
        "official API response exceeds size limit"
    );
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| anyhow!("official API body read failed"))?;
        ensure!(
            body.len().saturating_add(chunk.len()) <= limit,
            "official API response exceeds size limit"
        );
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(debug_assertions)]
static TEST_URLS: std::sync::LazyLock<
    parking_lot::Mutex<std::collections::HashMap<(u64, String), String>>,
> = std::sync::LazyLock::new(Default::default);

fn test_endpoint(generation: u64, url: &str) -> String {
    #[cfg(debug_assertions)]
    if let Some(value) = TEST_URLS.lock().get(&(generation, url.into())).cloned() {
        return value;
    }
    let _ = generation;
    url.into()
}

#[cfg(debug_assertions)]
#[doc(hidden)]
pub struct OfficialApiTestGuard {
    generation: u64,
    source: String,
}
#[cfg(debug_assertions)]
impl Drop for OfficialApiTestGuard {
    fn drop(&mut self) {
        TEST_URLS
            .lock()
            .remove(&(self.generation, self.source.clone()));
    }
}

#[cfg(debug_assertions)]
#[doc(hidden)]
pub fn install_official_api_endpoint_for_test(
    generation: u64,
    source: &str,
    target: &str,
) -> Result<OfficialApiTestGuard> {
    ensure!(
        matches!(
            source,
            BALANCE_URL | DEEPSEEK_PRICING_URL | ZHIPU_PRICING_URL
        ),
        "unknown test source"
    );
    let url = reqwest::Url::parse(target)?;
    ensure!(
        url.scheme() == "http"
            && matches!(url.host_str(), Some("127.0.0.1" | "[::1]" | "::1"))
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "test endpoint must be literal loopback HTTP"
    );
    let mut urls = TEST_URLS.lock();
    ensure!(
        !urls.contains_key(&(generation, source.into())),
        "test override already installed"
    );
    urls.insert((generation, source.into()), target.into());
    Ok(OfficialApiTestGuard {
        generation,
        source: source.into(),
    })
}
