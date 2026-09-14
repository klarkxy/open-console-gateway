//! Listener-only Gateway lifecycle.
//!
//! [`GatewayLifecycle::bind`] owns TCP bind, dashboard trust, forward-log
//! backfill, and the HTTP server task. It does not start or cancel process-level
//! workers. [`GatewayLifecycle::stop`] is signal-only.
//! [`GatewayLifecycle::stop_and_wait`] signals shutdown and awaits the listener
//! task for up to [`LISTENER_STOP_WAIT`], aborting and joining it on timeout.
//! [`GatewayLifecycle::rebind`] serializes the complete slot-aware transition:
//! same-port stop-then-bind, new-port bind-first. Signal-only replacement from
//! a serving request keeps dashboard trust fail-closed, then a detached
//! observer awaits the old listener with bounded abort and restores local
//! mode only when the installed listener is loopback and no public listener
//! remains.

use crate::gateway_runtime::GatewayHandle;
use crate::state::CoreState;
use anyhow::Result;
use axum::Router;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::sync::oneshot;

/// Bound wait after a listener shutdown signal. Matches the existing desktop
/// restart helper so a same-port rebind can claim the TCP port.
pub const LISTENER_STOP_WAIT: Duration = Duration::from_secs(5);

pub struct GatewayLifecycle;

/// Supplies the complete HTTP router to the listener without making the
/// gateway depend on the dashboard composition modules.
///
/// The implementation belongs to the host composition root. Keeping this
/// contract at the listener boundary makes that dependency inversion explicit
/// instead of relying on an inherent method defined in another module.
pub(crate) trait GatewayRouterHost {
    fn compose_router(state: CoreState) -> Router;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ListenerStopOutcome {
    Graceful,
    AbortedAfterTimeout,
    TaskCancelled,
    TaskPanicked,
}

impl ListenerStopOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Graceful => "graceful",
            Self::AbortedAfterTimeout => "aborted_after_timeout",
            Self::TaskCancelled => "task_cancelled",
            Self::TaskPanicked => "task_panicked",
        }
    }
}

struct PreparedListener {
    listener: tokio::net::TcpListener,
    local_addr: SocketAddr,
}

struct PublicListenerRegistration(CoreState);

impl PublicListenerRegistration {
    fn new(state: CoreState) -> Self {
        state.register_dashboard_public_listener();
        Self(state)
    }
}

impl Drop for PublicListenerRegistration {
    fn drop(&mut self) {
        if self.0.unregister_dashboard_public_listener() {
            GatewayLifecycle::schedule_dashboard_trust_recompute(self.0.clone());
        }
    }
}

impl GatewayLifecycle {
    pub async fn bind(state: CoreState, addr: SocketAddr) -> Result<GatewayHandle> {
        let _lifecycle = state.lock_gateway_lifecycle().await;
        Self::repair_active_dashboard_trust(&state);
        let prepared = match Self::prepare(addr).await {
            Ok(prepared) => prepared,
            Err(error) => {
                state.log_runtime_event(
                    "error",
                    "gateway",
                    &format!("event=listener_bind_failed address={addr} stage=initial"),
                );
                return Err(error);
            }
        };
        let dashboard_is_local = prepared.local_addr.ip().is_loopback();

        // A directly bound initial listener must have the right trust before
        // it can accept a request. If another public listener is installed,
        // an independently bound loopback listener may not make the shared
        // mode permissive; callers replacing an installed listener must use
        // `rebind` so trust can be promoted after verified shutdown.
        let can_enable_local = !state.has_dashboard_public_listener()
            && state
                .gateway
                .lock()
                .as_ref()
                .is_none_or(|handle| handle.dashboard_is_local);
        state.set_dashboard_local_mode(dashboard_is_local && can_enable_local);
        Ok(Self::spawn_prepared(state.clone(), prepared))
    }

    async fn prepare(addr: SocketAddr) -> Result<PreparedListener> {
        let listener = tokio::net::TcpListener::bind(addr).await?;
        let local_addr = listener.local_addr()?;
        Ok(PreparedListener {
            listener,
            local_addr,
        })
    }

