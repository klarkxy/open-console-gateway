//! Dashboard V4 contract kernel: schema drift against the generated catalog.

use ocg_core::dashboard_v4::contract_schema_pretty;
use std::fs;
use std::path::PathBuf;

fn checked_in_schema_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schema/dashboard-api-v4.schema.json")
}

fn normalize_schema_text(text: &str) -> String {
    text.replace("\r\n", "\n")
}

#[test]
fn checked_in_schema_matches_rust_dtos() {
    let generated = contract_schema_pretty();
    let path = checked_in_schema_path();
    let checked_in = fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "schema/dashboard-api-v4.schema.json is missing ({error}); generate it after the V4 Rust catalog lands"
        );
    });
    assert_eq!(
        normalize_schema_text(&generated),
        normalize_schema_text(&checked_in),
        "Dashboard V4 schema drifted; regenerate schema/dashboard-api-v4.schema.json"
    );
}
