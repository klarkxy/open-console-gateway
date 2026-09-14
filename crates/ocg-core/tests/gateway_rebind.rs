//! Core listener rebind: same-port stop-then-bind, new-port bind-first,
//! and failed new-port bind leaving the old Gateway serving.

use ocg_core::crypto::{KeyCipher, StaticKeyCipher};
use ocg_core::db::Database;
use ocg_core::gateway::{GatewayLifecycle, ListenerStopOutcome};
use ocg_core::state::{CoreState, CoreStateInner, GatewayHandle};
use std::fs;
use std::net::{SocketAddr, TcpListener as StdTcpListener};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
#[cfg(not(windows))]
use tokio::io::{AsyncReadExt, AsyncWriteExt};
#[cfg(not(windows))]
use tokio::sync::Barrier;
use tokio::sync::oneshot;

struct DropSignal(Arc<AtomicBool>);

impl Drop for DropSignal {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

fn temp_state(label: &str) -> (PathBuf, CoreState) {
    let dir = std::env::temp_dir().join(format!(
        "ocg-gateway-rebind-{label}-{}",
        uuid::Uuid::new_v4()
    ));
    fs::create_dir_all(&dir).unwrap();
    let db = Database::open(dir.clone()).unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("rebind-test"));
    let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());
    (dir, state)
}

fn loopback(port: u16) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], port))
}

#[cfg(not(windows))]
fn public(port: u16) -> SocketAddr {
    SocketAddr::from(([0, 0, 0, 0], port))
}

fn loopback_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .connect_timeout(Duration::from_secs(1))
        .timeout(Duration::from_secs(2))
        .build()
        .expect("loopback client should build")
}

async fn install_listener(state: &CoreState) -> u16 {
    let handle = GatewayLifecycle::bind(state.clone(), loopback(0))
        .await
        .expect("ephemeral listener should bind");
    let port = handle.port;
    assert_ne!(port, 0);
    *state.gateway.lock() = Some(handle);
    port
}

/// Installs a real listener whose shutdown future pauses on a barrier after
/// observing the lifecycle signal. This keeps the old socket serving without
/// sleeps, making the public-to-loopback trust transition deterministic.
#[cfg(not(windows))]
async fn install_held_listener(
    state: &CoreState,
    addr: SocketAddr,
) -> (u16, oneshot::Receiver<()>, Arc<Barrier>) {
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("held listener should bind");
    let local_addr = listener.local_addr().expect("held listener local address");
    let dashboard_is_local = local_addr.ip().is_loopback();
    state.set_dashboard_local_mode(dashboard_is_local);
    let app = ocg_core::host_router::build_router(state.clone());
    let port = local_addr.port();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let (shutdown_seen_tx, shutdown_seen_rx) = oneshot::channel();
    let release = Arc::new(Barrier::new(2));
    let server_release = release.clone();
    let task = tokio::spawn(async move {
        let server = axum::serve(listener, app).with_graceful_shutdown(async move {
            let _ = shutdown_rx.await;
            let _ = shutdown_seen_tx.send(());
            server_release.wait().await;
        });
        if let Err(error) = server.await {
            panic!("held gateway server failed: {error}");
        }
    });
    *state.gateway.lock() = Some(GatewayHandle {
        port,
        listen_addr: loopback(port),
        dashboard_is_local,
        shutdown: shutdown_tx,
        task,
    });
    (port, shutdown_seen_rx, release)
}

async fn shutdown(state: &CoreState) {
    let handle = state.gateway.lock().take();
    if let Some(handle) = handle {
        GatewayLifecycle::stop_and_wait(handle).await;
    }
}

async fn assert_serving(port: u16, key: &str) {
    let response = loopback_client()
        .get(format!("http://127.0.0.1:{port}/v1/models"))
        .bearer_auth(key)
        .send()
        .await
        .unwrap_or_else(|error| panic!("gateway on {port} should serve: {error}"));
    assert_eq!(
        response.status(),
        reqwest::StatusCode::OK,
        "gateway on {port} should accept an authenticated models list"
    );
}

async fn assert_not_serving(port: u16, key: &str) {
    let result = loopback_client()
        .get(format!("http://127.0.0.1:{port}/v1/models"))
        .bearer_auth(key)
        .send()
        .await;
    assert!(
        result.is_err(),
        "listener on {port} must not still be serving"
    );
}

#[cfg(not(windows))]
async fn assert_dashboard_auth(port: u16, expected: reqwest::StatusCode) {
    let client = loopback_client();
    for path in ["/dashboard/api/v3/contract"] {
        let response = client
            .get(format!("http://127.0.0.1:{port}{path}"))
            .send()
            .await
            .unwrap_or_else(|error| panic!("dashboard request to {path} failed: {error}"));
        assert_eq!(
            response.status(),
            expected,
            "{path} on listener {port} returned the wrong auth status"
        );
    }
}

