//! One-shot HTTP client for the live host.
//!
//! This module does not open SQLite. It sends a single request, optionally
//! filling absent `expectedRevision` and `processGeneration` from public
//! `GET /dashboard/api/v4/auth/status`, and does not retry or follow redirects.
//! It does not fill pricing tokens.

use crate::endpoint::{self, Endpoint};
use crate::session_file::{self, SessionStore};
use serde_json::{Value, json};
use std::io::{self, Read, Write};
use std::path::Path;
use std::time::Duration;

const MAX_INPUT_BYTES: usize = 64 * 1024 * 1024;
const MAX_NON_STREAM_OUTPUT_BYTES: usize = 32 * 1024 * 1024;
const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;
const MAX_KEY_FILE_BYTES: usize = 4 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const DASHBOARD_TIMEOUT: Duration = Duration::from_secs(180);
const INFERENCE_TIMEOUT: Duration = Duration::from_secs(600);

pub const API_HELP: &str = r#"Send one request to a running host. This command does not open the data directory or SQLite.

ENDPOINT
  --endpoint defaults to http://127.0.0.1:9042 and may be placed before the
  subcommand or after api. It is an origin (scheme, host, port) only.
  Credentials, fragments, queries, backslashes, and percent-encoding are
  rejected. The client does not use an HTTP proxy and does not follow
  redirects.

PATHS
  /dashboard/api/v4/...
  /dashboard/api/auth/status
  /dashboard/api/auth/register
  /dashboard/api/auth/login
  /dashboard/api/auth/logout
  POST /v1/chat/completions
  POST /v1/responses
  POST /v1/messages
  POST /v1/messages/count_tokens
  GET /v1/models
  POST /v1/models/{model}:{action}
  POST /v1beta/models/{model}:{action}

  /dashboard/api/v3 and every other /dashboard/api route are refused here.
  On the server, V3 REST is a 410 tombstone. The four unversioned auth paths
  above are the older handlers: register and login take username and password
  and ignore CAS fields, and logout takes no body. The same operations under
  /dashboard/api/v4/auth require CAS. Both families set ocg_dashboard_session.
  This client sends HTTP only. It does not upgrade the dashboard browser
  websocket at /dashboard/api/v4/browser/sessions/{token}/ws. The preserved
  V2 websocket path is refused.
  Portable node transfer is
  POST /dashboard/api/v4/accounts/transfer/export|preview|import with
  --input and --output. That package is encrypted account state, not a copy
  of the data directory. Stop the process before copying the data directory.

CAS
  Mutation bodies are JSON objects. expectedRevision and processGeneration
  are required by the host. --cas-current reads public
  GET /dashboard/api/v4/auth/status once and inserts only a missing
  expectedRevision or processGeneration. A field you already sent is not
  replaced, even when it is null or stale. The mutation is sent once.
  HTTP 409 revisionConflict means the revision or process generation changed,
  including after a restart. The CLI does not resubmit it.

  --cas-current does not fill expectedPricingRevision or
  expectedProviderPricingRevision. Those tokens belong to the selected
  provider. Supply them yourself.

  /dashboard/api/v4/auth/register, login, and logout require the same two CAS
  fields. Register does not bump settings_revision. Login and logout rotate
  the session cookie and do not bump settings_revision either. A 409 on those
  routes is not success. The unversioned /dashboard/api/auth paths do not
  read those fields.

CPA RUNTIME
  GET /dashboard/api/v4/external-integrations/cpa/runtime
  POST /dashboard/api/v4/external-integrations/cpa/runtime/check-update
  POST /dashboard/api/v4/external-integrations/cpa/runtime/install
  POST /dashboard/api/v4/external-integrations/cpa/runtime/update
  POST /dashboard/api/v4/external-integrations/cpa/runtime/start
  POST /dashboard/api/v4/external-integrations/cpa/runtime/stop
  POST /dashboard/api/v4/external-integrations/cpa/runtime/rollback
  GET /dashboard/api/v4/external-integrations/cpa/runtime/logs
  These are the existing V4 routes. install and start use the pinned host
  selected by serve --cpa-host-dir. A saved product change is not applied
  until start. check-update reports the pinned v8.0.10 artifact.

