//! One-time maintainer protocol-matrix runner. It never writes OCG state.
//!
//! Default invocation is an offline inventory:
//! `cargo run -p ocg-core --example probe_protocols -- --data-dir <dir>`.
//! Add `--run` to send the bounded real requests. Results go to a timestamped
//! JSONL and Markdown pair in the system temp directory.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use futures_util::StreamExt;
use ocg_core::crypto::{KeyCipher, MachineBoundCipher};
use ocg_core::gateway::free_models::resolve_upstream_base;
use ocg_core::kernel::protocol::supported_model_protocol_profiles;
use ocg_core::models::{AppConfig, UpstreamChannel};
use ocg_core::provider::{
    COMMAND_CODE_GOAT_BASE_URL, COMMAND_CODE_PROVIDER_ID, OPENCODE_PROVIDER_ID,
    OPENCODE_ZEN_FREE_PROVIDER_ID,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::Serialize;
use serde_json::{Value, json};

const MAX_TOKENS: u8 = 16;
const REQUEST_TIMEOUT_SECS: u64 = 90;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Protocol {
    ChatCompletions,
    Responses,
    Messages,
}

impl Protocol {
    const ALL: [Self; 3] = [Self::ChatCompletions, Self::Responses, Self::Messages];

    fn path(self) -> &'static str {
        match self {
            Self::ChatCompletions => "/v1/chat/completions",
            Self::Responses => "/v1/responses",
            Self::Messages => "/v1/messages",
        }
    }

    fn goat_path(self) -> &'static str {
        match self {
            Self::ChatCompletions => "/chat/completions",
            Self::Responses => "/responses",
            Self::Messages => "/messages",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "chat" | "chat_completions" => Some(Self::ChatCompletions),
            "responses" => Some(Self::Responses),
            "messages" => Some(Self::Messages),
            _ => None,
        }
    }
}

#[derive(Clone)]
struct Target {
    provider_id: &'static str,
    base_url: String,
    bearer_key: Option<String>,
    models: Vec<String>,
}

struct StoredAccount {
    id: String,
    name: String,
    provider_id: String,

    key_cipher: String,
    enabled: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    timestamp: String,
    provider_id: String,
    model_id: String,
    protocol: Protocol,
    stream: bool,
    status: u16,
    ok: bool,
    duration_ms: u128,
    evidence: String,
}

struct Options {
    data_dir: PathBuf,
    account_filter: Option<String>,
    model_filter: Option<String>,
    protocol_filter: Option<Protocol>,
    providers: Vec<String>,
    output_dir: Option<PathBuf>,
    concurrency: usize,
    run: bool,
}

fn usage() -> ! {
    eprintln!(
        "Usage: probe_protocols [--data-dir DIR] [--providers go,zen,goat] [--account NAME_OR_ID] [--model EXACT_ID] [--protocol chat|responses|messages] [--output-dir DIR] [--concurrency 1..8] [--run]\n\
         Default is offline inventory only. --run sends real one-shot requests; it never writes SQLite."
    );
    std::process::exit(2);
}

fn options() -> Options {
    let mut args = std::env::args().skip(1);
    let mut data_dir = None;
    let mut account_filter = None;
    let mut model_filter = None;
    let mut protocol_filter = None;
    let mut providers = vec!["go".into(), "zen".into(), "goat".into()];
    let mut output_dir = None;
    let mut concurrency = 4;
    let mut run = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--data-dir" => data_dir = Some(PathBuf::from(args.next().unwrap_or_else(|| usage()))),
            "--account" => account_filter = Some(args.next().unwrap_or_else(|| usage())),
            "--model" => model_filter = Some(args.next().unwrap_or_else(|| usage())),
            "--protocol" => {
                protocol_filter = Some(
                    Protocol::parse(&args.next().unwrap_or_else(|| usage()))
                        .unwrap_or_else(|| usage()),
                )
            }
            "--providers" => {
                providers = args
                    .next()
                    .unwrap_or_else(|| usage())
                    .split(',')
                    .map(str::to_string)
                    .collect();
                if providers.is_empty()
                    || providers
                        .iter()
                        .any(|provider| !matches!(provider.as_str(), "go" | "zen" | "goat"))
                {
                    usage();
                }
            }
            "--output-dir" => {
                output_dir = Some(PathBuf::from(args.next().unwrap_or_else(|| usage())))
            }
            "--concurrency" => {
                concurrency = args
                    .next()
                    .unwrap_or_else(|| usage())
                    .parse()
                    .unwrap_or_else(|_| usage());
                if !(1..=8).contains(&concurrency) {
                    usage();
                }
            }
            "--run" => run = true,
            "--help" | "-h" => usage(),
            _ => usage(),
        }
    }
    let data_dir = data_dir.unwrap_or_else(|| {
        PathBuf::from(
            std::env::var("USERPROFILE")
                .or_else(|_| std::env::var("HOME"))
                .unwrap_or_else(|_| ".".into()),
        )
        .join(".ocg-mgr")
    });
    Options {
        data_dir,
        account_filter,
        model_filter,
        protocol_filter,
        providers,
        output_dir,
        concurrency,
        run,
    }
}

