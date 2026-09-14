//! Emit a static preset-id → offering map from `resources/provider-presets.json`.
//!
//! This is not a general codegen framework: the only output is the offering
//! table consumed by `preset_offering`. Unknown or missing runtime ids still
//! resolve to `"api"`.

use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::PathBuf;

#[allow(dead_code)]
fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let presets_path = manifest_dir
        .join("..")
        .join("..")
        .join("resources")
        .join("provider-presets.json");
    println!("cargo:rerun-if-changed={}", presets_path.display());

    let raw = fs::read_to_string(&presets_path).unwrap_or_else(|error| {
        panic!(
            "failed to read provider presets at {}: {error}",
            presets_path.display()
        )
    });
    let generated = generate_preset_offerings(&raw).unwrap_or_else(|error| {
        panic!(
            "invalid provider-presets.json ({}): {error}",
            presets_path.display()
        )
    });

    let out_path = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("preset_offerings.rs");
    fs::write(&out_path, generated).unwrap_or_else(|error| {
        panic!(
            "failed to write preset offering map to {}: {error}",
            out_path.display()
        )
    });
}

pub fn generate_preset_offerings(raw: &str) -> Result<String, String> {
    let value: serde_json::Value =
        serde_json::from_str(raw).map_err(|error| format!("must be valid JSON: {error}"))?;
    let rows = value
        .as_array()
        .ok_or_else(|| "must be a JSON array".to_string())?;

    let mut seen = HashSet::new();
    let mut entries = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let object = row
            .as_object()
            .ok_or_else(|| format!("row {index} must be an object"))?;
        let id = object
            .get("id")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| format!("row {index} needs a non-empty id"))?;
        if !seen.insert(id.to_string()) {
            return Err(format!("duplicate id `{id}`"));
        }
        let offering = match object.get("offering") {
            None => "api",
            Some(value) => {
                let offering = value
                    .as_str()
                    .ok_or_else(|| format!("row {index} (`{id}`) offering must be a string"))?;
                match offering {
                    "plan" | "api" => offering,
                    other => {
                        return Err(format!(
                            "row {index} (`{id}`) offering must be absent, plan, or api; got `{other}`"
                        ));
                    }
                }
            }
        };
        entries.push((id.to_string(), offering.to_string()));
    }

    let mut generated = String::from(
        "/// Preset offering table generated from `resources/provider-presets.json`.\n\
         const PRESET_OFFERINGS: &[(&str, &str)] = &[\n",
    );
    for (id, offering) in &entries {
        generated.push_str("    (\"");
        generated.push_str(&escape_rust_str(id));
        generated.push_str("\", \"");
        generated.push_str(&escape_rust_str(offering));
        generated.push_str("\"),\n");
    }
    generated.push_str("];\n");

    Ok(generated)
}

fn escape_rust_str(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}