SECRETS
  Put JSON in --input file or --input -. The file contents are not echoed.
  --key-file is sent as Authorization: Bearer on inference routes only and
  is never printed. --session-file stores Set-Cookie values, including a
  Max-Age=0 deletion, and is bound to the exact endpoint. The cookie is
  never printed. Dashboard cookies are not attached to /v1 routes.

EXAMPLES
  ocg api GET /dashboard/api/v4/contract
  ocg api --session-file session.json --cas-current POST /dashboard/api/v4/auth/login --input login.json
  ocg api --cas-current PATCH /dashboard/api/v4/accounts/ACCOUNT_ID --input patch.json --output result.json
  ocg api --key-file gateway.key POST /v1/chat/completions --input request.json

Success writes the response body to stdout, or to --output and nothing else.
Stdout JSON redacts primaryKey, subKeys[].value, one-time secret, and known
secret fields. --output is the private raw body. Inference responses and
event streams are copied as bytes. Errors print a JSON object on stderr and
exit with a small non-zero code. status and code are the host values when
the host returned JSON. A transport failure has no HTTP status. A private
output failure after HTTP success names that acceptance and does not retry.
Stdout is empty on failure."#;

const EXIT_TRANSPORT: i32 = 1;
const EXIT_USAGE: i32 = 2;
const EXIT_HTTP: i32 = 3;
const EXIT_IO: i32 = 4;
const EXIT_ACCEPTED_NOT_SAVED: i32 = 5;

#[derive(Debug)]
pub struct ApiFailure {
    pub exit_code: i32,
    pub status: Option<u16>,
    pub code: String,
    pub message: String,
    pub current_revision: Option<u64>,
    pub process_generation: Option<u64>,
}

impl ApiFailure {
    pub fn stderr_line(&self) -> String {
        let mut object = serde_json::Map::new();
        if let Some(status) = self.status {
            object.insert("status".into(), json!(status));
        }
        object.insert("code".into(), json!(self.code));
        object.insert("message".into(), json!(self.message));
        if let Some(revision) = self.current_revision {
            object.insert("currentRevision".into(), json!(revision));
        }
        if let Some(generation) = self.process_generation {
            object.insert("processGeneration".into(), json!(generation));
        }
        Value::Object(object).to_string()
    }
}

impl std::fmt::Display for ApiFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.stderr_line())
    }
}

impl std::error::Error for ApiFailure {}

pub trait BodySink {
    fn write_chunk(&mut self, bytes: &[u8]) -> io::Result<()>;
}

impl BodySink for Vec<u8> {
    fn write_chunk(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.extend_from_slice(bytes);
        Ok(())
    }
}

impl BodySink for std::fs::File {
    fn write_chunk(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.write_all(bytes)
    }
}

pub struct ApiRequest {
    pub endpoint: String,
    pub method: String,
    pub path: String,
    pub body: Option<Vec<u8>>,
    pub cas_current: bool,
    pub bearer: Option<String>,
    pub session_file: Option<std::path::PathBuf>,
}

#[derive(Debug)]
pub struct ApiSuccess {
    pub status: u16,
}

pub struct Invocation {
    pub endpoint: String,
    pub method: String,
    pub path: String,
    pub input: Option<std::path::PathBuf>,
    pub output: Option<std::path::PathBuf>,
    pub cas_current: bool,
    pub key_file: Option<std::path::PathBuf>,
    pub session_file: Option<std::path::PathBuf>,
}