async fn cleanup(dir: PathBuf, state: CoreState) {
    shutdown(&state).await;
    drop(state);
    let _ = fs::remove_dir_all(dir);
}

#[tokio::test]
async fn same_port_rebind_stops_then_binds() {
    let (dir, state) = temp_state("same-port");
    let key = state.config().gateway_key.clone();
    let revision = state.settings_revision();
    let generation = state.process_generation();
    let configured_port = state.config().gateway_port;

    let port = install_listener(&state).await;
    assert_eq!(state.active_gateway_port(), port);
    assert_serving(port, &key).await;

    let rebound = GatewayLifecycle::rebind(state.clone(), loopback(port))
        .await
        .expect("same-port rebind must stop the old listener before binding");
    assert_eq!(rebound, port);
    assert_eq!(state.active_gateway_port(), port);
    assert_serving(port, &key).await;
    assert_eq!(state.settings_revision(), revision);
    assert_eq!(state.process_generation(), generation);
    assert_eq!(state.config().gateway_port, configured_port);
    assert_eq!(state.config().gateway_key, key);

    cleanup(dir, state).await;
}

#[tokio::test]
async fn new_port_rebind_binds_first_updates_active_port_and_stops_old() {
    let (dir, state) = temp_state("new-port");
    let key = state.config().gateway_key.clone();
    let revision = state.settings_revision();
    let generation = state.process_generation();
    let configured_port = state.config().gateway_port;

    let old_port = install_listener(&state).await;
    assert_eq!(state.active_gateway_port(), old_port);
    assert_serving(old_port, &key).await;
    let upstream_base_url = state.config().upstream_base_url.clone();

    // Port 0 is never the live handle port, so rebind takes the bind-first path.
    let new_port = GatewayLifecycle::rebind(state.clone(), loopback(0))
        .await
        .expect("new-port rebind should bind before stopping the old listener");
    assert_ne!(new_port, old_port);
    assert_eq!(state.active_gateway_port(), new_port);
    assert_serving(new_port, &key).await;
    assert_not_serving(old_port, &key).await;
    assert_eq!(state.settings_revision(), revision);
    assert_eq!(state.process_generation(), generation);
    assert_eq!(state.config().gateway_port, configured_port);
    assert_eq!(state.config().gateway_key, key);

    shutdown(&state).await;
    assert!(state.gateway.lock().is_none());
    assert_eq!(state.settings_revision(), revision);
    assert_eq!(state.process_generation(), generation);
    assert_eq!(state.config().gateway_port, configured_port);
    assert_eq!(state.config().gateway_key, key);
    assert_eq!(state.config().upstream_base_url, upstream_base_url);

    cleanup(dir, state).await;
}

#[tokio::test]
async fn failed_new_port_bind_keeps_old_listener_and_active_port() {
    let (dir, state) = temp_state("failed-new-port");
    let key = state.config().gateway_key.clone();
    let revision = state.settings_revision();
    let generation = state.process_generation();
    let configured_port = state.config().gateway_port;

    let old_port = install_listener(&state).await;
    assert_eq!(state.active_gateway_port(), old_port);
    assert_serving(old_port, &key).await;

    let occupied = StdTcpListener::bind(loopback(0)).expect("occupied listener should bind");
    let occupied_port = occupied.local_addr().expect("occupied port").port();
    assert_ne!(occupied_port, old_port);

    let error = GatewayLifecycle::rebind(state.clone(), loopback(occupied_port))
        .await
        .expect_err("bind to an occupied new port must fail");
    drop(error);
    drop(occupied);

    assert_eq!(state.active_gateway_port(), old_port);
    assert_serving(old_port, &key).await;
    assert_eq!(state.settings_revision(), revision);
    assert_eq!(state.process_generation(), generation);
    assert_eq!(state.config().gateway_port, configured_port);
    assert_eq!(state.config().gateway_key, key);
    assert!(
        state.gateway.lock().is_some(),
        "failed new-port bind must leave the old handle in the slot"
    );

    cleanup(dir, state).await;
}