    fn spawn_prepared(state: CoreState, prepared: PreparedListener) -> GatewayHandle {
        let PreparedListener {
            listener,
            local_addr,
        } = prepared;
        let dashboard_is_local = local_addr.ip().is_loopback();
        let public_registration =
            (!dashboard_is_local).then(|| PublicListenerRegistration::new(state.clone()));
        spawn_forward_log_backfill(state.clone());
        // Composed by the host root through the explicit listener boundary so
        // this module does not import dashboard mounts.
        let app = <CoreState as GatewayRouterHost>::compose_router(state.clone());
        let port = local_addr.port();

        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

        let task_state = state.clone();
        let handle = tokio::spawn(async move {
            let _public_registration = public_registration;
            let server = axum::serve(listener, app);
            let server = server.with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            });
            if let Err(e) = server.await {
                eprintln!("gateway server error: {e}");
                task_state.log_runtime_event(
                    "error",
                    "gateway",
                    &format!("event=listener_task_failed address={local_addr}"),
                );
            }
        });

        state.log_runtime_event(
            "info",
            "gateway",
            &format!(
                "event=listener_bound address={local_addr} dashboard_local={dashboard_is_local}"
            ),
        );

        GatewayHandle {
            port,
            listen_addr: local_addr,
            dashboard_is_local,
            shutdown: shutdown_tx,
            task: handle,
        }
    }

    pub fn stop(handle: GatewayHandle) {
        let _ = handle.shutdown.send(());
        // Important: don't block_on the JoinHandle — stop_gateway is called from
        // tokio runtime contexts and ExitRequested handlers.
        // The spawned task will exit when the graceful-shutdown future resolves.
        // Await [`Self::stop_and_wait`] from an async context when the caller
        // needs the port released before binding again.
    }

    /// Signal graceful shutdown and wait up to [`LISTENER_STOP_WAIT`] for the
    /// listener task to exit. A timeout aborts the listener task and awaits the
    /// abort before returning, so callers never leave a detached listener that
    /// could retain a public socket.
    pub async fn stop_and_wait(handle: GatewayHandle) -> ListenerStopOutcome {
        Self::stop_and_wait_for(handle, LISTENER_STOP_WAIT).await
    }

    /// Same termination contract as [`Self::stop_and_wait`] with a caller-
    /// supplied bound. Kept public so lifecycle behavior can be verified with
    /// a short deterministic timeout.
    #[doc(hidden)]
    pub async fn stop_and_wait_for(handle: GatewayHandle, wait: Duration) -> ListenerStopOutcome {
        let GatewayHandle {
            shutdown, mut task, ..
        } = handle;
        let _ = shutdown.send(());
        match tokio::time::timeout(wait, &mut task).await {
            Ok(Ok(())) => ListenerStopOutcome::Graceful,
            Ok(Err(error)) if error.is_cancelled() => ListenerStopOutcome::TaskCancelled,
            Ok(Err(error)) => {
                eprintln!("gateway listener task failed during shutdown: {error}");
                ListenerStopOutcome::TaskPanicked
            }
            Err(_) => {
                task.abort();
                match task.await {
                    Ok(()) => {}
                    Err(error) if error.is_cancelled() => {}
                    Err(error) => {
                        eprintln!("gateway listener task failed after shutdown timeout: {error}");
                    }
                }
                ListenerStopOutcome::AbortedAfterTimeout
            }
        }
    }

    /// Rebind the listener stored in `state.gateway`.
    ///
    /// Same port (requested port != 0 and equal to the live handle port): take
    /// the current handle, [`Self::stop_and_wait`], then bind. A same-port bind
    /// error cannot restore the previous listener; the slot stays empty because
    /// the port had to be released first.
    ///
    /// Different port, including requested port 0: bind the new address first;
    /// only stop the old listener after the new bind succeeds. Port 0 is never
    /// a same-port rebind (a live handle reports the assigned port). A failed
    /// new-port bind leaves the old handle in the slot, so `active_gateway_port`
    /// and the live listener stay unchanged.
    ///
    /// Listener-only: does not start, cancel, reset, or duplicate process-level
    /// workers, and does not mutate settings, config, revision, routing, or
    /// desktop/updater state.
    pub async fn rebind(state: CoreState, addr: SocketAddr) -> Result<u16> {
        Self::rebind_inner(state, addr, true).await
    }

    /// Bind-first replacement that signals the previous listener to stop
    /// without waiting for it. HTTP settings handlers must use this when the
    /// request is being served by the listener that will be replaced;
    /// awaiting that listener from inside the request would deadlock.
    pub async fn rebind_from_serving_request(state: CoreState, addr: SocketAddr) -> Result<u16> {
        Self::rebind_inner(state, addr, false).await
    }

    async fn rebind_inner(state: CoreState, addr: SocketAddr, wait_for_old: bool) -> Result<u16> {
        let _lifecycle = state.lock_gateway_lifecycle().await;
        Self::repair_active_dashboard_trust(&state);

        let requested_port = addr.port();
        let same_port = {
            let slot = state.gateway.lock();
            requested_port != 0 && slot.as_ref().map(|handle| handle.port) == Some(requested_port)
        };

        if same_port {
            anyhow::ensure!(
                wait_for_old,
                "same-port rebind must wait for the previous listener to release the port"
            );
            let old_handle = state
                .gateway
                .lock()
                .take()
                .expect("same-port classification requires an installed listener");
            if !addr.ip().is_loopback() || !old_handle.dashboard_is_local {
                state.set_dashboard_local_mode(false);
            }
            let outcome = Self::stop_and_wait(old_handle).await;

            // Same-port replacement cannot bind until the old task has
            // definitively terminated. A bind failure is therefore honest:
            // the slot remains empty and no listener is detached.
            let prepared = match Self::prepare(addr).await {
                Ok(prepared) => prepared,
                Err(error) => {
                    state.log_runtime_event(
                        "error",
                        "gateway",
                        &format!(
                            "event=listener_rebind_failed previous_port={requested_port} target_port={requested_port} mode=same_port stop_outcome={} stage=bind",
                            outcome.as_str()
                        ),
                    );
                    return Err(error);
                }
            };
            let dashboard_is_local = prepared.local_addr.ip().is_loopback();
            state.set_dashboard_local_mode(
                dashboard_is_local && !state.has_dashboard_public_listener(),
            );
            let handle = Self::spawn_prepared(state.clone(), prepared);
            let port = handle.port;
            let displaced = state.gateway.lock().replace(handle);
            debug_assert!(displaced.is_none());
            state.log_runtime_event(
                "info",
                "gateway",
                &format!(
                    "event=listener_rebound previous_port={requested_port} new_port={port} mode=same_port stop_outcome={} wait_for_previous=true",
                    outcome.as_str()
                ),
            );
            return Ok(port);
        }

        // Different-port and port-0 transitions bind before touching the live
        // slot. A failed bind therefore preserves both the old listener and
        // its trust mode.
        let prepared = match Self::prepare(addr).await {
            Ok(prepared) => prepared,
            Err(error) => {
                let previous_port = state.gateway.lock().as_ref().map(|handle| handle.port);
                state.log_runtime_event(
                    "error",
                    "gateway",
                    &format!(
                        "event=listener_rebind_failed previous_port={} target_port={requested_port} mode=bind_first stage=bind",
                        previous_port
                            .map(|port| port.to_string())
                            .unwrap_or_else(|| "none".to_string())
                    ),
                );
                return Err(error);
            }
        };
        let dashboard_is_local = prepared.local_addr.ip().is_loopback();
        let had_old_listener = state.gateway.lock().is_some();
        if !dashboard_is_local {
            // Fail closed before a public listener can accept a request. This
            // temporarily makes an old loopback listener require a session.
            state.set_dashboard_local_mode(false);
        } else if !had_old_listener && !state.has_dashboard_public_listener() {
            state.set_dashboard_local_mode(true);
        }
        let handle = Self::spawn_prepared(state.clone(), prepared);
        let port = handle.port;
        let old_handle = state.gateway.lock().replace(handle);
        let previous_port = old_handle.as_ref().map(|handle| handle.port);
        let mut stop_outcome = None;
        if let Some(handle) = old_handle {
            if wait_for_old {
                stop_outcome = Some(Self::stop_and_wait(handle).await);
            } else {
                Self::observe_displaced_listener(state.clone(), handle);
            }
        }
        // A loopback listener becomes trusted only after every replaced public
        // listener is confirmed stopped (gracefully or by an awaited abort).
        // Signal-only replacement from a serving request cannot wait; keep
        // fail-closed while a public listener may still be draining. The
        // detached observer restores trust once the installed listener is
        // loopback and the public count is zero.
        if wait_for_old {
            state.set_dashboard_local_mode(
                dashboard_is_local && !state.has_dashboard_public_listener(),
            );
        } else if !dashboard_is_local {
            state.set_dashboard_local_mode(false);
        }
        state.log_runtime_event(
            "info",
            "gateway",
            &format!(
                "event=listener_rebound previous_port={} new_port={port} mode=bind_first stop_outcome={} wait_for_previous={wait_for_old}",
                previous_port
                    .map(|port| port.to_string())
                    .unwrap_or_else(|| "none".to_string()),
                stop_outcome
                    .map(ListenerStopOutcome::as_str)
                    .unwrap_or("pending")
            ),
        );
        Ok(port)
    }

    /// Signal the displaced listener and wait with the same bounded abort as
    /// [`Self::stop_and_wait`]. Trust is recomputed only after that wait under
    /// the lifecycle gate.
    fn observe_displaced_listener(state: CoreState, handle: GatewayHandle) {
        std::mem::drop(tokio::spawn(async move {
            let _outcome = Self::stop_and_wait(handle).await;
            let _lifecycle = state.lock_gateway_lifecycle().await;
            Self::recompute_dashboard_trust(&state);
        }));
    }

    /// A public-listener registration is owned by the listener task, so its
    /// final unregister runs from `Drop` and cannot await the lifecycle mutex.
    /// Defer the recomputation without retaining any synchronous lock. If the
    /// runtime is already gone, leaving trust fail-closed is the safe shutdown
    /// behavior.
    fn schedule_dashboard_trust_recompute(state: CoreState) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        std::mem::drop(runtime.spawn(async move {
            let _lifecycle = state.lock_gateway_lifecycle().await;
            Self::recompute_dashboard_trust(&state);
        }));
    }

    /// Recompute from lifecycle-owned facts. The lifecycle gate prevents a
    /// new independent public bind from registering between the count check
    /// and the mode write.
    fn recompute_dashboard_trust(state: &CoreState) {
        let installed_is_loopback = {
            let slot = state.gateway.lock();
            slot.as_ref()
                .is_some_and(|handle| handle.dashboard_is_local)
        };
        state.set_dashboard_local_mode(
            installed_is_loopback && !state.has_dashboard_public_listener(),
        );
    }

    fn repair_active_dashboard_trust(state: &CoreState) {
        let installed_public_listener = {
            let slot = state.gateway.lock();
            slot.as_ref()
                .is_some_and(|handle| !handle.dashboard_is_local)
        };
        if installed_public_listener || state.has_dashboard_public_listener() {
            state.set_dashboard_local_mode(false);
        }
    }
}