pub async fn invoke(
    invocation: Invocation,
    sink: &mut dyn BodySink,
) -> Result<ApiSuccess, ApiFailure> {
    let body = read_input(invocation.input.as_deref())?;
    let bearer = invocation
        .key_file
        .as_deref()
        .map(read_key_file)
        .transpose()?;
    let request = ApiRequest {
        endpoint: invocation.endpoint,
        method: invocation.method,
        path: invocation.path,
        body,
        cas_current: invocation.cas_current,
        bearer,
        session_file: invocation.session_file,
    };
    if let Some(path) = invocation.output {
        let mut file = OutputFile::create(path)?;
        file.preflight()?;
        match execute_with(request, &mut file, true).await {
            Ok(success) => {
                if let Err(error) = file.commit(success.status) {
                    file.discard();
                    return Err(error);
                }
                Ok(success)
            }
            Err(error) => {
                file.discard();
                Err(error)
            }
        }
    } else {
        execute_with(request, sink, false).await
    }
}

struct OutputFile {
    path: std::path::PathBuf,
    temporary: std::path::PathBuf,
    file: Option<std::fs::File>,
}

impl OutputFile {
    fn create(path: std::path::PathBuf) -> Result<Self, ApiFailure> {
        let temporary = sibling_temp(&path);
        Ok(Self {
            path,
            temporary,
            file: None,
        })
    }

    fn preflight(&self) -> Result<(), ApiFailure> {
        reject_symlink(&self.path)?;
        reject_symlink(&self.temporary)?;
        if self.path.exists() {
            if self.path.is_dir() {
                return Err(io_failure(format!(
                    "{} is a directory",
                    self.path.display()
                )));
            }
            let _existing = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&self.path)
                .map_err(io_failure)?;
            return Ok(());
        }
        let parent = self
            .path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        if !parent.is_dir() {
            return Err(io_failure(format!(
                "output directory {} is missing",
                parent.display()
            )));
        }
        let probe = parent.join(format!(".ocg-output-probe-{}", std::process::id()));
        let _ = std::fs::remove_file(&probe);
        session_file::open_private(&probe).map_err(io_failure)?;
        let _ = std::fs::remove_file(&probe);
        Ok(())
    }

    fn open_temp(&mut self) -> io::Result<()> {
        if self.file.is_none() {
            let file = session_file::open_private(&self.temporary)
                .map_err(|error| io::Error::other(error.to_string()))?;
            self.file = Some(file);
        }
        Ok(())
    }

    fn discard(&mut self) {
        self.file.take();
        let _ = std::fs::remove_file(&self.temporary);
    }

    fn commit(&mut self, status: u16) -> Result<(), ApiFailure> {
        self.open_temp()
            .map_err(|error| accepted_not_saved(status, error))?;
        if let Some(file) = self.file.take() {
            file.sync_all()
                .map_err(|error| accepted_not_saved(status, error))?;
        }
        #[cfg(windows)]
        if self.path.exists() {
            session_file::restrict_private_permissions(&self.path)
                .map_err(|error| accepted_not_saved(status, error))?;
        }
        replace_file(&self.temporary, &self.path)
            .map_err(|error| accepted_not_saved(status, error))?;
        #[cfg(windows)]
        session_file::restrict_private_permissions(&self.path)
            .map_err(|error| accepted_not_saved(status, error))?;
        Ok(())
    }
}

impl BodySink for OutputFile {
    fn write_chunk(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.open_temp()?;
        match self.file.as_mut() {
            Some(file) => file.write_all(bytes),
            None => Err(io::Error::other("output file was not opened")),
        }
    }
}

fn sibling_temp(path: &Path) -> std::path::PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".ocg-tmp");
    path.with_file_name(name)
}

fn replace_file(from: &Path, to: &Path) -> io::Result<()> {
    match std::fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(_) if to.exists() => {
            std::fs::remove_file(to)?;
            std::fs::rename(from, to)
        }
        Err(error) => Err(error),
    }
}

pub async fn execute(
    request: ApiRequest,
    sink: &mut dyn BodySink,
) -> Result<ApiSuccess, ApiFailure> {
    execute_with(request, sink, false).await
}

