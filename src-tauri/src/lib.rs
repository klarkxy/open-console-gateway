pub mod autostart;
pub mod host;
pub mod native_browser;
mod startup_recovery;
mod startup_ui;
pub mod state;
pub mod tray;
pub mod updater;

pub type Result<T> = anyhow::Result<T>;

use ocg_core::crypto::KeyCipher;
#[cfg(windows)]
use ocg_core::crypto::MachineBoundCipher;
#[cfg(not(windows))]
use ocg_core::crypto::load_or_create_static_cipher;
use ocg_core::db::Database;
use ocg_core::state::CoreStateInner;
use parking_lot::Mutex;
use state::{BrowserProcessState, GuiState};
use std::path::PathBuf;
use std::sync::Arc;
use tauri::Manager;

const GATEWAY_PORT_ENV: &str = "OCG_GATEWAY_PORT";

pub fn run() {
    ocg_core::cpa_runtime::host::run_internal_supervisor_if_requested();
    let application = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            if !args.iter().any(|arg| arg == "--startup")
                && app.try_state::<state::AppState>().is_some()
            {
                if let Err(error) = tray::setup_tray(app) {
                    startup_ui::show_startup_error(&error.to_string());
                    return;
                }
                tray::open_dashboard(app);
            }
        }))
        .plugin(tauri_plugin_shell::init())
        .setup(|app| {
            // Plugin setup (including single-instance ownership) precedes this
            // callback, so a secondary process never opens data or binds ports.
            let mut app_state = initialize_host()?;
            if startup_recovery::prepare(&app_state.core)? {
                // The old CLI could have written while its recovery prompt was
                // open. Reopen persisted state after it exits, never serve a
                // snapshot captured before user consent.
                drop(app_state);
                app_state = initialize_host()?;
            }
            let core = &app_state.core;
            if let Ok(resource_dir) = app.path().resource_dir() {
                core.set_dashboard_dir(Some(resource_dir.join("dist")));
            }
            app.manage(app_state.clone());
            tray::setup_tray(app.handle())?;
            host::register_dock_visibility(core, app);
            updater::configure(app.handle(), core.clone())?;
            host::gateway::start_on_configured_port(core)?;
            if !autostart::is_startup_launch() {
                tray::open_dashboard(app.handle());
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                window.hide().ok();
                api.prevent_close();
            }
        })
        .build(tauri::generate_context!());
    let application = match application {
        Ok(application) => application,
        Err(error) => {
            startup_ui::show_startup_error(&error.to_string());
            return;
        }
    };
    application.run(|app, event| {
        if let tauri::RunEvent::ExitRequested { .. } = event
            && let Some(state) = app.try_state::<state::AppState>()
        {
            let core = &state.core;
            host::close_native_browsers(&state.browser_processes, &core.data_dir());
            host::cpa_runtime::stop_on_exit(core);
            host::gateway::stop_listener(core);
            let _ = core
                .db
                .lock()
                .log_gateway("info", "gateway", "application exiting");
        }
    });
}

fn initialize_host() -> Result<state::AppState> {
    let data_dir = data_dir();
    let cipher = load_cipher(&data_dir)?;
    let db = Database::open_with_cipher(data_dir.clone(), cipher.clone())?;
    let core_state = Arc::new(CoreStateInner::new(db, data_dir.clone(), cipher.clone())?);
    if let Some(port) = gateway_port_override_from_env()? {
        core_state.register_gateway_port_override(port)?;
    }

    host::register_desktop_settings(&core_state);
    host::cpa_runtime::register(&core_state);

    let browser_processes = Arc::new(Mutex::new(BrowserProcessState::default()));
    host::register_native_browser(&core_state, browser_processes.clone());

    Ok(Arc::new(GuiState {
        core: core_state.clone(),
        browser_processes,
    }))
}

fn gateway_port_override_from_env() -> Result<Option<u16>> {
    match std::env::var(GATEWAY_PORT_ENV) {
        Ok(value) => parse_gateway_port_override(&value),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(anyhow::anyhow!(
            "{GATEWAY_PORT_ENV} must contain valid Unicode"
        )),
    }
}

fn parse_gateway_port_override(value: &str) -> Result<Option<u16>> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    value
        .parse::<u16>()
        .ok()
        .filter(|port| *port > 0)
        .map(Some)
        .ok_or_else(|| anyhow::anyhow!("{GATEWAY_PORT_ENV} must be an integer from 1 to 65535"))
}

fn load_cipher(data_dir: &std::path::Path) -> Result<Arc<dyn KeyCipher + Send + Sync>> {
    #[cfg(windows)]
    {
        let _ = data_dir;
        Ok(Arc::new(MachineBoundCipher::new()))
    }
    #[cfg(not(windows))]
    {
        Ok(Arc::new(load_or_create_static_cipher(data_dir)?))
    }
}

fn data_dir() -> PathBuf {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    home.join(".ocg-mgr")
}