/// Attributes pre-multi-key forward logs to the primary key in bounded
/// chunks. The runtime context (not `CoreStateInner` construction) owns this
/// so pure synchronous construction never starts it. Small tables finish
/// inline; a dedicated thread (holding only a weak state reference) continues
/// large ones chunk by chunk so request logging never waits behind more than
/// one short transaction. DB work is synchronous, so no lock is ever held
/// across an await point.
fn spawn_forward_log_backfill(state: CoreState) {
    // The primary key attributes under its fixed hardcoded id (see
    // `gateway_keys::PRIMARY_KEY_ID`); no config or db lock is needed.
    let (key_id, key_name) = (
        crate::kernel::ids::PRIMARY_KEY_ID.to_string(),
        crate::kernel::ids::PRIMARY_KEY_NAME.to_string(),
    );
    // Fast path: one bounded step inline. Fresh databases (and tests) have no
    // NULL rows, so this records the completion marker and never spawns.
    let more_chunks = {
        let db = state.db.lock();
        db.backfill_forward_logs_client_key_step(
            &key_id,
            &key_name,
            crate::db::FORWARD_LOG_BACKFILL_CHUNK_ROWS,
        )
    };
    match more_chunks {
        Ok(true) => state.log_runtime_event(
            "info",
            "observability",
            "event=forward_log_key_backfill_started mode=background",
        ),
        Ok(false) => return,
        Err(error) => {
            eprintln!("warning: forward log key backfill unavailable: {error}");
            state.log_runtime_event(
                "warn",
                "observability",
                "event=forward_log_key_backfill_failed phase=initial",
            );
            return;
        }
    }
    let weak = std::sync::Arc::downgrade(&state);
    let spawn_result = std::thread::Builder::new()
        .name("ocg-forward-log-backfill".to_string())
        .spawn(move || {
            while let Some(state) = weak.upgrade() {
                let step = {
                    let db = state.db.lock();
                    db.backfill_forward_logs_client_key_step(
                        &key_id,
                        &key_name,
                        crate::db::FORWARD_LOG_BACKFILL_CHUNK_ROWS,
                    )
                };
                match step {
                    Ok(true) => std::thread::sleep(crate::db::FORWARD_LOG_BACKFILL_CHUNK_PAUSE),
                    Ok(false) => {
                        state.log_runtime_event(
                            "info",
                            "observability",
                            "event=forward_log_key_backfill_completed mode=background",
                        );
                        return;
                    }
                    Err(error) => {
                        eprintln!("warning: forward log key backfill paused: {error}");
                        state.log_runtime_event(
                            "warn",
                            "observability",
                            "event=forward_log_key_backfill_failed phase=background",
                        );
                        return;
                    }
                }
            }
        });
    if let Err(error) = spawn_result {
        eprintln!("warning: forward log key backfill thread failed to start: {error}");
        state.log_runtime_event(
            "warn",
            "observability",
            "event=forward_log_key_backfill_failed phase=thread_spawn",
        );
    }
}