async fn execute_with(
    mut request: ApiRequest,
    sink: &mut dyn BodySink,
    private_output: bool,
) -> Result<ApiSuccess, ApiFailure> {
    let method = normalize_method(&request.method)?;
    let endpoint = endpoint::parse_endpoint(&request.endpoint).map_err(usage)?;
    let path = endpoint::parse_route(&request.path).map_err(usage)?;
    if let Some(body) = &request.body
        && body.len() > MAX_INPUT_BYTES
    {
        return Err(usage(anyhow::anyhow!(
            "input exceeds the {MAX_INPUT_BYTES} byte limit"
        )));
    }
    if request.bearer.is_some() && !endpoint::is_inference(&path) {
        return Err(usage(anyhow::anyhow!(
            "--key-file is sent only on inference routes"
        )));
    }
    let secrets = secrets_of(&request);
    let body = prepare_body(
        &endpoint,
        request.body.take(),
        request.cas_current,
        &request,
    )
    .await?;
    let stream = body.as_deref().is_some_and(requests_stream);
    execute_prepared(
        &endpoint,
        &method,
        &path,
        body,
        stream,
        &request,
        &secrets,
        sink,
        private_output,
    )
    .await
}

async fn prepare_body(
    endpoint: &Endpoint,
    body: Option<Vec<u8>>,
    cas_current: bool,
    request: &ApiRequest,
) -> Result<Option<Vec<u8>>, ApiFailure> {
    let Some(body) = body else {
        if cas_current {
            return Err(usage(anyhow::anyhow!(
                "CAS injection requires a JSON object body"
            )));
        }
        return Ok(None);
    };
    if !cas_current {
        return Ok(Some(body));
    }
    let value: Value = serde_json::from_slice(&body)
        .map_err(|_| usage(anyhow::anyhow!("CAS injection requires a JSON object body")))?;
    let Some(mut object) = value.as_object().cloned() else {
        return Err(usage(anyhow::anyhow!(
            "CAS injection requires a JSON object body"
        )));
    };
    let needs_revision = !object.contains_key("expectedRevision");
    let needs_generation = !object.contains_key("processGeneration");
    if !needs_revision && !needs_generation {
        return Ok(Some(body));
    }
    let status = fetch_auth_status(endpoint, request).await?;
    if needs_revision {
        object.insert("expectedRevision".into(), json!(status.revision));
    }
    if needs_generation {
        object.insert("processGeneration".into(), json!(status.process_generation));
    }
    serde_json::to_vec(&Value::Object(object))
        .map(Some)
        .map_err(io_failure)
}

struct AuthEpoch {
    revision: u64,
    process_generation: u64,
}

async fn fetch_auth_status(
    endpoint: &Endpoint,
    request: &ApiRequest,
) -> Result<AuthEpoch, ApiFailure> {
    let mut ignored = Vec::new();
    let probe = ApiRequest {
        endpoint: endpoint.origin.clone(),
        method: "GET".into(),
        path: "/dashboard/api/v4/auth/status".into(),
        body: None,
        cas_current: false,
        bearer: None,
        session_file: request.session_file.clone(),
    };
    // Public auth status is not a mutation and must not recurse into CAS.
    execute_prepared(
        endpoint,
        "GET",
        "/dashboard/api/v4/auth/status",
        None,
        false,
        &probe,
        &[],
        &mut ignored,
        false,
    )
    .await?;
    let value: Value = serde_json::from_slice(&ignored).map_err(|_| {
        usage(anyhow::anyhow!(
            "auth status response was not a JSON object"
        ))
    })?;
    let revision = value
        .get("revision")
        .and_then(Value::as_u64)
        .ok_or_else(|| usage(anyhow::anyhow!("auth status response has no revision")))?;
    let process_generation = value
        .get("processGeneration")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            usage(anyhow::anyhow!(
                "auth status response has no processGeneration"
            ))
        })?;
    Ok(AuthEpoch {
        revision,
        process_generation,
    })
}