#[tokio::test]
#[cfg(not(windows))]
async fn public_to_loopback_keeps_old_public_listener_authenticated_until_stopped() {
    let (dir, state) = temp_state("public-to-loopback-auth");
    let (old_port, shutdown_seen, release) = install_held_listener(&state, public(0)).await;
    assert!(!state.dashboard_local_mode());
    assert_dashboard_auth(old_port, reqwest::StatusCode::UNAUTHORIZED).await;

    let rebind_state = state.clone();
    let rebind =
        tokio::spawn(async move { GatewayLifecycle::rebind(rebind_state, loopback(0)).await });
    tokio::time::timeout(Duration::from_secs(2), shutdown_seen)
        .await
        .expect("rebind should signal the old listener")
        .expect("shutdown observation channel should stay open");

    // The new loopback socket is installed, but shared local trust must remain
    // disabled while the held public listener is still reachable.
    assert!(!state.dashboard_local_mode());
    assert_dashboard_auth(old_port, reqwest::StatusCode::UNAUTHORIZED).await;

    release.wait().await;
    let new_port = rebind
        .await
        .expect("rebind task should finish")
        .expect("public-to-loopback rebind should succeed");
    assert!(state.dashboard_local_mode());
    assert_eq!(state.active_gateway_port(), new_port);
    assert_dashboard_auth(new_port, reqwest::StatusCode::OK).await;
    assert_not_serving(old_port, &state.config().gateway_key).await;

    cleanup(dir, state).await;
}

#[tokio::test]
#[cfg(not(windows))]
async fn loopback_to_public_requires_v2_and_v3_auth_on_final_listener() {
    let (dir, state) = temp_state("loopback-to-public-auth");
    let old_port = install_listener(&state).await;
    assert!(state.dashboard_local_mode());
    assert_dashboard_auth(old_port, reqwest::StatusCode::OK).await;

    let new_port = GatewayLifecycle::rebind(state.clone(), public(0))
        .await
        .expect("loopback-to-public rebind should succeed");
    assert_ne!(new_port, old_port);
    assert!(!state.dashboard_local_mode());
    assert_eq!(state.active_gateway_port(), new_port);
    assert_dashboard_auth(new_port, reqwest::StatusCode::UNAUTHORIZED).await;
    assert_not_serving(old_port, &state.config().gateway_key).await;

    cleanup(dir, state).await;
}

#[tokio::test]
#[cfg(not(windows))]
async fn failed_public_bind_preserves_old_loopback_listener_and_local_trust() {
    let (dir, state) = temp_state("failed-public-auth");
    let old_port = install_listener(&state).await;
    assert!(state.dashboard_local_mode());
    assert_dashboard_auth(old_port, reqwest::StatusCode::OK).await;

    let occupied = StdTcpListener::bind(public(0)).expect("occupied public listener should bind");
    let occupied_port = occupied.local_addr().expect("occupied port").port();
    GatewayLifecycle::rebind(state.clone(), public(occupied_port))
        .await
        .expect_err("occupied public bind must fail");

    assert_eq!(state.active_gateway_port(), old_port);
    assert!(state.dashboard_local_mode());
    assert_dashboard_auth(old_port, reqwest::StatusCode::OK).await;

    drop(occupied);
    cleanup(dir, state).await;
}

#[tokio::test(flavor = "current_thread")]
#[cfg(not(windows))]
async fn concurrent_mixed_rebinds_are_serialized_and_return_installed_ports() {
    let (dir, state) = temp_state("concurrent-mixed");
    let (old_port, shutdown_seen, release) = install_held_listener(&state, loopback(0)).await;

    let first_state = state.clone();
    let first = tokio::spawn(async move {
        let returned = GatewayLifecycle::rebind(first_state.clone(), public(0))
            .await
            .expect("first public rebind should succeed");
        let observed = first_state.active_gateway_port();
        (returned, observed)
    });
    tokio::time::timeout(Duration::from_secs(2), shutdown_seen)
        .await
        .expect("first rebind should signal held old listener")
        .expect("shutdown observation channel should stay open");
    let first_installed_port = state.active_gateway_port();
    assert_ne!(first_installed_port, old_port);
    assert!(!state.dashboard_local_mode());

    let start_second = Arc::new(Barrier::new(2));
    let second_start = start_second.clone();
    let second_state = state.clone();
    let mut second = tokio::spawn(async move {
        second_start.wait().await;
        let returned = GatewayLifecycle::rebind(second_state.clone(), loopback(0))
            .await
            .expect("second loopback rebind should succeed");
        let observed = second_state.active_gateway_port();
        (returned, observed)
    });
    start_second.wait().await;

    // The first transition is paused inside old-listener shutdown. The second
    // call has crossed its start barrier but must remain queued on the async
    // lifecycle gate and may not replace the installed public listener.
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut second)
            .await
            .is_err(),
        "second rebind must not complete while the first transition owns the gate"
    );
    assert_eq!(state.active_gateway_port(), first_installed_port);
    assert!(!state.dashboard_local_mode());

    release.wait().await;
    let (first_returned, first_observed) = first.await.expect("first rebind task should finish");
    assert_eq!(first_returned, first_installed_port);
    assert_eq!(first_observed, first_returned);

    let (second_returned, second_observed) =
        second.await.expect("second rebind task should finish");
    assert_eq!(second_observed, second_returned);
    assert_eq!(state.active_gateway_port(), second_returned);
    assert_ne!(second_returned, first_returned);
    assert!(state.dashboard_local_mode());
    assert_dashboard_auth(second_returned, reqwest::StatusCode::OK).await;
    assert_not_serving(first_returned, &state.config().gateway_key).await;

    cleanup(dir, state).await;
}

