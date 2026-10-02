//! Gateway Lifecycle host capability.
//!
//! Binds through `ocg_core::gateway::start_gateway` (listener + process-level
//! usage-sync workers) and stops with a signal-only `stop_gateway`. Settings
//! port changes rebind through CoreState / GatewayLifecycle, not this module.

use ocg_core::gateway;
use ocg_core::state::CoreState;

pub fn start_on_configured_port(core: &CoreState) -> anyhow::Result<()> {
    let port = core.settings_config().gateway_port;
    let gateway_state = core.clone();
    match tauri::async_runtime::block_on(gateway::start_gateway(gateway_state, port)) {
        Ok(handle) => {
            let bound = handle.port;
            *core.gateway.lock() = Some(handle);
            core.log_runtime_event(
                "info",
                "gateway",
                &format!("gateway started on port {bound}"),
            );
            Ok(())
        }
        Err(error) => {
            tracing::error!("failed to start Gateway on 127.0.0.1:{port}: {error}");
            core.log_runtime_event("error", "gateway", "failed to start gateway");
            Err(error)
        }
    }
}

pub fn stop_listener(core: &CoreState) {
    if let Some(handle) = core.gateway.lock().take() {
        gateway::stop_gateway(handle);
        core.clear_gateway_error();
    }
}

#[cfg(test)]
mod tests;