// One prepared HTTP shot. The arguments stay separate so CAS, secrets, and the sink are not rebundled.
#[allow(clippy::too_many_arguments)]
async fn execute_prepared(
    endpoint: &Endpoint,
    method: &str,
    path: &str,
    body: Option<Vec<u8>>,
    stream: bool,
    request: &ApiRequest,
    secrets: &[String],
    sink: &mut dyn BodySink,
    private_output: bool,
) -> Result<ApiSuccess, ApiFailure> {
    let url = format!("{}{path}", endpoint.origin);
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
        .no_proxy()
        .user_agent("ocg")
        .build()
        .map_err(|error| transport(&redact(&error.to_string(), secrets)))?;
    let http_method = reqwest::Method::from_bytes(method.as_bytes())
        .map_err(|_| usage(anyhow::anyhow!("method is not an HTTP method")))?;
    let mut builder = client.request(http_method, &url);
    if let Some(timeout) = request_timeout(path, stream) {
        builder = builder.timeout(timeout);
    }
    if let Some(body) = body {
        builder = builder
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body);
    }
    let mut secret_values = secrets.to_vec();
    if endpoint::is_inference(path)
        && let Some(bearer) = &request.bearer
    {
        builder = builder.header(reqwest::header::AUTHORIZATION, format!("Bearer {bearer}"));
    }
    if !endpoint::is_inference(path)
        && let Some(session_path) = &request.session_file
    {
        let store = SessionStore::load(session_path, &endpoint.origin)
            .map_err(|error| usage_redacted(error, &secret_values))?;
        extend_secrets(&mut secret_values, store.secret_values());
        if let Some(cookie) = store.header_for(path, endpoint.origin.starts_with("https://")) {
            extend_secrets(&mut secret_values, cookie_pair_values(&cookie));
            builder = builder.header(reqwest::header::COOKIE, cookie);
        }
    }
    let response = builder
        .send()
        .await
        .map_err(|error| transport(&redact(&error.to_string(), &secret_values)))?;
    extend_secrets(&mut secret_values, set_cookie_secrets(response.headers()));
    let status = response.status().as_u16();
    if let Some(session_path) = &request.session_file {
        match persist_session(session_path, &endpoint.origin, response.headers()) {
            Ok(values) => extend_secrets(&mut secret_values, values),
            Err(error) if (200..300).contains(&status) && method_mutates(method) => {
                return Err(accepted_not_saved(
                    status,
                    redact(&error.to_string(), &secret_values),
                ));
            }
            Err(error) if (200..300).contains(&status) => {
                return Err(io_failure(redact(&error.to_string(), &secret_values)));
            }
            Err(_) => {}
        }
    }
    if (300..400).contains(&status) {
        return Err(ApiFailure {
            exit_code: EXIT_HTTP,
            status: Some(status),
            code: "redirectRefused".into(),
            message: "redirects are not followed".into(),
            current_revision: None,
            process_generation: None,
        });
    }
    let event_stream = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.to_ascii_lowercase().starts_with("text/event-stream"));
    if !(200..300).contains(&status) {
        let bytes = read_limited(response, MAX_ERROR_BODY_BYTES, &secret_values).await?;
        return Err(failure_from_status(status, &bytes, &secret_values));
    }
    let pass_through = stream || event_stream || endpoint::is_inference(path);
    let cap = if pass_through {
        None
    } else {
        Some(MAX_NON_STREAM_OUTPUT_BYTES)
    };
    write_response(
        response,
        cap,
        &secret_values,
        sink,
        status,
        private_output,
        pass_through,
    )
    .await?;
    Ok(ApiSuccess { status })
}

fn request_timeout(path: &str, stream: bool) -> Option<Duration> {
    if stream {
        None
    } else if endpoint::is_inference(path) {
        Some(INFERENCE_TIMEOUT)
    } else {
        Some(DASHBOARD_TIMEOUT)
    }
}

fn requests_stream(body: &[u8]) -> bool {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| value.get("stream").and_then(Value::as_bool))
        .unwrap_or(false)
}

