//! Process-owned desktop host capabilities.
//!
//! These seams are not WebView `invoke` commands. The dashboard talks HTTP.
//!
//! - Native Browser: Chromium-family process helpers registered into CoreState
//! - Gateway Lifecycle: bind/stop via ocg-core GatewayLifecycle
//! - Desktop Settings: auto-start and Dock visibility hooks
//! - Updater: signed updates registered as a CoreState starter, never a command

pub mod cpa_runtime;
pub mod gateway;

use crate::native_browser;
use crate::state::BrowserProcessState;
use ocg_core::state::CoreState;
use parking_lot::Mutex;
use std::sync::Arc;

pub fn register_native_browser(
    core: &CoreState,
    browser_processes: Arc<Mutex<BrowserProcessState>>,
) {
    match native_browser::native_browser_name() {
        Ok(browser_name) => {
            let launcher_data_dir = core.data_dir();
            let launcher_processes = browser_processes.clone();
            let launcher: ocg_core::browser::NativeBrowserLauncher =
                Arc::new(move |account_id, url| {
                    native_browser::open_external_browser(
                        launcher_data_dir.clone(),
                        launcher_processes.clone(),
                        account_id,
                        url,
                    )
                    .map(|_| ())
                    .map_err(anyhow::Error::msg)
                });
            let stopper_processes = browser_processes.clone();
            let stopper_data_dir = core.data_dir();
            let stopper: ocg_core::browser::NativeBrowserStopper = Arc::new(move |account_id| {
                native_browser::stop_external_browser(
                    &stopper_processes,
                    account_id,
                    Some(&stopper_data_dir),
                )
                .map_err(anyhow::Error::msg)
            });
            if let Err(error) = core.browser.register_native_hooks(launcher, stopper) {
                let _ = core.db.lock().log_gateway(
                    "warn",
                    "browser",
                    &format!("failed to register native browser hooks: {error}"),
                );
            } else {
                let _ = core.db.lock().log_gateway(
                    "info",
                    "browser",
                    &format!("native browser available: {browser_name}"),
                );
            }
        }
        Err(reason) => {
            if let Err(error) = core
                .browser
                .register_native_unavailable_reason(reason.clone())
            {
                let _ = core.db.lock().log_gateway(
                    "warn",
                    "browser",
                    &format!("failed to register native browser availability: {error}"),
                );
            }
            let _ = core.db.lock().log_gateway("warn", "browser", &reason);
        }
    }
}

pub fn close_native_browsers(
    processes: &Arc<Mutex<BrowserProcessState>>,
    data_dir: &std::path::Path,
) {
    let _ = native_browser::close_all_browser_processes(processes, Some(data_dir));
}

#[allow(unused_variables)]
pub fn register_desktop_settings(core: &CoreState) {
    #[cfg(all(
        not(debug_assertions),
        any(
            all(windows, target_arch = "x86_64"),
            target_os = "macos",
            all(target_os = "linux", target_arch = "x86_64"),
        )
    ))]
    {
        core.set_auto_start_sync(crate::autostart::sync);
        if let Err(e) = core.sync_auto_start(core.config().auto_start) {
            let _ = core.db.lock().log_gateway(
                "warn",
                "startup",
                &format!("failed to synchronize auto-start: {e}"),
            );
        }
    }
}

#[allow(unused_variables)]
pub fn register_dock_visibility(core: &CoreState, app: &tauri::App) {
    #[cfg(target_os = "macos")]
    {
        let app_handle = app.handle().clone();
        core.set_dock_visibility_sync(Arc::new(move |visible| {
            app_handle
                .set_dock_visibility(visible)
                .map_err(anyhow::Error::from)
        }));
        if let Err(error) = core.sync_dock_visibility(core.config().show_dock_icon) {
            let _ = core.db.lock().log_gateway(
                "warn",
                "startup",
                &format!("failed to synchronize Dock visibility: {error}"),
            );
        }
    }
}
