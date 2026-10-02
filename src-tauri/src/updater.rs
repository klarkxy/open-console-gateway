use anyhow::{Context, anyhow};
use ocg_core::models::{AppConfig, ProxyListDirection, ProxyMode};
use ocg_core::state::CoreState;
use std::sync::Arc;
use std::time::Duration;
use tauri::AppHandle;
use tauri_plugin_updater::{UpdaterBuilder, UpdaterExt};

const UPDATE_ENDPOINT: &str =
    "https://github.com/klarkxy/open-console-gateway/releases/latest/download/latest.json";
const UPDATE_CHECK_TIMEOUT: Duration = Duration::from_secs(30);
const UPDATE_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(15 * 60);

pub fn configure(app: &AppHandle, state: CoreState) -> crate::Result<()> {
    if cfg!(debug_assertions) {
        return Ok(());
    }

    let Some(public_key) = embedded_public_key() else {
        ocg_core::process_log::diagnostic(
            "warn",
            "update",
            "signed desktop updates are disabled because this build has no updater public key",
        );
        return Ok(());
    };

    #[cfg(target_os = "macos")]
    if std::env::current_exe()
        .ok()
        .is_some_and(|path| path.starts_with("/Volumes"))
    {
        ocg_core::process_log::diagnostic(
            "warn",
            "update",
            "signed desktop updates are disabled while the app is running from a mounted DMG",
        );
        return Ok(());
    }

    app.plugin(
        tauri_plugin_updater::Builder::new()
            .pubkey(public_key)
            .build(),
    )?;

    let app = app.clone();
    let task_state = state.clone();
    state.set_desktop_update_starter(Arc::new(move |expected_version| {
        let app = app.clone();
        let state = task_state.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(error) = install_update(app, state.clone(), expected_version).await {
                let message = format!("signed desktop update failed: {error:#}");
                state.set_desktop_update_failed(message);
                ocg_core::process_log::diagnostic(
                    "error",
                    "update",
                    "signed desktop update failed",
                );
            }
        });
        Ok(())
    }));

    Ok(())
}

async fn install_update(
    app: AppHandle,
    state: CoreState,
    expected_version: String,
) -> crate::Result<()> {
    let endpoint = UPDATE_ENDPOINT
        .parse()
        .context("invalid built-in updater endpoint")?;
    let updater = apply_updater_proxy(app.updater_builder(), &state.config())?
        .timeout(UPDATE_CHECK_TIMEOUT)
        .endpoints(vec![endpoint])?
        .build()?;
    let mut update = updater
        .check()
        .await?
        .ok_or_else(|| anyhow!("the signed update feed has no newer version"))?;

    if update.version != expected_version {
        return Err(anyhow!(
            "the signed update feed changed from expected version {expected_version} to {}",
            update.version
        ));
    }
    update.timeout = Some(UPDATE_DOWNLOAD_TIMEOUT);

    let progress_state = state.clone();
    let mut downloaded = 0_u64;
    let bytes = update
        .download(
            move |chunk, total| {
                downloaded = downloaded.saturating_add(chunk as u64);
                progress_state.set_desktop_update_progress(downloaded, total);
            },
            || {},
        )
        .await?;

    state.set_desktop_update_installing();
    ocg_core::process_log::diagnostic("info", "update", "installing signed desktop update");
    update.install(&bytes)?;
    state.set_desktop_update_completed();

    #[cfg(not(windows))]
    app.request_restart();

    Ok(())
}

/// Runs the signed updater from a minimal hidden app when startup is blocked
/// by a data directory written by a newer version: no database, tray, or
/// dashboard exists yet. Returns after the update flow finishes; the caller
/// then exits without building the normal app.
pub fn run_standalone_update(data_dir: &std::path::Path) {
    let Some(public_key) = embedded_public_key() else {
        // Unsigned builds cannot verify an update; go straight to the page.
        crate::startup_ui::open_release_page();
        return;
    };
    let config = ocg_core::db::peek_app_config(data_dir).unwrap_or_default();
    // The release build merges `plugins.updater.pubkey` into the config; this
    // standalone path must inject it itself or plugin init fails on null.
    let mut context = tauri::generate_context!();
    context.config_mut().plugins.0.insert(
        "updater".to_string(),
        serde_json::json!({ "pubkey": public_key }),
    );
    let application = tauri::Builder::default()
        .plugin(
            tauri_plugin_updater::Builder::new()
                .pubkey(public_key)
                .build(),
        )
        .setup(move |app| {
            let handle = app.handle().clone();
            let config = config.clone();
            tauri::async_runtime::spawn(async move {
                standalone_update_flow(&handle, &config).await;
                handle.exit(0);
            });
            Ok(())
        })
        .build(context);
    let application = match application {
        Ok(application) => application,
        Err(error) => {
            crate::startup_ui::show_update_error(&format!("{error:#}"));
            crate::startup_ui::open_release_page();
            return;
        }
    };
    application.run(|_, _| {});
}