async fn write_response(
    mut response: reqwest::Response,
    cap: Option<usize>,
    secrets: &[String],
    sink: &mut dyn BodySink,
    status: u16,
    private_output: bool,
    pass_through: bool,
) -> Result<(), ApiFailure> {
    if !private_output && !pass_through {
        let mut body = Vec::new();
        loop {
            let chunk = response
                .chunk()
                .await
                .map_err(|error| accepted_not_saved(status, redact(&error.to_string(), secrets)))?;
            let Some(chunk) = chunk else {
                break;
            };
            if let Some(cap) = cap
                && body.len().saturating_add(chunk.len()) > cap
            {
                return Err(accepted_not_saved(
                    status,
                    format!("response exceeds the {cap} byte limit"),
                ));
            }
            body.extend_from_slice(&chunk);
        }
        let redacted = redact_stdout_body(&body);
        return sink
            .write_chunk(&redacted)
            .map_err(|error| accepted_not_saved(status, redact(&error.to_string(), secrets)));
    }
    let mut total = 0usize;
    loop {
        let chunk = response.chunk().await.map_err(|error| {
            let message = redact(&error.to_string(), secrets);
            if private_output {
                accepted_not_saved(status, message)
            } else {
                transport(&message)
            }
        })?;
        let Some(chunk) = chunk else {
            return Ok(());
        };
        total = total.saturating_add(chunk.len());
        if let Some(cap) = cap
            && total > cap
        {
            return Err(accepted_not_saved(
                status,
                format!("response exceeds the {cap} byte limit"),
            ));
        }
        sink.write_chunk(&chunk)
            .map_err(|error| accepted_not_saved(status, redact(&error.to_string(), secrets)))?;
    }
}

async fn read_limited(
    mut response: reqwest::Response,
    cap: usize,
    secrets: &[String],
) -> Result<Vec<u8>, ApiFailure> {
    let mut body = Vec::new();
    loop {
        let chunk = response
            .chunk()
            .await
            .map_err(|error| transport(&redact(&error.to_string(), secrets)))?;
        let Some(chunk) = chunk else {
            break;
        };
        let room = cap.saturating_sub(body.len());
        if room == 0 {
            break;
        }
        if chunk.len() > room {
            body.extend_from_slice(&chunk[..room]);
            break;
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn failure_from_status(status: u16, body: &[u8], secrets: &[String]) -> ApiFailure {
    let parsed: Option<Value> = serde_json::from_slice(body).ok();
    let code = parsed
        .as_ref()
        .and_then(|value| value.get("code"))
        .and_then(Value::as_str)
        .unwrap_or("http")
        .to_string();
    let message = parsed
        .as_ref()
        .and_then(|value| value.get("message"))
        .and_then(Value::as_str)
        .map(|message| redact(message, secrets))
        .unwrap_or_else(|| format!("host returned HTTP {status}"));
    let current_revision = parsed
        .as_ref()
        .and_then(|value| value.get("currentRevision"))
        .and_then(Value::as_u64);
    let process_generation = parsed
        .as_ref()
        .and_then(|value| value.get("processGeneration"))
        .and_then(Value::as_u64);
    ApiFailure {
        exit_code: EXIT_HTTP,
        status: Some(status),
        code: redact(&code, secrets),
        message,
        current_revision,
        process_generation,
    }
}

fn normalize_method(method: &str) -> Result<String, ApiFailure> {
    let method = method.to_ascii_uppercase();
    if matches!(method.as_str(), "GET" | "POST" | "PUT" | "PATCH" | "DELETE") {
        Ok(method)
    } else {
        Err(usage(anyhow::anyhow!(
            "method must be GET, POST, PUT, PATCH, or DELETE"
        )))
    }
}

fn read_input(path: Option<&Path>) -> Result<Option<Vec<u8>>, ApiFailure> {
    let Some(path) = path else {
        return Ok(None);
    };
    if path.as_os_str() == "-" {
        return read_capped(std::io::stdin(), MAX_INPUT_BYTES).map(Some);
    }
    reject_symlink(path)?;
    let file = std::fs::File::open(path).map_err(io_failure)?;
    read_capped(file, MAX_INPUT_BYTES).map(Some)
}

fn read_key_file(path: &Path) -> Result<String, ApiFailure> {
    reject_symlink(path)?;
    let bytes = read_capped(
        std::fs::File::open(path).map_err(io_failure)?,
        MAX_KEY_FILE_BYTES,
    )?;
    let mut text =
        String::from_utf8(bytes).map_err(|_| usage(anyhow::anyhow!("key file must be UTF-8")))?;
    if text.ends_with('\n') {
        text.pop();
        if text.ends_with('\r') {
            text.pop();
        }
    }
    if text.is_empty() || text.chars().any(|character| character.is_control()) {
        return Err(usage(anyhow::anyhow!(
            "key file must contain one secret line"
        )));
    }
    Ok(text)
}

fn read_capped(mut reader: impl Read, cap: usize) -> Result<Vec<u8>, ApiFailure> {
    let mut body = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        let read = reader.read(&mut buffer).map_err(io_failure)?;
        if read == 0 {
            break;
        }
        if body.len().saturating_add(read) > cap {
            return Err(usage(anyhow::anyhow!("input exceeds the {cap} byte limit")));
        }
        body.extend_from_slice(&buffer[..read]);
    }
    Ok(body)
}

fn reject_symlink(path: &Path) -> Result<(), ApiFailure> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(usage(anyhow::anyhow!(
            "refusing to use symlink {}",
            path.display()
        ))),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_failure(error)),
    }
}

