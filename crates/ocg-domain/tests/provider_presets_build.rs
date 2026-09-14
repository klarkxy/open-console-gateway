#[path = "../build.rs"]
mod provider_presets_build;

use provider_presets_build::generate_preset_offerings;

#[test]
fn json_offering_is_the_generated_rust_authority() {
    let generated = generate_preset_offerings(
        r#"[
          {"id":"future-plan","offering":"plan"},
          {"id":"future-api"}
        ]"#,
    )
    .unwrap();
    assert!(generated.contains("(\"future-plan\", \"plan\")"));
    assert!(generated.contains("(\"future-api\", \"api\")"));
}

#[test]
fn duplicate_ids_and_invalid_offerings_block_generation() {
    let duplicate =
        generate_preset_offerings(r#"[{"id":"same"},{"id":" same ","offering":"plan"}]"#)
            .unwrap_err();
    assert!(duplicate.contains("duplicate id `same`"), "{duplicate}");

    let invalid =
        generate_preset_offerings(r#"[{"id":"future","offering":"subscription"}]"#).unwrap_err();
    assert!(
        invalid.contains("must be absent, plan, or api"),
        "{invalid}"
    );
}
