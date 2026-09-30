//! Attempt HTTP send, body read limits, and upstream URL checks.

use super::*;

#[cfg(test)]
mod tests;

pub(super) const MAX_UPSTREAM_ERROR_BODY_BYTES: usize = 64 * 1024;

pub(crate) struct ForwardOnceOutput {
    pub(super) started: Instant,
    pub(super) result: std::result::Result<reqwest::Response, AttemptTransportError>,
}

/// Prepare transport and request before the final live credential check.
#[allow(clippy::too_many_arguments)]
pub(super) fn build_attempt_request(
    spec: &AttemptSpec,
    snapshot_client: &Client,
    route: RouteLabel,
    config: &AppConfig,
    timeouts: AttemptTimeouts,
    url: &str,
    headers: reqwest::header::HeaderMap,
    body: bytes::Bytes,
    stream: bool,
) -> Result<reqwest::RequestBuilder> {
    let secret_bearing = headers_carry_upstream_secret(&headers);
    let mut request = match spec.proxy_routing {
        ProxyRoutingModel::IsolatedTrustedAdmin => {
            let client = build_custom_http_client_for_route(config, route)?;
            let url = reqwest::Url::parse(url)?;
            crate::custom_http::inspect_custom_url(&url)
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            client.request(reqwest::Method::POST, url)
        }
        ProxyRoutingModel::ProcessWideNoRedirect => {
            let client = crate::http_client::build_no_redirect_for_route(config, route)?;
            client.post(url)
        }
        ProxyRoutingModel::LocalExternalIntegration => {
            let client = crate::http_client::build_no_redirect_for_route(
                config,
                crate::http_client::RouteLabel::Direct,
            )?;
            client.post(url)
        }
        ProxyRoutingModel::RequestEntrySnapshot
            if crate::custom_http::follows_redirects_with_secret(
                spec.follow_redirects,
                secret_bearing,
            ) =>
        {
            snapshot_client.post(url)
        }
        ProxyRoutingModel::RequestEntrySnapshot => {
            let client = crate::http_client::build_no_redirect_for_route(config, route)?;
            client.post(url)
        }
    };
    request = request.headers(headers).body(body);
    if !stream {
        request = request.timeout(timeouts.non_stream);
    }
    Ok(request)
}

pub(super) async fn forward_once(
    request: reqwest::RequestBuilder,
    timeouts: AttemptTimeouts,
    stream: bool,
) -> Result<ForwardOnceOutput> {
    let started = Instant::now();
    let send_future = request.send();
    let result = if stream {
        match tokio::time::timeout(timeouts.stream_header, send_future).await {
            Ok(result) => result.map_err(map_attempt_send_error),
            Err(_) => Err(AttemptTransportError::HeaderTimeout {
                timeout: timeouts.stream_header,
            }),
        }
    } else {
        send_future.await.map_err(map_attempt_send_error)
    };
    Ok(ForwardOnceOutput { started, result })
}

fn map_attempt_send_error(error: reqwest::Error) -> AttemptTransportError {
    AttemptTransportError::Send(TransportSendFailure::from_send_error(
        error.is_connect(),
        error.is_timeout(),
        error.to_string(),
    ))
}

pub(super) fn join_chunks(chunks: Vec<bytes::Bytes>) -> bytes::Bytes {
    let capacity = chunks.iter().map(bytes::Bytes::len).sum();
    let mut joined = BytesMut::with_capacity(capacity);
    for chunk in chunks {
        joined.extend_from_slice(&chunk);
    }
    joined.freeze()
}

pub(crate) fn headers_carry_upstream_secret(headers: &reqwest::header::HeaderMap) -> bool {
    headers.keys().any(|name| {
        matches!(
            name.as_str(),
            "authorization" | "x-api-key" | "api-key" | "x-goog-api-key"
        )
    })
}

pub(super) fn ensure_safe_upstream_base_url(base: &str) -> Result<()> {
    let url = reqwest::Url::parse(base)?;
    match url.scheme() {
        "https" => Ok(()),
        "http" if is_loopback_host(&url) => Ok(()),
        scheme => anyhow::bail!("unsafe upstream scheme or host: {scheme}"),
    }
}

fn is_loopback_host(url: &reqwest::Url) -> bool {
    matches!(
        url.host_str(),
        Some("localhost") | Some("127.0.0.1") | Some("::1") | Some("[::1]")
    )
}

pub(super) fn sanitize_upstream_error(text: &str, known_secret: &str) -> String {
    let safe = sanitize_upstream_error_value_with_known_secret(text, known_secret);
    safe.get("text")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| safe.to_string())
        .chars()
        .take(500)
        .collect()
}

fn response_body_error(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        "upstream response body timed out".to_string()
    } else {
        format!("upstream response body failed: {error}")
    }
}

#[derive(Debug)]
pub(super) enum ResponseBodyFailure {
    IdleTimeout(StdDuration),
    Transport(reqwest::Error),
}

impl ResponseBodyFailure {
    pub(super) fn is_timeout(&self) -> bool {
        match self {
            Self::IdleTimeout(_) => true,
            Self::Transport(error) => error.is_timeout(),
        }
    }

    pub(super) fn into_detail(self) -> String {
        match self {
            Self::IdleTimeout(timeout) => format!(
                "upstream response body timed out after {}s",
                timeout.as_secs()
            ),
            Self::Transport(error) => response_body_error(&error),
        }
    }
}

pub(super) async fn response_text_with_timeout(
    response: reqwest::Response,
    timeout: Option<StdDuration>,
    max_bytes: Option<usize>,
) -> std::result::Result<String, ResponseBodyFailure> {
    let read = response_text(response, max_bytes);
    match timeout {
        Some(timeout) => match tokio::time::timeout(timeout, read).await {
            Ok(result) => result.map_err(ResponseBodyFailure::Transport),
            Err(_) => Err(ResponseBodyFailure::IdleTimeout(timeout)),
        },
        None => read.await.map_err(ResponseBodyFailure::Transport),
    }
}

async fn response_text(
    response: reqwest::Response,
    max_bytes: Option<usize>,
) -> std::result::Result<String, reqwest::Error> {
    let Some(max_bytes) = max_bytes else {
        return response.text().await;
    };

    let read_limit = max_bytes.saturating_add(1);
    let capacity = response
        .content_length()
        .and_then(|length| usize::try_from(length).ok())
        .map_or(read_limit, |length| length.min(read_limit));
    let mut body = Vec::with_capacity(capacity);
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        let remaining = read_limit.saturating_sub(body.len());
        if remaining == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
        if body.len() == read_limit {
            break;
        }
    }

    let truncated = body.len() > max_bytes;
    body.truncate(max_bytes);
    let mut text = String::from_utf8_lossy(&body).into_owned();
    if truncated {
        text.push_str("\n<upstream error body truncated>");
    }
    Ok(text)
}