fn secrets_of(request: &ApiRequest) -> Vec<String> {
    let mut secrets = Vec::new();
    if let Some(bearer) = &request.bearer {
        push_secret(&mut secrets, bearer);
    }
    if let Some(body) = &request.body
        && let Ok(value) = serde_json::from_slice::<Value>(body)
    {
        collect_json_secrets(&value, &mut secrets);
    }
    secrets
}

const SECRET_JSON_FIELDS: &[&str] = &[
    "password",
    "apiKey",
    "inferenceKey",
    "secretInput",
    "bundlePassword",
    "managementKey",
    "key",
    "secret",
    "primaryKey",
];

fn collect_json_secrets(value: &Value, secrets: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if SECRET_JSON_FIELDS.contains(&key.as_str())
                    && let Some(text) = child.as_str()
                {
                    push_secret(secrets, text);
                }
                collect_json_secrets(child, secrets);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_json_secrets(item, secrets);
            }
        }
        _ => {}
    }
}

fn push_secret(secrets: &mut Vec<String>, value: &str) {
    if !value.is_empty() && !secrets.iter().any(|existing| existing == value) {
        secrets.push(value.to_string());
    }
}

fn extend_secrets(secrets: &mut Vec<String>, values: impl IntoIterator<Item = String>) {
    for value in values {
        push_secret(secrets, &value);
    }
}

fn cookie_pair_values(header: &str) -> Vec<String> {
    header
        .split(';')
        .filter_map(|pair| {
            pair.split_once('=')
                .map(|(_, value)| value.trim().to_string())
        })
        .collect()
}

fn set_cookie_secrets(headers: &reqwest::header::HeaderMap) -> Vec<String> {
    headers
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .filter_map(|text| {
            let pair = text.split(';').next()?.trim();
            let (_, cookie_value) = pair.split_once('=')?;
            let cookie_value = cookie_value.trim();
            (!cookie_value.is_empty()).then(|| cookie_value.to_string())
        })
        .collect()
}

fn persist_session(
    path: &Path,
    endpoint: &str,
    headers: &reqwest::header::HeaderMap,
) -> anyhow::Result<Vec<String>> {
    let mut store = SessionStore::load(path, endpoint)?;
    store.apply_set_cookie(headers)?;
    Ok(store.secret_values())
}

fn method_mutates(method: &str) -> bool {
    matches!(method, "POST" | "PUT" | "PATCH" | "DELETE")
}

fn redact_stdout_body(bytes: &[u8]) -> Vec<u8> {
    let Ok(mut value) = serde_json::from_slice::<Value>(bytes) else {
        return bytes.to_vec();
    };
    let mut changed = false;
    redact_json_value(&mut value, &mut changed);
    if !changed {
        return bytes.to_vec();
    }
    serde_json::to_vec(&value).unwrap_or_else(|_| bytes.to_vec())
}