async fn standalone_update_flow(app: &AppHandle, config: &AppConfig) {
    let updater = (|| -> crate::Result<tauri_plugin_updater::Updater> {
        let endpoint = UPDATE_ENDPOINT
            .parse()
            .context("invalid built-in updater endpoint")?;
        let builder = apply_updater_proxy(app.updater_builder(), config)?;
        builder
            .timeout(UPDATE_CHECK_TIMEOUT)
            .endpoints(vec![endpoint])
            .and_then(|builder| builder.build())
            .map_err(Into::into)
    })();
    let updater = match updater {
        Ok(updater) => updater,
        Err(error) => return fail_standalone_update(&format!("{error:#}")),
    };
    let update = match updater.check().await {
        Ok(Some(update)) => update,
        Ok(None) => {
            return fail_standalone_update("the signed update feed has no newer version");
        }
        Err(error) => return fail_standalone_update(&format!("{error:#}")),
    };
    if !crate::startup_ui::confirm_install_update(&update.version) {
        return;
    }
    let mut update = update;
    update.timeout = Some(UPDATE_DOWNLOAD_TIMEOUT);
    let bytes = match update.download(|_, _| {}, || {}).await {
        Ok(bytes) => bytes,
        Err(error) => return fail_standalone_update(&format!("{error:#}")),
    };
    if let Err(error) = update.install(&bytes) {
        fail_standalone_update(&format!("{error:#}"));
    }
    // The spawned installer waits for this process to exit; the caller exits now.
}

fn fail_standalone_update(error: &str) {
    crate::startup_ui::show_update_error(error);
    crate::startup_ui::open_release_page();
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum UpdaterProxySetting {
    FollowSystem,
    Manual(String),
    Disabled,
}

fn updater_proxy_setting(config: &AppConfig) -> UpdaterProxySetting {
    match config.proxy_mode {
        ProxyMode::Auto => UpdaterProxySetting::FollowSystem,
        ProxyMode::Manual => UpdaterProxySetting::Manual(config.proxy_url.clone()),
        ProxyMode::Direct => UpdaterProxySetting::Disabled,
        // Signed downloads are non-model-scoped traffic: they follow the
        // direction's default leg, matching `configured_builder`.
        ProxyMode::List => match config.proxy_list_direction {
            ProxyListDirection::Whitelist => UpdaterProxySetting::Disabled,
            ProxyListDirection::Blacklist => UpdaterProxySetting::Manual(config.proxy_url.clone()),
        },
    }
}

fn apply_updater_proxy(
    builder: UpdaterBuilder,
    config: &AppConfig,
) -> crate::Result<UpdaterBuilder> {
    match updater_proxy_setting(config) {
        UpdaterProxySetting::FollowSystem => Ok(builder),
        UpdaterProxySetting::Manual(url) => Ok(builder.proxy(
            url.parse()
                .context("invalid outbound proxy URL for signed desktop updates")?,
        )),
        UpdaterProxySetting::Disabled => Ok(builder.no_proxy()),
    }
}

fn embedded_public_key() -> Option<&'static str> {
    normalize_public_key(option_env!("TAURI_UPDATER_PUBLIC_KEY"))
}

fn normalize_public_key(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::{UpdaterProxySetting, normalize_public_key, updater_proxy_setting};
    use ocg_core::models::{AppConfig, ProxyListDirection, ProxyMode};

    #[test]
    fn updater_public_key_must_be_non_empty() {
        assert_eq!(normalize_public_key(None), None);
        assert_eq!(normalize_public_key(Some("  \r\n")), None);
        assert_eq!(
            normalize_public_key(Some("  public-key  ")),
            Some("public-key")
        );
    }

    #[test]
    fn updater_follows_the_process_wide_proxy_policy() {
        let mut config = AppConfig::default();
        assert_eq!(
            updater_proxy_setting(&config),
            UpdaterProxySetting::FollowSystem
        );

        config.proxy_mode = ProxyMode::Direct;
        assert_eq!(
            updater_proxy_setting(&config),
            UpdaterProxySetting::Disabled
        );

        config.proxy_mode = ProxyMode::Manual;
        config.proxy_url = "http://127.0.0.1:7890".to_string();
        assert_eq!(
            updater_proxy_setting(&config),
            UpdaterProxySetting::Manual("http://127.0.0.1:7890".to_string())
        );
    }

    #[test]
    fn updater_follows_the_list_mode_default_leg_per_direction() {
        let mut config = AppConfig {
            gateway_key: "k".to_string(),
            proxy_mode: ProxyMode::List,
            proxy_url: "http://127.0.0.1:7890".to_string(),
            ..AppConfig::default()
        };

        config.proxy_list_direction = ProxyListDirection::Whitelist;
        assert_eq!(
            updater_proxy_setting(&config),
            UpdaterProxySetting::Disabled,
            "whitelist default leg is direct"
        );

        config.proxy_list_direction = ProxyListDirection::Blacklist;
        assert_eq!(
            updater_proxy_setting(&config),
            UpdaterProxySetting::Manual("http://127.0.0.1:7890".to_string()),
            "blacklist default leg is the manual proxy"
        );
    }
}
