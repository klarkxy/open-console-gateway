use super::*;
use std::io::Read;
#[test]
fn embedded_vsix_contains_exact_provider_runtime_and_identity() {
    let bytes = vsix_bytes().unwrap();
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut manifest = String::new();
    archive
        .by_name("extension/package.json")
        .unwrap()
        .read_to_string(&mut manifest)
        .unwrap();
    let manifest: serde_json::Value = serde_json::from_str(&manifest).unwrap();
    assert_eq!(manifest["publisher"], "open-console-gateway");
    assert_eq!(manifest["name"], "copilot");
    assert_eq!(manifest["version"], EXTENSION_VERSION);
    assert_eq!(manifest["extensionKind"], serde_json::json!(["ui"]));
    let mut runtime = Vec::new();
    archive
        .by_name("extension/dist/extension.cjs")
        .unwrap()
        .read_to_end(&mut runtime)
        .unwrap();
    assert_eq!(runtime, RUNTIME);
    assert!(archive.by_name("extension.vsixmanifest").is_ok());
    assert!(archive.by_name("[Content_Types].xml").is_ok());
}
#[test]
fn package_is_deterministic() {
    assert_eq!(vsix_bytes().unwrap(), vsix_bytes().unwrap());
}