fn enabled_provider(options: &Options, provider: &str) -> bool {
    options.providers.iter().any(|item| item == provider)
}

fn matches(account: &StoredAccount, provider: &str, filter: Option<&str>) -> bool {
    account.enabled
        && account.provider_id == provider
        && filter.is_none_or(|value| account.name.contains(value) || account.id.starts_with(value))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let options = options();
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(MachineBoundCipher::new());
    let db_path = options.data_dir.join("data.sqlite");
    let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let config_json: String = conn.query_row(
        "SELECT value FROM settings WHERE key = 'config'",
        [],
        |row| row.get(0),
    )?;
    let config: AppConfig = serde_json::from_str(&config_json)?;
    let accounts = conn
        .prepare("SELECT id, name, provider_id, key_cipher, enabled FROM accounts")?
        .query_map([], |row| {
            Ok(StoredAccount {
                id: row.get(0)?,
                name: row.get(1)?,
                provider_id: row.get(2)?,
                key_cipher: row.get(3)?,
                enabled: row.get::<_, i64>(4)? != 0,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let catalog = |id: &str| -> anyhow::Result<Vec<String>> {
        let persisted: Option<String> = conn.query_row("SELECT catalog_models_json FROM provider_contract_scopes WHERE scope_kind = 'provider' AND scope_id = ?1", [id], |row| row.get(0)).optional()?;
        let models = persisted
            .and_then(|json| serde_json::from_str::<Vec<String>>(&json).ok())
            .filter(|models| !models.is_empty());
        Ok(models.unwrap_or_else(|| match id {
            OPENCODE_PROVIDER_ID => supported_model_protocol_profiles()
                .map(|(model, _, _)| model.to_string())
                .collect(),
            // GOAT probes require a saved current catalog, never a live whitelist.
            COMMAND_CODE_PROVIDER_ID => Vec::new(),
            _ => Vec::new(),
        }))
    };
    let go_accounts: Vec<_> = accounts
        .iter()
        .filter(|account| {
            matches(
                account,
                OPENCODE_PROVIDER_ID,
                options.account_filter.as_deref(),
            )
        })
        .collect();
    let goat_accounts: Vec<_> = accounts
        .iter()
        .filter(|account| {
            matches(
                account,
                COMMAND_CODE_PROVIDER_ID,
                options.account_filter.as_deref(),
            )
        })
        .collect();
    let free_accounts = accounts
        .iter()
        .filter(|account| {
            matches(
                account,
                OPENCODE_ZEN_FREE_PROVIDER_ID,
                options.account_filter.as_deref(),
            )
        })
        .count();
    let go_base = ocg_core::gateway::free_models::opencode_go_base_url(&config.upstream_base_url);
    let free_base = resolve_upstream_base(UpstreamChannel::Free, &go_base).ok();

    println!("offline inventory (no network unless --run):");
    let go_catalog = catalog(OPENCODE_PROVIDER_ID)?;
    let free_catalog = catalog(OPENCODE_ZEN_FREE_PROVIDER_ID)?;
    let goat_catalog = catalog(COMMAND_CODE_PROVIDER_ID)?;
    if enabled_provider(&options, "go") {
        println!(
            "  {OPENCODE_PROVIDER_ID}: models={} enabled matching accounts={}",
            go_catalog.len(),
            go_accounts.len()
        );
    }
    if enabled_provider(&options, "zen") {
        println!(
            "  {OPENCODE_ZEN_FREE_PROVIDER_ID}: models={} enabled matching accounts={} base={}",
            free_catalog.len(),
            free_accounts,
            free_base.as_deref().unwrap_or("unavailable")
        );
    }
    if enabled_provider(&options, "goat") {
        println!(
            "  {COMMAND_CODE_PROVIDER_ID}: models={} enabled matching accounts={}",
            goat_catalog.len(),
            goat_accounts.len()
        );
    }
    if !options.run {
        return Ok(());
    }

    let first_key = |accounts: &[&StoredAccount]| -> anyhow::Result<Option<String>> {
        accounts
            .first()
            .map(|account| cipher.decrypt(&account.key_cipher))
            .transpose()
    };
    let go_key = first_key(&go_accounts)?;
    let goat_key = first_key(&goat_accounts)?;
    let mut targets = Vec::new();
    if enabled_provider(&options, "go")
        && let Some(key) = go_key
    {
        targets.push(Target {
            provider_id: OPENCODE_PROVIDER_ID,
            base_url: go_base,
            bearer_key: Some(key),
            models: go_catalog,
        });
    }
    if enabled_provider(&options, "zen")
        && let Some(base_url) = free_base
    {
        targets.push(Target {
            provider_id: OPENCODE_ZEN_FREE_PROVIDER_ID,
            base_url,
            bearer_key: None,
            models: free_catalog,
        });
    }
    if enabled_provider(&options, "goat")
        && let Some(key) = goat_key
    {
        targets.push(Target {
            provider_id: COMMAND_CODE_PROVIDER_ID,
            base_url: COMMAND_CODE_GOAT_BASE_URL.trim_end_matches('/').to_string(),
            bearer_key: Some(key),
            models: goat_catalog,
        });
    }

    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let root = options
        .output_dir
        .unwrap_or_else(std::env::temp_dir)
        .join(format!("ocg-protocol-matrix-{timestamp}"));
    let jsonl_path = root.with_extension("jsonl");
    let markdown_path = root.with_extension("md");
    let mut jobs = Vec::new();
    for target in &targets {
        for model in &target.models {
            if options
                .model_filter
                .as_deref()
                .is_some_and(|filter| model != filter)
            {
                continue;
            }
            for protocol in Protocol::ALL {
                if options
                    .protocol_filter
                    .is_some_and(|filter| protocol != filter)
                {
                    continue;
                }
                for stream in [false, true] {
                    jobs.push((target.clone(), model.clone(), protocol, stream));
                }
            }
        }
    }
    let records: Vec<Record> = futures_util::stream::iter(jobs)
        .map(|(target, model, protocol, stream)| probe(&client, target, model, protocol, stream))
        .buffer_unordered(options.concurrency)
        .collect()
        .await;
    let jsonl = records
        .iter()
        .map(serde_json::to_string)
        .collect::<Result<Vec<_>, _>>()?
        .join("\n")
        + "\n";
    std::fs::write(&jsonl_path, jsonl)?;
    let mut markdown = format!(
        "# OCG protocol matrix {timestamp}\n\n| Provider | Model | Protocol | Stream | Status | OK | ms | Evidence |\n|---|---|---|---:|---:|---:|---:|---|\n"
    );
    for row in &records {
        markdown.push_str(&format!(
            "| {} | `{}` | {:?} | {} | {} | {} | {} | {} |\n",
            row.provider_id,
            row.model_id,
            row.protocol,
            row.stream,
            row.status,
            row.ok,
            row.duration_ms,
            row.evidence.replace('|', "\\|")
        ));
    }
    std::fs::write(&markdown_path, markdown)?;
    println!(
        "wrote {} records (Go accounts={}, Zen accounts={}, GOAT accounts={})",
        records.len(),
        go_accounts.len(),
        free_accounts,
        goat_accounts.len()
    );
    println!(
        "jsonl: {}\nmarkdown: {}",
        jsonl_path.display(),
        markdown_path.display()
    );
    Ok(())
}

async fn probe(
    client: &reqwest::Client,
    target: Target,
    model: String,
    protocol: Protocol,
    stream: bool,
) -> Record {
    let url = format!(
        "{}{}",
        target.base_url,
        if target.provider_id == COMMAND_CODE_PROVIDER_ID {
            protocol.goat_path()
        } else {
            protocol.path()
        }
    );
    let started = Instant::now();
    let mut request = client
        .post(url)
        .header("content-type", "application/json")
        .json(&body(&model, protocol, stream));
    if let Some(key) = target.bearer_key.as_deref() {
        request = if target.provider_id == OPENCODE_PROVIDER_ID
            && matches!(protocol, Protocol::Messages)
        {
            request
                .header("x-api-key", key)
                .header("anthropic-version", "2023-06-01")
        } else {
            request.bearer_auth(key)
        };
    }
    let response = request
        .timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .send()
        .await;
    let duration_ms = started.elapsed().as_millis();
    match response {
        Ok(response) => {
            let status = response.status().as_u16();
            let (ok, evidence) = if stream {
                stream_evidence(response, protocol).await
            } else {
                json_evidence(response, protocol).await
            };
            Record {
                timestamp: chrono::Utc::now().to_rfc3339(),
                provider_id: target.provider_id.into(),
                model_id: model,
                protocol,
                stream,
                status,
                ok: status < 300 && ok,
                duration_ms,
                evidence: redact(&evidence, target.bearer_key.as_deref()),
            }
        }
        Err(error) => Record {
            timestamp: chrono::Utc::now().to_rfc3339(),
            provider_id: target.provider_id.into(),
            model_id: model,
            protocol,
            stream,
            status: 0,
            ok: false,
            duration_ms,
            evidence: redact(&sanitize(&error.to_string()), target.bearer_key.as_deref()),
        },
    }
}

fn body(model: &str, protocol: Protocol, stream: bool) -> Value {
    match protocol {
        Protocol::ChatCompletions => {
            json!({"model": model, "messages": [{"role":"user","content":"Reply: PING"}], "max_tokens": MAX_TOKENS, "stream": stream})
        }
        Protocol::Responses => {
            json!({"model": model, "input":"Reply: PING", "max_output_tokens": MAX_TOKENS, "store":false, "stream":stream})
        }
        Protocol::Messages => {
            json!({"model":model, "messages":[{"role":"user","content":"Reply: PING"}], "max_tokens":MAX_TOKENS, "stream":stream})
        }
    }
}

async fn json_evidence(response: reqwest::Response, protocol: Protocol) -> (bool, String) {
    let status = response.status();
    let raw = response.text().await.unwrap_or_default();
    let value: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    let shaped = match protocol {
        Protocol::ChatCompletions => value.pointer("/choices/0").is_some(),
        Protocol::Responses => value.get("output").is_some() || value.get("output_text").is_some(),
        Protocol::Messages => value.get("content").and_then(Value::as_array).is_some(),
    };
    (
        status.is_success() && shaped,
        if shaped {
            "protocol-shaped JSON".into()
        } else {
            sanitize(&raw)
        },
    )
}

async fn stream_evidence(response: reqwest::Response, protocol: Protocol) -> (bool, String) {
    let status = response.status();
    if !status.is_success() {
        return (false, sanitize(&response.text().await.unwrap_or_default()));
    }
    let mut raw = String::new();
    let mut bytes = response.bytes_stream();
    while let Some(chunk) = bytes.next().await {
        match chunk {
            Ok(chunk) => raw.push_str(&String::from_utf8_lossy(&chunk)),
            Err(error) => return (false, sanitize(&error.to_string())),
        }
    }
    let token = match protocol {
        Protocol::ChatCompletions => "chat.completion.chunk",
        Protocol::Responses => "response.",
        Protocol::Messages => "message_",
    };
    let shaped = raw.contains("data:") && raw.contains(token);
    (
        shaped,
        if shaped {
            "protocol-shaped SSE".into()
        } else {
            sanitize(&raw)
        },
    )
}

fn sanitize(value: &str) -> String {
    value.replace(['\n', '\r'], " ").chars().take(300).collect()
}

fn redact(value: &str, key: Option<&str>) -> String {
    key.filter(|key| !key.is_empty())
        .map_or_else(|| value.to_string(), |key| value.replace(key, "[redacted]"))
}