#[tokio::test]
async fn stop_timeout_aborts_and_awaits_listener_task_termination() {
    let dropped = Arc::new(AtomicBool::new(false));
    let task_dropped = dropped.clone();
    let (ready_tx, ready_rx) = oneshot::channel();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        let _drop_signal = DropSignal(task_dropped);
        let _ = ready_tx.send(());
        let _ = shutdown_rx.await;
        std::future::pending::<()>().await;
    });
    ready_rx.await.expect("listener task should start");

    let outcome = GatewayLifecycle::stop_and_wait_for(
        GatewayHandle {
            port: 1,
            listen_addr: loopback(1),
            dashboard_is_local: false,
            shutdown: shutdown_tx,
            task,
        },
        Duration::from_millis(10),
    )
    .await;

    assert_eq!(outcome, ListenerStopOutcome::AbortedAfterTimeout);
    assert!(
        dropped.load(Ordering::Acquire),
        "timeout handling must await task cancellation before returning"
    );
}

#[tokio::test]
#[cfg(not(windows))]
async fn signal_only_public_to_loopback_restores_trust_after_old_listener_quiesces() {
    let (dir, state) = temp_state("signal-only-public-to-loopback");
    let (old_port, shutdown_seen, release) = install_held_listener(&state, public(0)).await;
    assert!(!state.dashboard_local_mode());
    assert_dashboard_auth(old_port, reqwest::StatusCode::UNAUTHORIZED).await;

    let new_port = GatewayLifecycle::rebind_from_serving_request(state.clone(), loopback(0))
        .await
        .expect("signal-only public-to-loopback rebind should bind the new listener");
    tokio::time::timeout(Duration::from_secs(2), shutdown_seen)
        .await
        .expect("signal-only rebind should signal the old listener")
        .expect("shutdown observation channel should stay open");

    assert!(!state.dashboard_local_mode());
    assert_eq!(state.active_gateway_port(), new_port);
    assert_dashboard_auth(old_port, reqwest::StatusCode::UNAUTHORIZED).await;
    assert_dashboard_auth(new_port, reqwest::StatusCode::UNAUTHORIZED).await;

    release.wait().await;
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if state.dashboard_local_mode() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("trust must restore after the displaced public listener quiesces");

    assert!(state.dashboard_local_mode());
    assert_eq!(state.active_gateway_port(), new_port);
    assert_dashboard_auth(new_port, reqwest::StatusCode::OK).await;
    assert_not_serving(old_port, &state.config().gateway_key).await;

    cleanup(dir, state).await;
}

#[tokio::test]
#[cfg(not(windows))]
async fn independently_bound_public_listener_keeps_shared_trust_fail_closed() {
    let (dir, state) = temp_state("independent-bind-trust");
    let public_handle = GatewayLifecycle::bind(state.clone(), public(0))
        .await
        .expect("public listener should bind");
    let public_port = public_handle.port;
    assert!(!state.dashboard_local_mode());

    // Even though neither handle is installed in `state.gateway`, the live
    // public task is lifecycle-tracked and a later loopback bind cannot make
    // its dashboard routes trust unauthenticated requests.
    let loopback_handle = GatewayLifecycle::bind(state.clone(), loopback(0))
        .await
        .expect("loopback listener should bind");
    assert!(!state.dashboard_local_mode());
    assert_dashboard_auth(public_port, reqwest::StatusCode::UNAUTHORIZED).await;
    assert_dashboard_auth(loopback_handle.port, reqwest::StatusCode::UNAUTHORIZED).await;

    let _ = GatewayLifecycle::stop_and_wait(public_handle).await;
    let _ = GatewayLifecycle::stop_and_wait(loopback_handle).await;
    drop(state);
    let _ = fs::remove_dir_all(dir);
}

