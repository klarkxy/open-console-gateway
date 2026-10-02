use super::*;
use ocg_core::{crypto::StaticKeyCipher, db::Database, state::CoreStateInner};
use std::sync::Arc;

#[test]
fn occupied_port_fails_start_without_claiming_a_listener() {
    let root = std::env::temp_dir().join(format!("ocg-host-bind-{}", uuid::Uuid::new_v4()));
    let cipher = Arc::new(StaticKeyCipher::new("host-bind-test"));
    let db = Database::open_with_cipher(root.clone(), cipher.clone()).unwrap();
    let core = Arc::new(CoreStateInner::new(db, root.clone(), cipher).unwrap());
    let occupied = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    core.register_gateway_port_override(occupied.local_addr().unwrap().port())
        .unwrap();
    assert!(start_on_configured_port(&core).is_err());
    assert!(core.gateway.lock().is_none());
    assert_no_gateway_operation(&core);
    drop(occupied);
    start_on_configured_port(&core).unwrap();
    assert_no_gateway_operation(&core);
    let handle = core.gateway.lock().take().unwrap();
    let _ =
        tauri::async_runtime::block_on(ocg_core::gateway::GatewayLifecycle::stop_and_wait(handle));
    drop(core);
    std::fs::remove_dir_all(root).unwrap();
}

fn assert_no_gateway_operation(core: &CoreStateInner) {
    let operations = core
        .db
        .lock()
        .query_operation_logs(&ocg_core::log_types::OperationLogQuery::default())
        .unwrap();
    assert!(
        operations.items.is_empty(),
        "automatic gateway start records no user operation: {:?}",
        operations.items
    );
    let gateway_logs = core.db.lock().list_gateway_logs(20).unwrap();
    assert!(
        gateway_logs.iter().all(|row| row.category != "gateway"),
        "automatic gateway start stays off gateway history: {gateway_logs:?}"
    );
}