fn redact_json_value(value: &mut Value, changed: &mut bool) {
    match value {
        Value::Array(items) => {
            for item in items {
                redact_json_value(item, changed);
            }
        }
        Value::Object(map) => {
            let sub_keys = map.remove("subKeys");
            let names = map.keys().cloned().collect::<Vec<_>>();
            for name in names {
                if SECRET_JSON_FIELDS.contains(&name.as_str()) {
                    if map
                        .get(&name)
                        .and_then(Value::as_str)
                        .is_some_and(|text| !text.is_empty())
                    {
                        map.insert(name, json!("[redacted]"));
                        *changed = true;
                    }
                } else if let Some(child) = map.get_mut(&name) {
                    redact_json_value(child, changed);
                }
            }
            if let Some(mut sub_keys) = sub_keys {
                if let Value::Array(items) = &mut sub_keys {
                    for item in items.iter_mut() {
                        if let Value::Object(fields) = item
                            && fields
                                .get("value")
                                .and_then(Value::as_str)
                                .is_some_and(|text| !text.is_empty())
                        {
                            fields.insert("value".into(), json!("[redacted]"));
                            *changed = true;
                        }
                        redact_json_value(item, changed);
                    }
                }
                map.insert("subKeys".into(), sub_keys);
            }
        }
        _ => {}
    }
}

fn redact(text: &str, secrets: &[String]) -> String {
    let mut ordered: Vec<&String> = secrets.iter().filter(|secret| !secret.is_empty()).collect();
    // Longer values first so a short secret that is a prefix cannot split a longer one.
    ordered.sort_by_key(|secret| std::cmp::Reverse(secret.len()));
    let mut output = text.to_string();
    for secret in ordered {
        output = output.replace(secret, "[redacted]");
    }
    if output.to_ascii_lowercase().contains("set-cookie") {
        output = output
            .lines()
            .filter(|line| !line.to_ascii_lowercase().contains("set-cookie"))
            .collect::<Vec<_>>()
            .join("\n");
        if output.is_empty() {
            output = "host returned a secret header".into();
        }
    }
    output
}

fn usage(error: anyhow::Error) -> ApiFailure {
    ApiFailure {
        exit_code: EXIT_USAGE,
        status: None,
        code: "invalidRequest".into(),
        message: error.to_string(),
        current_revision: None,
        process_generation: None,
    }
}

fn usage_redacted(error: impl std::fmt::Display, secrets: &[String]) -> ApiFailure {
    let mut failure = usage(anyhow::anyhow!("{error}"));
    failure.message = redact(&failure.message, secrets);
    failure
}

fn accepted_not_saved(status: u16, error: impl std::fmt::Display) -> ApiFailure {
    ApiFailure {
        exit_code: EXIT_ACCEPTED_NOT_SAVED,
        status: Some(status),
        code: "acceptedNotSaved".into(),
        message: format!(
            "the server already accepted this request (HTTP {status}); the response was not saved ({error}). The request was not replayed."
        ),
        current_revision: None,
        process_generation: None,
    }
}

fn transport(message: &str) -> ApiFailure {
    ApiFailure {
        exit_code: EXIT_TRANSPORT,
        status: None,
        code: "transport".into(),
        message: message.to_string(),
        current_revision: None,
        process_generation: None,
    }
}

fn io_failure(error: impl std::fmt::Display) -> ApiFailure {
    ApiFailure {
        exit_code: EXIT_IO,
        status: None,
        code: "io".into(),
        message: error.to_string(),
        current_revision: None,
        process_generation: None,
    }
}

pub fn schema_json(version: &str) -> Result<String, ApiFailure> {
    match version {
        "v3" => Ok(ocg_core::dashboard_v3::contract_schema_pretty()),
        "v4" => Ok(ocg_core::dashboard_v4::contract_schema_pretty()),
        _ => Err(usage(anyhow::anyhow!("schema version must be v3 or v4"))),
    }
}

#[cfg(test)]
#[path = "api_cmd/tests.rs"]
mod tests;