#[tokio::test]
#[cfg(not(windows))]
async fn stopping_last_independent_public_listener_restores_installed_loopback_trust() {
    let (dir, state) = temp_state("independent-public-stop-restores-trust");
    let loopback_port = install_listener(&state).await;
    assert!(state.dashboard_local_mode());

    let public_handle = GatewayLifecycle::bind(state.clone(), public(0))
        .await
        .expect("independent public listener should bind");
    let public_port = public_handle.port;
    assert!(!state.dashboard_local_mode());
    assert_dashboard_auth(loopback_port, reqwest::StatusCode::UNAUTHORIZED).await;
    assert_dashboard_auth(public_port, reqwest::StatusCode::UNAUTHORIZED).await;

    let _ = GatewayLifecycle::stop_and_wait(public_handle).await;
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if state.dashboard_local_mode() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("last public unregister must restore installed loopback trust");
    assert_dashboard_auth(loopback_port, reqwest::StatusCode::OK).await;

    cleanup(dir, state).await;
}

#[tokio::test]
#[cfg(not(windows))]
async fn new_independent_public_during_displaced_public_drain_stays_fail_closed() {
    let (dir, state) = temp_state("overlapping-public-drain");
    let old_public = GatewayLifecycle::bind(state.clone(), public(0))
        .await
        .expect("initial public listener should bind");
    let old_port = old_public.port;
    *state.gateway.lock() = Some(old_public);

    // Confirm an in-flight body extractor before triggering graceful shutdown.
    // The incomplete request keeps the displaced public task draining while a
    // new independent public listener is registered.
    let mut held_request = tokio::net::TcpStream::connect(("127.0.0.1", old_port))
        .await
        .expect("held request should connect");
    let headers = format!(
        "POST /v1/chat/completions HTTP/1.1\r\n\
         Host: 127.0.0.1:{old_port}\r\n\
         Authorization: Bearer {}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: 128\r\n\
         Expect: 100-continue\r\n\r\n",
        state.config().gateway_key
    );
    held_request
        .write_all(headers.as_bytes())
        .await
        .expect("held request headers should write");
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), async {
        let mut buffer = [0_u8; 256];
        loop {
            let read = held_request
                .read(&mut buffer)
                .await
                .expect("100-continue response should read");
            assert_ne!(read, 0, "connection closed before 100 Continue");
            response.extend_from_slice(&buffer[..read]);
            if response.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
    })
    .await
    .expect("server should acknowledge the pending body");
    assert!(
        String::from_utf8_lossy(&response).contains("100 Continue"),
        "server must be waiting on the incomplete request body: {}",
        String::from_utf8_lossy(&response)
    );

    let loopback_port = GatewayLifecycle::rebind_from_serving_request(state.clone(), loopback(0))
        .await
        .expect("signal-only public-to-loopback rebind should install loopback");
    assert!(!state.dashboard_local_mode());

    let overlapping_public = GatewayLifecycle::bind(state.clone(), public(0))
        .await
        .expect("new independent public listener should bind during old drain");
    let overlapping_port = overlapping_public.port;
    assert_eq!(state.dashboard_public_listener_count(), 2);
    assert_dashboard_auth(loopback_port, reqwest::StatusCode::UNAUTHORIZED).await;
    assert_dashboard_auth(overlapping_port, reqwest::StatusCode::UNAUTHORIZED).await;

    held_request
        .write_all(&[b' '; 128])
        .await
        .expect("completing the held request body should write");
    let mut completed_response = Vec::new();
    tokio::time::timeout(
        Duration::from_secs(2),
        held_request.read_to_end(&mut completed_response),
    )
    .await
    .expect("completed request should finish during graceful drain")
    .expect("completed response should read to EOF");
    drop(held_request);
    // Axum may retain the listener task until the lifecycle's five-second
    // bounded graceful-shutdown fallback even after the in-flight response
    // closes. Await the exact old-registration 2 -> 1 transition instead of
    // probing the socket and perturbing its accept backlog.
    tokio::time::timeout(Duration::from_secs(7), async {
        loop {
            if state.dashboard_public_listener_count() == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("older displaced public listener should finish draining");
    assert!(
        !state.dashboard_local_mode(),
        "older observer must not restore trust while a newer public listener exists"
    );
    assert_dashboard_auth(loopback_port, reqwest::StatusCode::UNAUTHORIZED).await;

    let _ = GatewayLifecycle::stop_and_wait(overlapping_public).await;
    assert_eq!(state.dashboard_public_listener_count(), 0);
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if state.dashboard_local_mode() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("final public unregister should restore loopback trust");
    assert_dashboard_auth(loopback_port, reqwest::StatusCode::OK).await;

    cleanup(dir, state).await;
}
