//! Print the Dashboard V4 JSON Schema catalog to stdout.
//!
//! Used by the V4 contract generation pipeline. Output is deterministic.

fn main() {
    print!("{}", ocg_core::dashboard_v4::contract_schema_pretty());
}
