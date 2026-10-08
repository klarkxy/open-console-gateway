use super::*;

const NO_IDS: &[String] = &[];

fn catalogs<'a>(
    go: &'a [String],
    zen_free: &'a [String],
    custom: &'a [String],
    command_code: &'a [String],
    minimax: &'a [String],
    kimi: &'a [String],
) -> RuntimeCatalogs<'a> {
    RuntimeCatalogs {
        go,
        zen_free,
        custom,
        command_code,
        minimax,
        kimi,
        cpa: NO_IDS,
        ollama: NO_IDS,
        ollama_pinned: NO_IDS,
        builtin_aliases: &[],
        extra: &[],
    }
}

fn resolve_with_custom(
    requested: &str,
    custom_model_ids: &[String],
) -> Result<ResolvedModel, ResolveError> {
    resolve_with_runtime_catalogs(
        requested,
        catalogs(NO_IDS, NO_IDS, custom_model_ids, NO_IDS, NO_IDS, NO_IDS),
    )
}

fn resolve_with_provider_models(
    requested: &str,
    zen_free_models: &[String],
    custom_model_ids: &[String],
) -> Result<ResolvedModel, ResolveError> {
    resolve_with_runtime_catalogs(
        requested,
        catalogs(
            NO_IDS,
            zen_free_models,
            custom_model_ids,
            NO_IDS,
            NO_IDS,
            NO_IDS,
        ),
    )
}

fn resolve_with_catalogs(
    requested: &str,
    zen_free_models: &[String],
    custom_model_ids: &[String],
    goat_model_ids: &[String],
) -> Result<ResolvedModel, ResolveError> {
    resolve_with_runtime_catalogs(
        requested,
        catalogs(
            NO_IDS,
            zen_free_models,
            custom_model_ids,
            goat_model_ids,
            NO_IDS,
            NO_IDS,
        ),
    )
}

fn resolve_with_all_catalogs(
    requested: &str,
    go_model_ids: &[String],
    zen_free_models: &[String],
    custom_model_ids: &[String],
    goat_model_ids: &[String],
) -> Result<ResolvedModel, ResolveError> {
    resolve_with_runtime_catalogs(
        requested,
        catalogs(
            go_model_ids,
            zen_free_models,
            custom_model_ids,
            goat_model_ids,
            NO_IDS,
            NO_IDS,
        ),
    )
}

fn resolve_with_extended_catalogs(
    requested: &str,
    go_model_ids: &[String],
    zen_free_models: &[String],
    custom_model_ids: &[String],
    goat_model_ids: &[String],
    minimax_model_ids: &[String],
    kimi_model_ids: &[String],
) -> Result<ResolvedModel, ResolveError> {
    resolve_with_runtime_catalogs(
        requested,
        catalogs(
            go_model_ids,
            zen_free_models,
            custom_model_ids,
            goat_model_ids,
            minimax_model_ids,
            kimi_model_ids,
        ),
    )
}

fn published_routeable_aliases_with_zen(zen_free_models: &[String]) -> Vec<PublishedAlias> {
    published_routeable_aliases_with_runtime_catalogs(catalogs(
        NO_IDS,
        zen_free_models,
        NO_IDS,
        NO_IDS,
        NO_IDS,
        NO_IDS,
    ))
}

fn published_routeable_aliases_with_catalogs(
    zen_free_models: &[String],
    goat_model_ids: &[String],
) -> Vec<PublishedAlias> {
    published_routeable_aliases_with_runtime_catalogs(catalogs(
        NO_IDS,
        zen_free_models,
        NO_IDS,
        goat_model_ids,
        NO_IDS,
        NO_IDS,
    ))
}

fn published_routeable_aliases_with_all_catalogs(
    go_model_ids: &[String],
    zen_free_models: &[String],
    goat_model_ids: &[String],
) -> Vec<PublishedAlias> {
    published_routeable_aliases_with_runtime_catalogs(catalogs(
        go_model_ids,
        zen_free_models,
        NO_IDS,
        goat_model_ids,
        NO_IDS,
        NO_IDS,
    ))
}

fn routeable_aliases_for_with_extended_catalogs(
    provider_id: &str,
    zen_free_models: &[String],
    goat_model_ids: &[String],
    minimax_model_ids: &[String],
    kimi_model_ids: &[String],
) -> Vec<String> {
    routeable_aliases_for_with_runtime_catalogs(
        provider_id,
        catalogs(
            NO_IDS,
            zen_free_models,
            NO_IDS,
            goat_model_ids,
            minimax_model_ids,
            kimi_model_ids,
        ),
    )
}

fn is_published_alias(name: &str) -> bool {
    matches!(resolve(name), Ok(ResolvedModel::Alias { .. }))
}

/// A refreshed Zen Free catalog used only as a test fixture. The builtin
/// registry stays empty until an official `/models` snapshot is supplied.
fn example_zen_free_catalog() -> Vec<String> {
    [
        "deepseek-v4-flash-free",
        "ling-3.0-flash-fin-free",
        "mimo-v2.5-free",
        "muse-spark-1.2-contributor-free",
        "muse-spark-1.3-contributor-free",
        "nemotron-3-ultra-free",
        "nemotron-3.5-lightning-free",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn resolve_with_example_zen(requested: &str) -> Result<ResolvedModel, ResolveError> {
    resolve_with_provider_models(requested, &example_zen_free_catalog(), &[])
}

#[test]
fn go_model_ids_are_preferred_aliases() {
    let resolved = resolve("glm-5.2").expect("known Go model");
    match resolved {
        ResolvedModel::Alias {
            alias, mappings, ..
        } => {
            assert_eq!(alias, "glm-5.2");
            assert_eq!(mappings.len(), 1);
            assert!(mappings[0].is_opencode_go());
            assert!(mappings[0].routeable);
            assert_eq!(mappings[0].upstream_model, "glm-5.2");
        }
        other => panic!("expected alias, got {other:?}"),
    }
}

#[test]
fn alias_lookup_is_case_insensitive_kebab() {
    let resolved = resolve("GLM-5.2").expect("case-folded alias");
    assert!(matches!(resolved, ResolvedModel::Alias { alias, .. } if alias == "glm-5.2"));
    assert!(is_published_alias("Grok-4.5"));
    assert!(is_published_alias(" glm-5.2 "));
    for alias in published_aliases() {
        assert_eq!(alias, alias.to_lowercase());
        assert!(!alias.is_empty());
        assert!(!looks_raw_shaped(&alias));
    }
}

#[test]
fn raw_looking_names_do_not_collapse_onto_kebab_aliases() {
    for name in ["glm/5.2", "GLM_5.2", "Grok 4.5", "glm 5.2"] {
        match resolve(name) {
            Err(ResolveError::Unknown { requested }) => assert_eq!(requested, name),
            other => panic!("`{name}` must not collapse onto a kebab alias, got {other:?}"),
        }
        assert!(!is_published_alias(name));
    }
    assert!(matches!(
        resolve("glm-5.2").unwrap(),
        ResolvedModel::Alias { alias, .. } if alias == "glm-5.2"
    ));
}

#[test]
fn free_ids_are_exact_pins_and_only_stripped_aliases_are_published() {
    let resolved = resolve_with_example_zen("deepseek-v4-flash-free").expect("Zen model id");
    match resolved {
        ResolvedModel::PinnedRaw { mapping, .. } => {
            assert!(mapping.is_zen_free());
            assert_eq!(mapping.upstream_model, "deepseek-v4-flash-free");
        }
        other => panic!("expected raw pin, got {other:?}"),
    }
    assert!(matches!(
        resolve_with_example_zen("deepseek-v4-flash"),
        Ok(ResolvedModel::Alias { mappings, .. }) if mappings.iter().any(ProviderMapping::is_zen_free)
    ));
    assert!(
        !published_routeable_aliases_with_zen(&example_zen_free_catalog())
            .iter()
            .any(|entry| entry.alias == "deepseek-v4-flash-free")
    );
    assert!(
        resolve("deepseek-v4-flash-free").is_err(),
        "unfetched Zen must not leave a leftover seed pin in the builtin registry"
    );
}

#[test]
fn shared_aliases_record_go_and_zen_mappings_in_the_registry() {
    match resolve("mimo-v2.5").unwrap() {
        ResolvedModel::Alias { mappings, .. } => {
            assert_eq!(mappings.len(), 1);
            assert!(mappings[0].is_opencode_go());
        }
        other => panic!("expected Go-only builtin alias, got {other:?}"),
    }
    match resolve_with_example_zen("mimo-v2.5").unwrap() {
        ResolvedModel::Alias { mappings, .. } => {
            assert_eq!(mappings.len(), 2);
            assert!(mappings[0].is_opencode_go());
            assert!(mappings[1].is_zen_free());
            assert_eq!(mappings[1].upstream_model, "mimo-v2.5-free");
        }
        other => panic!("expected alias, got {other:?}"),
    }
    match resolve("glm-5.2").unwrap() {
        ResolvedModel::Alias { mappings, .. } => assert_eq!(mappings.len(), 1),
        other => panic!("expected alias, got {other:?}"),
    }
}

#[test]
fn command_catalog_uses_go_canonical_aliases_and_keeps_raw_ids_pinned() {
    let go = vec!["hy3".to_string(), "future-go".to_string()];
    let zen = vec!["hy3-free".to_string()];
    let command = vec![
        "vendor/hy3".to_string(),
        "vendor/future-go".to_string(),
        "acme/Model_Name".to_string(),
        "stealth/ox-alpha".to_string(),
    ];

    match resolve_with_all_catalogs("hy3", &go, &zen, &[], &command).unwrap() {
        ResolvedModel::Alias {
            alias, mappings, ..
        } => {
            assert_eq!(alias, "hy3");
            assert!(mappings.iter().any(ProviderMapping::is_opencode_go));
            assert!(mappings.iter().any(ProviderMapping::is_zen_free));
            assert!(mappings.iter().any(|mapping| {
                mapping.is_command_code_goat() && mapping.upstream_model == "vendor/hy3"
            }));
        }
        other => panic!("expected three-supplier Alias, got {other:?}"),
    }
    assert!(matches!(
        resolve_with_all_catalogs("vendor/hy3", &go, &zen, &[], &command),
        Ok(ResolvedModel::PinnedRaw { mapping, .. })
            if mapping.is_command_code_goat() && mapping.upstream_model == "vendor/hy3"
    ));
    assert!(matches!(
        resolve_with_all_catalogs("future-go", &go, &zen, &[], &command),
        Ok(ResolvedModel::PinnedRaw { mapping, .. })
            if mapping.is_opencode_go() && mapping.upstream_model == "future-go"
    ));
    let suffix_command = vec!["hy3-paid".to_string()];
    assert!(matches!(
        resolve_with_all_catalogs("hy3", &go, &zen, &[], &suffix_command),
        Ok(ResolvedModel::Alias { mappings, .. })
            if mappings.iter().any(|mapping| mapping.is_command_code_goat()
                && mapping.upstream_model == "hy3-paid")
    ));
    assert!(matches!(
        resolve_with_all_catalogs("hy3-paid", &go, &zen, &[], &suffix_command),
        Ok(ResolvedModel::PinnedRaw { mapping, .. })
            if mapping.is_command_code_goat() && mapping.upstream_model == "hy3-paid"
    ));
    assert!(matches!(
        resolve_with_all_catalogs("stealth/ox-alpha", &go, &zen, &[], &command),
        Ok(ResolvedModel::PinnedRaw { mapping, .. })
            if mapping.is_command_code_goat()
                && mapping.upstream_model == "stealth/ox-alpha"
    ));

    match resolve_with_all_catalogs("model-name", &go, &zen, &[], &command).unwrap() {
        ResolvedModel::Alias {
            alias, mappings, ..
        } => {
            assert_eq!(alias, "model-name");
            assert!(mappings.iter().any(|mapping| {
                mapping.is_command_code_goat() && mapping.upstream_model == "acme/Model_Name"
            }));
        }
        other => panic!("unique slash Command ids publish the last-segment Alias, got {other:?}"),
    }
    assert!(matches!(
        resolve_with_all_catalogs("acme/Model_Name", &go, &zen, &[], &command),
        Ok(ResolvedModel::PinnedRaw { mapping, .. })
            if mapping.is_command_code_goat() && mapping.upstream_model == "acme/Model_Name"
    ));

    let shared_raw = vec!["vendor/shared".to_string()];
    let error = resolve_with_all_catalogs("vendor/shared", &shared_raw, &[], &[], &shared_raw)
        .expect_err("provider raw collision must remain ambiguous");
    assert_eq!(error.code(), Some(AMBIGUOUS_MODEL_ID));

    let published = published_routeable_aliases_with_all_catalogs(&go, &zen, &command);
    assert_eq!(
        published.iter().filter(|item| item.alias == "hy3").count(),
        1
    );
    assert!(published.iter().any(|item| item.alias == "model-name"));
    assert!(
        !published
            .iter()
            .any(|item| { item.alias.contains('/') || is_free_model(&item.alias) })
    );
}

#[test]
fn command_catalog_shortens_code_owned_and_unique_slash_leaves() {
    let nemotron_upstream = "nvidia/nemotron-3-ultra-550b-a55b".to_string();
    let command = vec![nemotron_upstream.clone()];

    match resolve_with_all_catalogs("nemotron-3-ultra-550b-a55b", &[], &[], &[], &command).unwrap()
    {
        ResolvedModel::Alias {
            alias, mappings, ..
        } => {
            assert_eq!(alias, "nemotron-3-ultra-550b-a55b");
            assert!(mappings.iter().any(|mapping| {
                mapping.is_command_code_goat() && mapping.upstream_model == nemotron_upstream
            }));
        }
        other => panic!("expected sealed GOAT short Alias, got {other:?}"),
    }
    assert!(matches!(
        resolve_with_all_catalogs(&nemotron_upstream, &[], &[], &[], &command),
        Ok(ResolvedModel::PinnedRaw { mapping, .. })
            if mapping.is_command_code_goat()
                && mapping.upstream_model == nemotron_upstream
    ));
    assert_eq!(
        canonical_alias_for_provider_model(COMMAND_CODE_PROVIDER_ID, &nemotron_upstream, &[], &[],),
        "nemotron-3-ultra-550b-a55b"
    );
    assert!(
        published_routeable_aliases_with_all_catalogs(&[], &[], &command)
            .iter()
            .any(|item| item.alias == "nemotron-3-ultra-550b-a55b"
                && item.owned_by == COMMAND_CODE_PROVIDER_ID)
    );
    assert_eq!(
        routeable_aliases_for_with_extended_catalogs(
            COMMAND_CODE_PROVIDER_ID,
            &[],
            &command,
            &[],
            &[],
        ),
        vec!["nemotron-3-ultra-550b-a55b".to_string()]
    );
    assert!(
        !published_routeable_aliases_with_all_catalogs(&[], &[], &[])
            .iter()
            .any(|item| item.alias == "nemotron-3-ultra-550b-a55b")
    );

    let future = vec!["vendor/future-model-with-a-very-long-name".to_string()];
    match resolve_with_all_catalogs("future-model-with-a-very-long-name", &[], &[], &[], &future)
        .unwrap()
    {
        ResolvedModel::Alias {
            alias, mappings, ..
        } => {
            assert_eq!(alias, "future-model-with-a-very-long-name");
            assert!(mappings.iter().any(|mapping| {
                mapping.is_command_code_goat()
                    && mapping.upstream_model == "vendor/future-model-with-a-very-long-name"
            }));
        }
        other => panic!("unique slash Command ids publish the last-segment Alias, got {other:?}"),
    }
    assert!(
        published_routeable_aliases_with_all_catalogs(&[], &[], &future)
            .iter()
            .any(|item| item.alias == "future-model-with-a-very-long-name")
    );
    assert_eq!(
        canonical_alias_for_provider_model(
            COMMAND_CODE_PROVIDER_ID,
            "google/gemini-3.5-flash",
            &[],
            &[],
        ),
        "gemini-3.5-flash"
    );
    assert_eq!(
        canonical_alias_for_provider_model(COMMAND_CODE_PROVIDER_ID, "claude-sonnet-4-6", &[], &[],),
        "claude-sonnet-4-6"
    );
    let colliding = vec![
        "deepseek/deepseek-v4-flash-fast".to_string(),
        "other/deepseek-v4-flash-fast".to_string(),
    ];
    assert!(
        !published_routeable_aliases_with_all_catalogs(&[], &[], &colliding)
            .iter()
            .any(|item| item.alias == "deepseek-v4-flash-fast"),
        "ambiguous last-segment names stay raw pins"
    );
}

#[test]
fn command_new_catalog_names_publish_without_a_model_allowlist() {
    let command = (0..32)
        .map(|n| format!("next-catalog-model-{n}"))
        .collect::<Vec<_>>();
    let index = RuntimeCatalogIndex::from_catalogs(RuntimeCatalogs {
        command_code: &command,
        ..Default::default()
    });
    let aliases = command_catalog_aliases(&command, &[], &[]);
    for upstream in &command {
        assert_eq!(aliases.get(upstream), Some(upstream));
        assert!(
            index
                .published_models()
                .iter()
                .any(|row| row.alias == *upstream && row.owned_by == COMMAND_CODE_PROVIDER_ID)
        );
        assert!(
            matches!(index.resolve(upstream), Ok(ResolvedModel::Alias { mappings, .. })
            if mappings == vec![goat_mapping(upstream, true)])
        );
    }
    let empty = RuntimeCatalogIndex::from_catalogs(RuntimeCatalogs::default());
    assert!(empty.resolve(&command[0]).is_err());
    assert!(
        !empty
            .published_models()
            .iter()
            .any(|row| command.contains(&row.alias))
    );
}

#[test]
fn command_generated_names_do_not_authorize_suffix_stripping_and_ignore_catalog_order() {
    let command = vec![
        "next-model-paid".into(),
        "next-model".into(),
        "next-model-free".into(),
    ];
    let mut reversed = command.clone();
    reversed.reverse();
    let aliases = command_catalog_aliases(&command, &[], &[]);
    assert_eq!(aliases, command_catalog_aliases(&reversed, &[], &[]));
    for ids in [&command, &reversed] {
        let index = RuntimeCatalogIndex::from_catalogs(RuntimeCatalogs {
            command_code: ids,
            ..Default::default()
        });
        for upstream in &command {
            assert_eq!(aliases.get(upstream), Some(upstream));
            assert!(
                matches!(index.resolve(upstream), Ok(ResolvedModel::Alias { alias, mappings, .. })
                if alias == *upstream && mappings == vec![goat_mapping(upstream, true)])
            );
        }
    }
}

#[test]
fn command_catalog_aliases_reject_normalization_collisions_and_reserved_go_names() {
    let command = vec![
        "vendor/Next_Model".into(),
        "next-model".into(),
        "future-go".into(),
    ];
    let go = vec!["future-go".into()];
    assert!(command_catalog_aliases(&command, &go, &[]).is_empty());
    let index = RuntimeCatalogIndex::from_catalogs(RuntimeCatalogs {
        command_code: &command,
        go: &go,
        ..Default::default()
    });
    assert!(
        !index
            .published_models()
            .iter()
            .any(|row| row.alias == "next-model" || row.alias == "future-go")
    );
    assert!(
        matches!(index.resolve("vendor/Next_Model"), Ok(ResolvedModel::PinnedRaw { mapping, .. })
        if mapping == goat_mapping("vendor/Next_Model", true))
    );
    assert!(matches!(
        index.resolve("future-go"),
        Err(ResolveError::Ambiguous { .. })
    ));
    let unique = ["Vendor/Next_Model".to_string()];
    assert_eq!(
        command_catalog_aliases(&unique, &[], &[])
            .get(&unique[0])
            .map(String::as_str),
        Some("next-model")
    );
}

#[test]
fn command_generated_aliases_keep_saved_custom_names_and_exact_raw_pins() {
    let upstream = "next-model-77";
    let command = [upstream.to_string()];
    let overrides = [ExtraProviderCatalog {
        provider_id: COMMAND_CODE_PROVIDER_ID.into(),
        mappings: vec![("my-next-model".into(), upstream.into())],
    }];
    let index = RuntimeCatalogIndex::from_catalogs(RuntimeCatalogs {
        command_code: &command,
        builtin_aliases: &overrides,
        ..Default::default()
    });
    assert!(
        !index
            .published_models()
            .iter()
            .any(|row| row.alias == upstream)
    );
    assert!(
        matches!(index.resolve("my-next-model"), Ok(ResolvedModel::Alias { mappings, .. })
        if mappings == vec![goat_mapping(upstream, true)])
    );
    assert!(
        matches!(index.resolve(upstream), Ok(ResolvedModel::PinnedRaw { mapping, .. })
        if mapping == goat_mapping(upstream, true))
    );
}

#[test]
fn command_catalog_reuses_sealed_cn_aliases_and_known_plan_suffixes() {
    let command = vec![
        "moonshotai/Kimi-K2.7-Code-Highspeed".to_string(),
        "poolside/laguna-s-2.1-free".to_string(),
    ];
    for (alias, upstream) in [
        (
            "kimi-k2.7-code-highspeed",
            "moonshotai/Kimi-K2.7-Code-Highspeed",
        ),
        ("laguna-s-2.1", "poolside/laguna-s-2.1-free"),
    ] {
        match resolve_with_all_catalogs(alias, &[], &["laguna-s-2.1-free".into()], &[], &command)
            .unwrap()
        {
            ResolvedModel::Alias { mappings, .. } => assert!(mappings.iter().any(|mapping| {
                mapping.is_command_code_goat() && mapping.upstream_model == upstream
            })),
            other => panic!("expected code-owned Command Alias, got {other:?}"),
        }
    }
}

#[test]
fn refreshed_zen_models_derive_stripped_aliases_from_the_free_suffix() {
    let models = vec!["brand-new-coder-free".to_string()];
    match resolve_with_provider_models("brand-new-coder-free", &models, &[]).unwrap() {
        ResolvedModel::PinnedRaw { mapping, .. } => {
            assert!(mapping.is_zen_free());
            assert_eq!(mapping.upstream_model, "brand-new-coder-free");
        }
        other => panic!("expected dynamic Zen raw pin, got {other:?}"),
    }
    match resolve_with_provider_models("brand-new-coder", &models, &[]).unwrap() {
        ResolvedModel::Alias {
            alias, mappings, ..
        } => {
            assert_eq!(alias, "brand-new-coder");
            assert_eq!(mappings.len(), 1);
            assert!(mappings[0].is_zen_free());
            assert_eq!(mappings[0].upstream_model, "brand-new-coder-free");
        }
        other => panic!("expected stripped Zen Alias, got {other:?}"),
    }
    let published = published_routeable_aliases_with_zen(&models);
    assert!(
        published
            .iter()
            .any(|entry| entry.alias == "brand-new-coder"
                && entry.owned_by == OPENCODE_ZEN_FREE_PROVIDER_ID)
    );
    assert!(
        !published
            .iter()
            .any(|entry| entry.alias == "brand-new-coder-free")
    );
}

#[test]
fn registry_covers_every_opencode_protocol_id() {
    let aliases = published_aliases();
    let free_models = example_zen_free_catalog();
    for id in supported_model_ids() {
        if id == "big-pickle" || is_free_model(id) {
            continue;
        }
        assert!(
            aliases.iter().any(|alias| alias == id),
            "MODEL_PROTOCOLS id `{id}` must have an alias"
        );
    }
    let published_zen = published_routeable_aliases_with_zen(&free_models);
    for id in &free_models {
        let alias = stripped_free_alias(id).expect("refreshed Zen ids end in -free");
        assert!(
            published_zen.iter().any(|item| item.alias == alias),
            "Zen `-free` catalog rows must publish the stripped alias `{alias}`"
        );
        assert!(resolve_with_example_zen(id).unwrap().routeable_mappings()[0].is_zen_free());
    }
    assert!(!aliases.iter().any(|alias| alias.contains("goat")));
    assert!(
        !aliases
            .iter()
            .any(|alias| alias.contains("unknown-provider"))
    );
    assert!(!aliases.iter().any(|alias| alias.contains("custom")));
}

#[test]
fn published_routeable_aliases_use_routeable_provider_ownership() {
    let published = published_routeable_aliases();
    assert!(!published.is_empty());
    let ids: Vec<&str> = published.iter().map(|item| item.alias.as_str()).collect();
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    assert_eq!(ids, sorted, "GET /v1/models order must be deterministic");
    assert_eq!(
        published.len(),
        published_aliases().len(),
        "builtin aliases currently all have a routeable mapping"
    );
    for item in &published {
        assert!(!looks_raw_shaped(&item.alias));
        assert_ne!(item.alias, COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM);
        match resolve(&item.alias).unwrap() {
            ResolvedModel::Alias { mappings, .. } => {
                let routeable = mappings
                    .iter()
                    .find(|mapping| mapping.routeable)
                    .expect("published alias must have a routeable mapping");
                assert_eq!(item.owned_by, routeable.provider_id);
                assert_ne!(item.owned_by, COMMAND_CODE_PROVIDER_ID);
                assert_ne!(item.owned_by, "unknown-provider");
                assert_ne!(item.owned_by, CUSTOM_PROVIDER_ID);
            }
            other => panic!("published id must be an alias, got {other:?}"),
        }
    }
    let go = published
        .iter()
        .find(|item| item.alias == "glm-5.2")
        .expect("Go alias");
    assert_eq!(go.owned_by, OPENCODE_PROVIDER_ID);
    assert!(
        !published
            .iter()
            .any(|item| item.alias == "deepseek-v4-flash-free")
    );
    let goat_alias = published
        .iter()
        .find(|item| item.alias == COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS)
        .expect("Go still owns the kebab alias");
    assert_eq!(goat_alias.owned_by, OPENCODE_PROVIDER_ID);
    assert!(
        !published
            .iter()
            .any(|item| item.alias.contains('/') || is_free_model(&item.alias))
    );
}

#[test]
fn slash_prefixed_goat_raw_pins_to_command_code_and_does_not_steal_go() {
    match resolve(COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM) {
        Ok(ResolvedModel::PinnedRaw { mapping, .. }) => {
            assert!(mapping.is_command_code_goat());
            assert!(!mapping.routeable);
            assert_eq!(
                mapping.upstream_model,
                COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM
            );
            assert!(
                ResolvedModel::PinnedRaw {
                    requested: COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM.into(),
                    mapping: mapping.clone(),
                }
                .routeable_mappings()
                .is_empty()
            );
        }
        other => panic!("GOAT raw id must uniquely pin to command-code/goat, got {other:?}"),
    }
    match resolve(COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS).unwrap() {
        ResolvedModel::Alias {
            alias, mappings, ..
        } => {
            assert_eq!(alias, COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS);
            assert!(mappings.iter().any(|mapping| mapping.is_opencode_go()));
            assert!(
                mappings
                    .iter()
                    .any(|mapping| mapping.is_command_code_goat() && !mapping.routeable)
            );
            let routeable = mappings
                .iter()
                .filter(|mapping| mapping.routeable)
                .collect::<Vec<_>>();
            assert_eq!(routeable.len(), 1);
            assert!(routeable.iter().any(|mapping| mapping.is_opencode_go()));
            assert_eq!(
                routeable
                    .iter()
                    .find(|mapping| mapping.is_opencode_go())
                    .unwrap()
                    .upstream_model,
                COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS
            );
        }
        other => panic!("expected published Go alias, got {other:?}"),
    }
    match resolve_with_example_zen(COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS).unwrap() {
        ResolvedModel::Alias { mappings, .. } => {
            let routeable = mappings
                .iter()
                .filter(|mapping| mapping.routeable)
                .collect::<Vec<_>>();
            assert_eq!(routeable.len(), 2);
            assert!(routeable.iter().any(|mapping| mapping.is_opencode_go()));
            assert!(routeable.iter().any(|mapping| mapping.is_zen_free()));
        }
        other => panic!("refreshed Zen must join the Go kebab alias, got {other:?}"),
    }
    assert!(is_published_alias(
        COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS
    ));
    assert!(!is_published_alias(
        COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM
    ));
}

#[test]
fn eligible_goat_catalog_joins_static_aliases_and_publishes_new_names() {
    let goat_ids = vec![
        COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM.to_string(),
        "claude-sonnet-4-6".into(),
    ];
    match resolve_with_catalogs(
        COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
        &[],
        &[],
        &goat_ids,
    )
    .unwrap()
    {
        ResolvedModel::PinnedRaw { mapping, .. } => {
            assert!(mapping.is_command_code_goat());
            assert!(mapping.routeable);
        }
        other => panic!("expected routeable GOAT pin, got {other:?}"),
    }
    match resolve_with_catalogs("claude-sonnet-4-6", &[], &[], &goat_ids).unwrap() {
        ResolvedModel::Alias { mappings, .. } => {
            assert_eq!(mappings, vec![goat_mapping("claude-sonnet-4-6", true)]);
        }
        other => panic!("expected catalog-derived GOAT alias, got {other:?}"),
    }
    match resolve_with_catalogs("deepseek-v4-flash", &[], &[], &goat_ids).unwrap() {
        ResolvedModel::Alias { mappings, .. } => {
            assert!(
                mappings
                    .iter()
                    .any(|mapping| mapping.routeable && mapping.is_opencode_go())
            );
            assert!(
                mappings
                    .iter()
                    .any(|mapping| mapping.routeable && mapping.is_command_code_goat())
            );
        }
        other => panic!("GOAT must not steal Go kebab alias, got {other:?}"),
    }
    let published = published_routeable_aliases_with_catalogs(&[], &goat_ids);
    assert!(
        published
            .iter()
            .any(|item| item.alias == "claude-sonnet-4-6"),
        "slash-free Command ids derive their public aliases from the catalog"
    );
    assert!(
        published
            .iter()
            .find(|item| item.alias == COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS)
            .is_some_and(|item| item.owned_by == OPENCODE_PROVIDER_ID)
    );
    assert!(
        !published
            .iter()
            .any(|item| item.alias == COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM)
    );
    match resolve_with_catalogs(COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM, &[], &[], &[])
        .unwrap()
    {
        ResolvedModel::PinnedRaw { mapping, .. } => {
            assert!(mapping.is_command_code_goat());
            assert!(!mapping.routeable);
        }
        other => panic!("empty GOAT catalog must keep the static pin closed, got {other:?}"),
    }
    assert!(
        !published_aliases()
            .iter()
            .any(|alias| alias.contains('/')
                || *alias == COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM)
    );
}

#[test]
fn unknown_names_are_not_aliases() {
    match resolve("definitely-not-a-model") {
        Err(ResolveError::Unknown { requested }) => {
            assert_eq!(requested, "definitely-not-a-model");
            assert!(
                ResolveError::Unknown {
                    requested: requested.clone()
                }
                .code()
                .is_none()
            );
        }
        other => panic!("expected unknown, got {other:?}"),
    }
}

#[test]
fn unique_raw_id_pins_to_one_mapping() {
    let registry = registry_from_entries(vec![
        AliasEntry {
            alias: "widget".into(),
            mappings: vec![go_mapping("widget")],
        },
        AliasEntry {
            alias: "gadget".into(),
            mappings: vec![ProviderMapping {
                provider_id: OPENCODE_PROVIDER_ID.to_string(),
                upstream_model: "vendor.gadget-v1".into(),
                routeable: true,
            }],
        },
    ]);
    match resolve_in(&registry, "vendor.gadget-v1").unwrap() {
        ResolvedModel::PinnedRaw { mapping, .. } => {
            assert_eq!(mapping.upstream_model, "vendor.gadget-v1");
            assert!(mapping.is_opencode_go());
        }
        other => panic!("expected pinned raw, got {other:?}"),
    }
    // Alias still wins when a kebab string is both an alias and a raw ID.
    assert!(matches!(
        resolve_in(&registry, "widget").unwrap(),
        ResolvedModel::Alias { alias, .. } if alias == "widget"
    ));
    // Exact slash-form raw IDs pin without collapsing onto a kebab alias.
    let slash_registry = registry_from_entries(vec![AliasEntry {
        alias: "widget".into(),
        mappings: vec![ProviderMapping {
            provider_id: OPENCODE_PROVIDER_ID.to_string(),
            upstream_model: "vendor/widget-v1".into(),
            routeable: true,
        }],
    }]);
    match resolve_in(&slash_registry, "vendor/widget-v1").unwrap() {
        ResolvedModel::PinnedRaw { mapping, .. } => {
            assert_eq!(mapping.upstream_model, "vendor/widget-v1");
        }
        other => panic!("exact slash raw must pin, got {other:?}"),
    }
    assert!(matches!(
        resolve_in(&slash_registry, "Vendor/widget-v1"),
        Err(ResolveError::Unknown { .. })
    ));
    assert!(matches!(
        resolve_in(&slash_registry, "vendor-widget-v1"),
        Err(ResolveError::Unknown { .. })
    ));
}

#[test]
fn overlapping_raw_ids_return_ambiguous_model_id() {
    let registry = registry_from_entries(vec![
        AliasEntry {
            alias: "alpha".into(),
            mappings: vec![go_mapping("shared-raw")],
        },
        AliasEntry {
            alias: "beta".into(),
            mappings: vec![zen_mapping("shared-raw")],
        },
    ]);
    match resolve_in(&registry, "shared-raw") {
        Err(error) => {
            assert_eq!(error.code(), Some(AMBIGUOUS_MODEL_ID));
            let message = error.message();
            assert!(message.contains(AMBIGUOUS_MODEL_ID));
            assert!(message.contains("shared-raw"));
            assert!(message.contains("opencode:"));
            assert!(message.contains("opencode-zen-free"));
        }
        other => panic!("expected ambiguous, got {other:?}"),
    }
    // Preferred aliases still resolve even when their upstream IDs overlap.
    assert!(matches!(
        resolve_in(&registry, "alpha").unwrap(),
        ResolvedModel::Alias { alias, .. } if alias == "alpha"
    ));
}

#[test]
fn fail_closed_raw_mapping_is_not_routeable() {
    let registry = registry_from_entries(vec![AliasEntry {
        alias: "visible".into(),
        mappings: vec![ProviderMapping {
            provider_id: "command-code".to_string(),
            upstream_model: "goat-only-raw".into(),
            routeable: false,
        }],
    }]);
    match resolve_in(&registry, "goat-only-raw") {
        Ok(ResolvedModel::PinnedRaw { mapping, .. }) => {
            assert!(!mapping.routeable);
            assert_eq!(mapping.provider_id, "command-code");
            assert!(
                ResolvedModel::PinnedRaw {
                    requested: "goat-only-raw".into(),
                    mapping: mapping.clone(),
                }
                .routeable_mappings()
                .is_empty()
            );
        }
        other => {
            panic!("fail-closed unique raw must pin without being routeable, got {other:?}")
        }
    }
    match resolve_in(&registry, "visible").unwrap() {
        ResolvedModel::Alias { mappings, .. } => {
            assert!(!mappings[0].routeable);
            assert!(
                ResolvedModel::Alias {
                    requested: "visible".into(),
                    alias: "visible".into(),
                    mappings: mappings.clone(),
                }
                .routeable_mappings()
                .is_empty()
            );
        }
        other => panic!("expected alias, got {other:?}"),
    }
    assert!(
        published_routeable_in(&registry).is_empty(),
        "fail-closed aliases must stay off GET /v1/models"
    );
}

#[test]
fn catalog_aliases_are_routeable_mappings_in_registry_order() {
    let go = routeable_aliases_for(OPENCODE_PROVIDER_ID);
    let free_models = example_zen_free_catalog();
    assert!(
        routeable_aliases_for(OPENCODE_ZEN_FREE_PROVIDER_ID).is_empty(),
        "unfetched Zen must not publish leftover seed aliases"
    );
    let zen = routeable_aliases_for_with_extended_catalogs(
        OPENCODE_ZEN_FREE_PROVIDER_ID,
        &free_models,
        &[],
        &[],
        &[],
    );
    assert!(!go.is_empty());
    assert!(!zen.is_empty());
    let mut sorted_go = go.clone();
    sorted_go.sort_unstable();
    assert_eq!(go, sorted_go, "catalog aliases must be deterministic");
    let mut sorted_zen = zen.clone();
    sorted_zen.sort_unstable();
    assert_eq!(zen, sorted_zen);

    for alias in go.iter().chain(zen.iter()) {
        assert!(!looks_raw_shaped(alias));
        assert_ne!(*alias, COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM);
        assert!(!alias.contains('/'));
    }
    assert!(go.iter().any(|alias| alias == "glm-5.2"));
    assert!(
        go.iter()
            .any(|alias| alias == COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS)
    );
    assert!(go.iter().any(|alias| alias == "minimax-m2.7-highspeed"));
    assert!(!go.iter().any(|alias| alias == "deepseek-v4-flash-free"));
    assert!(!zen.iter().any(|alias| alias == "glm-5.2"));
    assert!(zen.iter().any(|alias| alias == "deepseek-v4-flash"));
    assert!(
        zen.iter()
            .any(|alias| alias == "muse-spark-1.3-contributor"),
        "Zen-only `-free` rows still publish a stripped Alias"
    );
    assert!(!zen.iter().any(|alias| alias.ends_with("-free")));
    for id in &free_models {
        let alias = stripped_free_alias(id).expect("seeded Zen ids end in -free");
        assert!(
            zen.iter().any(|item| item == alias),
            "Zen catalog must include stripped `{id}` alias"
        );
        assert!(
            !go.iter().any(|item| item == id),
            "Go catalog must not include free `{id}`"
        );
    }
    for id in supported_model_ids().filter(|id| !is_free_model(id)) {
        if id != "big-pickle" {
            assert!(
                go.iter().any(|alias| alias == id),
                "Go catalog must include `{id}`"
            );
        }
        let has_free_twin = free_models
            .iter()
            .any(|free| stripped_free_alias(free).is_some_and(|alias| alias == id));
        assert_eq!(
            zen.iter().any(|alias| alias == id),
            has_free_twin,
            "Zen stripped aliases must match the refreshed Free catalog for `{id}`"
        );
    }

    assert!(routeable_aliases_for(COMMAND_CODE_PROVIDER_ID).is_empty());
    assert!(
        routeable_aliases_for(CUSTOM_PROVIDER_ID).is_empty(),
        "Custom catalog aliases stay empty; client IDs come from account capabilities"
    );
}

#[test]
fn catalog_aliases_keep_every_routeable_provider_not_first_wins_owner() {
    let registry = registry_from_entries(vec![AliasEntry {
        alias: "shared".into(),
        mappings: vec![zen_mapping("shared"), go_mapping("shared")],
    }]);
    let published = published_routeable_in(&registry);
    assert_eq!(published.len(), 1);
    assert_eq!(published[0].alias, "shared");
    assert_eq!(
        published[0].owned_by, OPENCODE_ZEN_FREE_PROVIDER_ID,
        "GET /v1/models owned_by stays first-wins"
    );
    assert_eq!(
        routeable_aliases_for_in(&registry, OPENCODE_PROVIDER_ID),
        ["shared"]
    );
    assert_eq!(
        routeable_aliases_for_in(&registry, OPENCODE_ZEN_FREE_PROVIDER_ID),
        ["shared"]
    );
    assert!(routeable_aliases_for_in(&registry, COMMAND_CODE_PROVIDER_ID).is_empty());
}

#[test]
fn custom_overlay_does_not_steal_published_aliases_and_resolves_unknown_ids() {
    match resolve_with_custom("glm-5.2", &["glm-5.2".into()]).unwrap() {
        ResolvedModel::Alias {
            alias, mappings, ..
        } => {
            assert_eq!(alias, "glm-5.2");
            assert!(mappings.iter().any(|mapping| mapping.is_opencode_go()));
            assert!(mappings.iter().any(|mapping| mapping.is_custom_api()));
            let routeable = mappings
                .iter()
                .filter(|mapping| mapping.routeable)
                .collect::<Vec<_>>();
            assert!(routeable.iter().any(|mapping| mapping.is_opencode_go()));
            assert!(routeable.iter().any(|mapping| mapping.is_custom_api()));
        }
        other => panic!("expected alias overlay, got {other:?}"),
    }
    match resolve_with_custom("my-local-model", &["my-local-model".into()]).unwrap() {
        ResolvedModel::Alias {
            alias, mappings, ..
        } => {
            assert_eq!(alias, "my-local-model");
            assert_eq!(mappings.len(), 1);
            assert!(mappings[0].is_custom_api());
            assert!(mappings[0].routeable);
        }
        other => panic!("expected custom-only alias, got {other:?}"),
    }
    match resolve_with_custom("org/model", &["org/model".into()]).unwrap() {
        ResolvedModel::Alias { alias, .. } => assert_eq!(alias, "org/model"),
        other => panic!("expected raw-shaped Custom public alias, got {other:?}"),
    }
    match resolve_with_custom(
        COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
        &[COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM.into()],
    ) {
        Err(error) => {
            assert_eq!(error.code(), Some(AMBIGUOUS_MODEL_ID));
            assert!(error.message().contains("command-code:"));
            assert!(error.message().contains("custom:"));
        }
        other => panic!("GOAT raw overlapping Custom must be ambiguous, got {other:?}"),
    }
    assert!(matches!(
        resolve_with_custom("definitely-not-a-model", &[]),
        Err(ResolveError::Unknown { .. })
    ));
}

#[test]
fn refreshed_go_catalog_adds_raw_pins_without_expanding_alias_authority() {
    let go = vec!["future-go-model".to_string(), "vendor/raw-go".to_string()];
    match resolve_with_all_catalogs("future-go-model", &go, &[], &[], &[]).unwrap() {
        ResolvedModel::PinnedRaw { mapping, .. } => {
            assert!(mapping.is_opencode_go());
            assert_eq!(mapping.upstream_model, "future-go-model");
        }
        other => panic!("expected dynamic raw Go pin, got {other:?}"),
    }
    match resolve_with_all_catalogs("vendor/raw-go", &go, &[], &[], &[]).unwrap() {
        ResolvedModel::PinnedRaw { mapping, .. } => {
            assert!(mapping.is_opencode_go());
            assert_eq!(mapping.upstream_model, "vendor/raw-go");
        }
        other => panic!("expected dynamic raw Go pin, got {other:?}"),
    }

    let published = published_routeable_aliases_with_all_catalogs(&go, &[], &[]);
    assert!(!published.iter().any(|item| item.alias == "future-go-model"));
    assert!(!published.iter().any(|item| item.alias == "vendor/raw-go"));

    let goat = vec!["vendor/raw-go".to_string()];
    let overlap = resolve_with_all_catalogs("vendor/raw-go", &go, &[], &[], &goat)
        .expect_err("overlapping raw provider IDs must remain ambiguous");
    assert_eq!(overlap.code(), Some(AMBIGUOUS_MODEL_ID));
}

#[test]
fn cn_catalog_names_are_data_driven_and_collisions_keep_exact_pins() {
    let minimax = vec![
        "MiniMax-Future-99".to_string(),
        "vendor/shared_leaf".to_string(),
        "shared-leaf".to_string(),
    ];
    let kimi = vec![
        "kimi-for-coding".to_string(),
        "kimi-for-coding-highspeed".to_string(),
        "k3".to_string(),
        "kimi-future-99".to_string(),
    ];
    let catalogs = RuntimeCatalogs {
        minimax: &minimax,
        kimi: &kimi,
        ..RuntimeCatalogs::default()
    };
    let index = RuntimeCatalogIndex::from_catalogs(catalogs);
    for (provider, upstream, alias) in [
        (
            MINIMAX_PROVIDER_ID,
            "MiniMax-Future-99",
            "minimax-future-99",
        ),
        (KIMI_PROVIDER_ID, "kimi-future-99", "kimi-future-99"),
        (KIMI_PROVIDER_ID, "kimi-for-coding", "kimi-for-coding"),
        (
            KIMI_PROVIDER_ID,
            "kimi-for-coding-highspeed",
            "kimi-for-coding-highspeed",
        ),
        (KIMI_PROVIDER_ID, "k3", "k3"),
    ] {
        assert_eq!(
            catalog_aliases(provider, catalogs)
                .get(upstream)
                .map(String::as_str),
            Some(alias)
        );
        assert!(
            index
                .published_models()
                .iter()
                .any(|row| row.alias == alias)
        );
        assert!(
            index
                .resolve(alias)
                .unwrap()
                .routeable_mappings()
                .iter()
                .any(
                    |mapping| mapping.provider_id == provider && mapping.upstream_model == upstream
                )
        );
    }
    assert!(!catalog_aliases(MINIMAX_PROVIDER_ID, catalogs).contains_key("vendor/shared_leaf"));
    assert!(
        matches!(index.resolve("vendor/shared_leaf"), Ok(ResolvedModel::PinnedRaw { mapping, .. }) if mapping.is_minimax_cn())
    );
    assert!(
        matches!(index.resolve("MiniMax-Future-99"), Ok(ResolvedModel::PinnedRaw { mapping, .. }) if mapping.is_minimax_cn())
    );
    for fixed_version in ["kimi-k2.7-code", "kimi-k2.7-code-highspeed"] {
        if let Ok(resolved) = index.resolve(fixed_version) {
            assert!(
                resolved
                    .routeable_mappings()
                    .iter()
                    .all(|mapping| !mapping.is_kimi_cn())
            );
        }
    }
    let shared = vec!["vendor/shared".to_string()];
    assert_eq!(
        resolve_with_extended_catalogs("vendor/shared", &[], &[], &[], &[], &shared, &shared)
            .unwrap_err()
            .code(),
        Some(AMBIGUOUS_MODEL_ID)
    );
}

#[test]
fn arbitrary_ollama_stems_tags_and_pin_selection_are_catalog_driven() {
    let models = vec![
        "future-a:2099".to_string(),
        "future-b:20b".to_string(),
        "future-b:120b".to_string(),
    ];
    let catalogs = RuntimeCatalogs {
        ollama: &models,
        ..RuntimeCatalogs::default()
    };
    let aliases = catalog_aliases(OLLAMA_PROVIDER_ID, catalogs);
    assert_eq!(aliases["future-a:2099"], "future-a");
    assert_eq!(aliases["future-b:20b"], "future-b-20b");
    assert_eq!(aliases["future-b:120b"], "future-b-120b");
    assert!(resolve_with_runtime_catalogs("future-b", catalogs).is_err());
    assert!(
        matches!(resolve_with_runtime_catalogs("future-b:20b", catalogs), Ok(ResolvedModel::PinnedRaw { mapping, .. }) if mapping.upstream_model == "future-b:20b")
    );
    let pinned = [models[1].clone()];
    let pinned_catalogs = RuntimeCatalogs {
        ollama_pinned: &pinned,
        ..catalogs
    };
    assert_eq!(
        catalog_aliases(OLLAMA_PROVIDER_ID, pinned_catalogs)["future-b:20b"],
        "future-b"
    );
    assert!(
        resolve_with_runtime_catalogs("future-b", pinned_catalogs)
            .unwrap()
            .routeable_mappings()
            .iter()
            .any(|mapping| mapping.upstream_model == "future-b:20b")
    );
    for id in &models {
        let alias = &aliases[id];
        assert!(
            RuntimeCatalogIndex::from_catalogs(catalogs)
                .published_models()
                .iter()
                .any(|row| &row.alias == alias)
        );
    }
}

#[test]
fn cpa_catalog_joins_code_owned_aliases_and_keeps_raw_ids_exact_and_fail_closed() {
    let cpa = vec![
        "GLM-5.2".to_string(),
        "cpa-raw-model".to_string(),
        "vendor/cpa-raw".to_string(),
    ];
    let catalogs = RuntimeCatalogs {
        cpa: &cpa,
        ..RuntimeCatalogs::default()
    };
    match resolve_with_runtime_catalogs("glm-5.2", catalogs).unwrap() {
        ResolvedModel::Alias {
            alias, mappings, ..
        } => {
            assert_eq!(alias, "glm-5.2");
            assert!(mappings.iter().any(|mapping| {
                mapping.is_cpa() && mapping.upstream_model == "GLM-5.2" && mapping.routeable
            }));
        }
        other => panic!("CPA code-owned Alias must join, got {other:?}"),
    }
    for raw in ["cpa-raw-model", "vendor/cpa-raw"] {
        match resolve_with_runtime_catalogs(raw, catalogs).unwrap() {
            ResolvedModel::PinnedRaw { mapping, .. } => {
                assert!(mapping.is_cpa());
                assert_eq!(mapping.upstream_model, raw);
            }
            other => panic!("CPA unknown catalog id must remain raw, got {other:?}"),
        }
    }
    let published = published_routeable_aliases_with_runtime_catalogs(catalogs);
    assert!(published.iter().any(|item| item.alias == "glm-5.2"));
    assert!(!published.iter().any(|item| item.alias == "cpa-raw-model"));
    assert!(!published.iter().any(|item| item.alias == "vendor/cpa-raw"));
    assert_eq!(
        routeable_aliases_for_with_runtime_catalogs(CPA_PROVIDER_ID, catalogs),
        ["glm-5.2"]
    );

    let shared = vec!["vendor/shared".to_string()];
    let conflict = resolve_with_runtime_catalogs(
        "vendor/shared",
        RuntimeCatalogs {
            go: &shared,
            cpa: &shared,
            ..RuntimeCatalogs::default()
        },
    )
    .unwrap_err();
    assert_eq!(conflict.code(), Some(AMBIGUOUS_MODEL_ID));
}

#[test]
fn cpa_and_other_catalogs_share_an_exact_public_name_without_merging_other_raw_pins() {
    let id = "gpt-6-sol".to_string();
    let go = vec![id.clone()];
    let cpa = vec![id.clone()];
    let http = ExtraProviderCatalog {
        provider_id: "11111111-1111-4111-8111-111111111111".into(),
        mappings: vec![(id.clone(), id.clone())],
    };
    let extras = [http.clone()];
    let catalogs = RuntimeCatalogs {
        go: &go,
        cpa: &cpa,
        extra: &extras,
        ..RuntimeCatalogs::default()
    };
    match resolve_with_runtime_catalogs(&id, catalogs).unwrap() {
        ResolvedModel::Alias {
            alias, mappings, ..
        } => {
            assert_eq!(alias, id);
            assert!(mappings.iter().any(ProviderMapping::is_cpa));
            assert!(mappings.iter().any(ProviderMapping::is_opencode_go));
            assert!(
                mappings
                    .iter()
                    .any(|mapping| mapping.provider_id == http.provider_id)
            );
        }
        other => panic!("expected a shared public Alias, got {other:?}"),
    }
    assert_eq!(
        published_routeable_models_with_runtime_catalogs(catalogs)
            .iter()
            .filter(|item| item.alias == id)
            .count(),
        1
    );

    let http_and_cpa = RuntimeCatalogs {
        cpa: &cpa,
        extra: &extras,
        ..RuntimeCatalogs::default()
    };
    assert!(matches!(
        resolve_with_runtime_catalogs(&id, http_and_cpa),
        Ok(ResolvedModel::Alias { mappings, .. })
            if mappings.iter().any(ProviderMapping::is_cpa)
                && mappings.iter().any(|mapping| mapping.provider_id == http.provider_id)
    ));
    assert!(
        published_routeable_models_with_runtime_catalogs(http_and_cpa)
            .iter()
            .any(|item| item.alias == id)
    );

    let differently_named = [ExtraProviderCatalog {
        mappings: vec![("other-public-name".into(), id.clone())],
        ..http
    }];
    let error = resolve_with_runtime_catalogs(
        &id,
        RuntimeCatalogs {
            cpa: &cpa,
            extra: &differently_named,
            ..RuntimeCatalogs::default()
        },
    )
    .unwrap_err();
    assert_eq!(error.code(), Some(AMBIGUOUS_MODEL_ID));
}

#[test]
fn extra_catalogs_aggregate_public_aliases_and_fail_closed_on_raw_ambiguity() {
    let one = ExtraProviderCatalog {
        provider_id: "11111111-1111-4111-8111-111111111111".into(),
        mappings: vec![("lab-opus".into(), "vendor/opus".into())],
    };
    let two = ExtraProviderCatalog {
        provider_id: "22222222-2222-4222-8222-222222222222".into(),
        mappings: vec![("lab-opus".into(), "other/opus".into())],
    };
    let extras = [one.clone(), two.clone()];
    match resolve_with_runtime_catalogs(
        "lab-opus",
        RuntimeCatalogs {
            extra: &extras,
            ..RuntimeCatalogs::default()
        },
    )
    .unwrap()
    {
        ResolvedModel::Alias { mappings, .. } => {
            assert_eq!(mappings.len(), 2);
            assert!(
                mappings
                    .iter()
                    .any(|mapping| mapping.provider_id == one.provider_id)
            );
            assert!(
                mappings
                    .iter()
                    .any(|mapping| mapping.provider_id == two.provider_id)
            );
        }
        other => panic!("expected aggregated alias, got {other:?}"),
    }

    let raw_conflict = ExtraProviderCatalog {
        provider_id: "33333333-3333-4333-8333-333333333333".into(),
        mappings: vec![("other-public".into(), "vendor/opus".into())],
    };
    let err = resolve_with_runtime_catalogs(
        "vendor/opus",
        RuntimeCatalogs {
            extra: &[one, raw_conflict],
            ..RuntimeCatalogs::default()
        },
    )
    .unwrap_err();
    assert_eq!(err.code(), Some(AMBIGUOUS_MODEL_ID));
}

#[test]
fn extra_catalogs_keep_raw_shaped_public_to_upstream_mapping() {
    let extra = ExtraProviderCatalog {
        provider_id: "11111111-1111-4111-8111-111111111111".into(),
        mappings: vec![
            ("org/same".into(), "org/same".into()),
            ("org/public".into(), "vendor/real".into()),
            ("lab_model".into(), "vendor/lab".into()),
            ("lab model".into(), "vendor/space".into()),
            ("glm-5.2".into(), "vendor/glm".into()),
        ],
    };
    let catalogs = RuntimeCatalogs {
        extra: std::slice::from_ref(&extra),
        ..RuntimeCatalogs::default()
    };

    match resolve_with_runtime_catalogs("org/same", catalogs).unwrap() {
        ResolvedModel::PinnedRaw { mapping, .. } => {
            assert_eq!(mapping.provider_id, extra.provider_id);
            assert_eq!(mapping.upstream_model, "org/same");
            assert!(mapping.routeable);
        }
        other => panic!("raw public==upstream must pin, got {other:?}"),
    }
    let public_mappings = match resolve_with_runtime_catalogs("org/public", catalogs).unwrap() {
        ResolvedModel::Alias { mappings, .. } => mappings,
        ResolvedModel::PinnedRaw { mapping, .. } => vec![mapping],
    };
    assert_eq!(public_mappings.len(), 1);
    assert_eq!(public_mappings[0].provider_id, extra.provider_id);
    assert_eq!(public_mappings[0].upstream_model, "vendor/real");
    let underscore_upstream = match resolve_with_runtime_catalogs("lab_model", catalogs).unwrap() {
        ResolvedModel::Alias { mappings, .. } => mappings[0].upstream_model.clone(),
        ResolvedModel::PinnedRaw { mapping, .. } => mapping.upstream_model,
    };
    assert_eq!(underscore_upstream, "vendor/lab");
    let space_upstream = match resolve_with_runtime_catalogs("lab model", catalogs).unwrap() {
        ResolvedModel::Alias { mappings, .. } => mappings[0].upstream_model.clone(),
        ResolvedModel::PinnedRaw { mapping, .. } => mapping.upstream_model,
    };
    assert_eq!(space_upstream, "vendor/space");

    let published = published_routeable_aliases_with_runtime_catalogs(catalogs);
    assert!(
        published
            .iter()
            .any(|item| item.alias == "glm-5.2" && item.owned_by == OPENCODE_PROVIDER_ID),
        "dynamic public names must not steal code-owned aliases: {published:?}"
    );
    for leaked in [
        "org/same",
        "org/public",
        "vendor/real",
        "lab_model",
        "lab model",
    ] {
        assert!(
            !published.iter().any(|item| item.alias == leaked),
            "raw-shaped extra ids stay outside the Alias-only list: {leaked}"
        );
    }

    match resolve_with_runtime_catalogs("glm-5.2", catalogs).unwrap() {
        ResolvedModel::Alias { mappings, .. } => {
            assert!(mappings.iter().any(ProviderMapping::is_opencode_go));
            assert!(mappings.iter().any(|mapping| {
                mapping.provider_id == extra.provider_id && mapping.upstream_model == "vendor/glm"
            }));
        }
        other => panic!("code-owned alias must keep Go and join extra, got {other:?}"),
    }
}

#[test]
fn extra_catalog_raw_public_conflicts_stay_ambiguous() {
    let one = ExtraProviderCatalog {
        provider_id: "11111111-1111-4111-8111-111111111111".into(),
        mappings: vec![("shared/raw".into(), "shared/raw".into())],
    };
    let two = ExtraProviderCatalog {
        provider_id: "22222222-2222-4222-8222-222222222222".into(),
        mappings: vec![("shared/raw".into(), "shared/raw".into())],
    };
    let catalogs = RuntimeCatalogs {
        extra: &[one, two],
        ..RuntimeCatalogs::default()
    };
    let err = resolve_with_runtime_catalogs("shared/raw", catalogs).unwrap_err();
    assert_eq!(err.code(), Some(AMBIGUOUS_MODEL_ID));
    assert!(
        !published_routeable_aliases_with_runtime_catalogs(catalogs)
            .iter()
            .any(|item| item.alias == "shared/raw")
    );
}

fn extra_ab_shared_model_collision(order: [ExtraProviderCatalog; 2]) {
    let extras = order;
    let err = resolve_with_runtime_catalogs(
        "shared-model",
        RuntimeCatalogs {
            extra: &extras,
            ..RuntimeCatalogs::default()
        },
    )
    .unwrap_err();
    assert_eq!(err.code(), Some(AMBIGUOUS_MODEL_ID));
    match err {
        ResolveError::Ambiguous { mappings, .. } => {
            assert!(
                mappings
                    .iter()
                    .any(|mapping| mapping.provider_id == extras[0].provider_id
                        || mapping.provider_id == extras[1].provider_id)
            );
            assert!(
                mappings.len() >= 2,
                "raw/public collision must not pick a single extra, got {mappings:?}"
            );
        }
        other => panic!("expected Ambiguous, got {other:?}"),
    }
}

#[test]
fn extra_catalogs_fail_closed_when_public_alias_collides_with_another_upstream() {
    let extra_a = ExtraProviderCatalog {
        provider_id: "11111111-1111-4111-8111-111111111111".into(),
        mappings: vec![("shared-model".into(), "vendor-a".into())],
    };
    let extra_b = ExtraProviderCatalog {
        provider_id: "22222222-2222-4222-8222-222222222222".into(),
        mappings: vec![("other-model".into(), "shared-model".into())],
    };
    extra_ab_shared_model_collision([extra_a.clone(), extra_b.clone()]);
    extra_ab_shared_model_collision([extra_b.clone(), extra_a.clone()]);

    match resolve_with_runtime_catalogs(
        "other-model",
        RuntimeCatalogs {
            extra: &[extra_a.clone(), extra_b.clone()],
            ..RuntimeCatalogs::default()
        },
    )
    .unwrap()
    {
        ResolvedModel::Alias { mappings, .. } => {
            assert_eq!(mappings.len(), 1);
            assert_eq!(mappings[0].provider_id, extra_b.provider_id);
            assert_eq!(mappings[0].upstream_model, "shared-model");
        }
        other => panic!("other-model stays B's public alias, got {other:?}"),
    }

    match resolve_with_runtime_catalogs(
        "vendor-a",
        RuntimeCatalogs {
            extra: &[extra_a.clone(), extra_b],
            ..RuntimeCatalogs::default()
        },
    )
    .unwrap()
    {
        ResolvedModel::PinnedRaw { mapping, .. } => {
            assert_eq!(mapping.provider_id, extra_a.provider_id);
            assert_eq!(mapping.upstream_model, "vendor-a");
        }
        other => panic!("unique raw vendor-a must pin, got {other:?}"),
    }
}

#[test]
fn extra_catalogs_fail_closed_on_builtin_alias_upstream_collision() {
    let extra = ExtraProviderCatalog {
        provider_id: "11111111-1111-4111-8111-111111111111".into(),
        mappings: vec![("other-model".into(), "glm-5.2".into())],
    };
    let err = resolve_with_runtime_catalogs(
        "glm-5.2",
        RuntimeCatalogs {
            extra: std::slice::from_ref(&extra),
            ..RuntimeCatalogs::default()
        },
    )
    .unwrap_err();
    assert_eq!(err.code(), Some(AMBIGUOUS_MODEL_ID));
}

#[test]
fn extra_catalogs_fail_closed_when_same_provider_raw_precedes_public() {
    let extra = ExtraProviderCatalog {
        provider_id: "11111111-1111-4111-8111-111111111111".into(),
        mappings: vec![
            ("other-model".into(), "shared-model".into()),
            ("shared-model".into(), "vendor-a".into()),
        ],
    };
    let err = resolve_with_runtime_catalogs(
        "shared-model",
        RuntimeCatalogs {
            extra: std::slice::from_ref(&extra),
            ..RuntimeCatalogs::default()
        },
    )
    .unwrap_err();
    assert_eq!(err.code(), Some(AMBIGUOUS_MODEL_ID));

    let extra_public_first = ExtraProviderCatalog {
        provider_id: extra.provider_id.clone(),
        mappings: vec![
            ("shared-model".into(), "vendor-a".into()),
            ("other-model".into(), "shared-model".into()),
        ],
    };
    let err = resolve_with_runtime_catalogs(
        "shared-model",
        RuntimeCatalogs {
            extra: std::slice::from_ref(&extra_public_first),
            ..RuntimeCatalogs::default()
        },
    )
    .unwrap_err();
    assert_eq!(err.code(), Some(AMBIGUOUS_MODEL_ID));
}

#[test]
fn extra_catalogs_keep_canonical_public_match_without_stealing_raw() {
    let extra = ExtraProviderCatalog {
        provider_id: "11111111-1111-4111-8111-111111111111".into(),
        mappings: vec![("Shared-Model".into(), "vendor-a".into())],
    };
    match resolve_with_runtime_catalogs(
        "shared-model",
        RuntimeCatalogs {
            extra: std::slice::from_ref(&extra),
            ..RuntimeCatalogs::default()
        },
    )
    .unwrap()
    {
        ResolvedModel::Alias { mappings, .. } => {
            assert_eq!(mappings.len(), 1);
            assert_eq!(mappings[0].provider_id, extra.provider_id);
            assert_eq!(mappings[0].upstream_model, "vendor-a");
        }
        other => panic!("canonical public match must resolve, got {other:?}"),
    }
}

#[test]
fn extra_catalogs_allow_multiple_public_names_for_the_same_raw_target() {
    for requested in ["shared-model", "org/public"] {
        let mut extra = ExtraProviderCatalog {
            provider_id: "11111111-1111-4111-8111-111111111111".into(),
            mappings: vec![
                (requested.into(), requested.into()),
                ("other-model".into(), requested.into()),
            ],
        };
        for _ in 0..2 {
            let resolved = resolve_with_runtime_catalogs(
                requested,
                RuntimeCatalogs {
                    extra: std::slice::from_ref(&extra),
                    ..RuntimeCatalogs::default()
                },
            )
            .unwrap();
            let mappings = resolved.routeable_mappings();
            assert_eq!(mappings.len(), 1);
            assert_eq!(mappings[0].provider_id, extra.provider_id);
            assert_eq!(mappings[0].upstream_model, requested);
            extra.mappings.reverse();
        }
    }
}

#[test]
fn extra_catalogs_reject_same_provider_raw_shaped_public_target_conflicts() {
    let mut extra = ExtraProviderCatalog {
        provider_id: "11111111-1111-4111-8111-111111111111".into(),
        mappings: vec![
            ("org/public".into(), "vendor/real".into()),
            ("other-model".into(), "org/public".into()),
        ],
    };
    for _ in 0..2 {
        let error = resolve_with_runtime_catalogs(
            "org/public",
            RuntimeCatalogs {
                extra: std::slice::from_ref(&extra),
                ..RuntimeCatalogs::default()
            },
        )
        .unwrap_err();
        assert_eq!(error.code(), Some(AMBIGUOUS_MODEL_ID));
        let ResolveError::Ambiguous { mappings, .. } = error else {
            unreachable!()
        };
        assert_eq!(mappings.len(), 2);
        assert!(
            mappings
                .iter()
                .any(|mapping| mapping.upstream_model == "vendor/real")
        );
        assert!(
            mappings
                .iter()
                .any(|mapping| mapping.upstream_model == "org/public")
        );
        extra.mappings.reverse();
    }
}

fn resolve_ollama(
    requested: &str,
    ollama: &[&str],
    pinned: &[&str],
) -> Result<ResolvedModel, ResolveError> {
    let ollama_models: Vec<String> = ollama.iter().map(|id| id.to_string()).collect();
    let pinned_models: Vec<String> = pinned.iter().map(|id| id.to_string()).collect();
    resolve_with_runtime_catalogs(
        requested,
        RuntimeCatalogs {
            ollama: &ollama_models,
            ollama_pinned: &pinned_models,
            ..RuntimeCatalogs::default()
        },
    )
}

#[test]
fn ollama_overlay_appends_shared_alias_mappings_without_stealing_publication() {
    match resolve_ollama(
        "deepseek-v4-flash",
        &["deepseek-v4-flash:0731", "gpt-oss:20b", "gpt-oss:120b"],
        &[],
    )
    .unwrap()
    {
        ResolvedModel::Alias {
            alias, mappings, ..
        } => {
            assert_eq!(alias, "deepseek-v4-flash");
            assert!(
                mappings.iter().any(ProviderMapping::is_opencode_go),
                "Go keeps owning the shared alias"
            );
            assert!(mappings.iter().any(|mapping| {
                mapping.is_ollama_cloud()
                    && mapping.routeable
                    && mapping.upstream_model == "deepseek-v4-flash:0731"
            }));
            assert!(
                !mappings.iter().any(|mapping| {
                    mapping.is_ollama_cloud() && mapping.upstream_model == "gpt-oss:20b"
                }),
                "size-variant stems must not bind the shared alias"
            );
        }
        other => panic!("expected shared alias, got {other:?}"),
    }

    match resolve_ollama("gpt-oss:20b", &["gpt-oss:20b", "gpt-oss:120b"], &[]).unwrap() {
        ResolvedModel::PinnedRaw { mapping, .. } => {
            assert!(mapping.is_ollama_cloud());
            assert_eq!(mapping.upstream_model, "gpt-oss:20b");
        }
        other => panic!("expected raw pin, got {other:?}"),
    }
    assert!(resolve_ollama("gpt-oss", &["gpt-oss:20b", "gpt-oss:120b"], &[]).is_err());
}

#[test]
fn ollama_overlay_coexisting_tags_fail_closed_until_pinned() {
    let coexisting = ["deepseek-v4-flash:0731", "deepseek-v4-flash:0915"];
    match resolve_ollama(COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS, &coexisting, &[]).unwrap() {
        ResolvedModel::Alias { mappings, .. } => {
            assert!(
                !mappings.iter().any(ProviderMapping::is_ollama_cloud),
                "coexisting tags must not guess a shared-alias binding"
            );
        }
        other => panic!("expected shared alias, got {other:?}"),
    }
    match resolve_ollama(
        "deepseek-v4-flash",
        &coexisting,
        &["deepseek-v4-flash:0915"],
    )
    .unwrap()
    {
        ResolvedModel::Alias { mappings, .. } => {
            let ollama: Vec<_> = mappings
                .iter()
                .filter(|mapping| mapping.is_ollama_cloud())
                .collect();
            assert_eq!(ollama.len(), 1);
            assert_eq!(ollama[0].upstream_model, "deepseek-v4-flash:0915");
        }
        other => panic!("expected pinned shared alias, got {other:?}"),
    }
}

#[test]
fn public_go_catalog_names_include_new_pins_without_creating_shared_aliases() {
    let go = vec!["future-go-model".to_string(), "vendor/model-x".to_string()];
    let catalogs = RuntimeCatalogs {
        go: &go,
        ..RuntimeCatalogs::default()
    };
    let published = published_routeable_models_with_runtime_catalogs(catalogs);
    for id in &go {
        assert!(
            published
                .iter()
                .any(|item| item.alias == *id && item.owned_by == OPENCODE_PROVIDER_ID)
        );
        assert!(
            matches!(resolve_with_runtime_catalogs(id, catalogs), Ok(ResolvedModel::PinnedRaw { mapping, .. }) if mapping.is_opencode_go())
        );
        assert!(
            !published_routeable_aliases_with_runtime_catalogs(catalogs)
                .iter()
                .any(|item| item.alias == *id)
        );
    }
    let custom = vec!["future-go-model".to_string()];
    let conflicting = RuntimeCatalogs {
        custom: &custom,
        ..catalogs
    };
    assert!(
        !published_routeable_models_with_runtime_catalogs(conflicting)
            .iter()
            .any(|item| item.alias == "future-go-model")
    );
    assert!(resolve_with_runtime_catalogs("not-in-catalog", catalogs).is_err());
}

#[test]
fn builtin_alias_overrides_preserve_exact_upstreams_for_every_adapter() {
    for (provider, upstream, old_alias) in [
        (OPENCODE_PROVIDER_ID, "glm-5.2", "glm-5.2"),
        (OPENCODE_ZEN_FREE_PROVIDER_ID, "glm-5.2-free", "glm-5.2"),
        (COMMAND_CODE_PROVIDER_ID, "vendor/glm-5.2", "glm-5.2"),
        (MINIMAX_PROVIDER_ID, "MiniMax-M3", "minimax-m3"),
        (KIMI_PROVIDER_ID, "k3", "kimi-k3"),
        (OLLAMA_PROVIDER_ID, "glm-5.2:cloud", "glm-5.2"),
    ] {
        let models = [upstream.to_string()];
        let overrides = [ExtraProviderCatalog {
            provider_id: provider.into(),
            mappings: vec![("My-Alias".into(), upstream.into())],
        }];
        let mut catalogs = RuntimeCatalogs {
            builtin_aliases: &overrides,
            ..RuntimeCatalogs::default()
        };
        match provider {
            OPENCODE_PROVIDER_ID => catalogs.go = &models,
            OPENCODE_ZEN_FREE_PROVIDER_ID => catalogs.zen_free = &models,
            COMMAND_CODE_PROVIDER_ID => catalogs.command_code = &models,
            MINIMAX_PROVIDER_ID => catalogs.minimax = &models,
            KIMI_PROVIDER_ID => catalogs.kimi = &models,
            OLLAMA_PROVIDER_ID => catalogs.ollama = &models,
            _ => unreachable!(),
        }
        for requested in ["My-Alias", "my-alias"] {
            let resolved = resolve_with_runtime_catalogs(requested, catalogs).unwrap();
            assert!(matches!(&resolved, ResolvedModel::Alias { alias, .. } if alias == "My-Alias"));
            assert_eq!(
                resolved.routeable_mappings(),
                vec![&mapping(provider, upstream, true)]
            );
        }
        let raw = resolve_with_runtime_catalogs(upstream, catalogs).unwrap();
        assert!(
            matches!(raw, ResolvedModel::PinnedRaw { mapping, .. } if mapping.provider_id == provider && mapping.upstream_model == upstream && mapping.routeable)
        );
        let published = published_routeable_models_with_runtime_catalogs(catalogs);
        assert!(
            published
                .iter()
                .any(|item| item.alias == "My-Alias" && item.owned_by == provider)
        );
        let registry = build_runtime_registry(catalogs);
        assert!(!registry.aliases.get(old_alias).is_some_and(|entry| {
            entry.mappings.iter().any(|mapping| {
                mapping.provider_id == provider && mapping.upstream_model == upstream
            })
        }));
        if provider != OPENCODE_PROVIDER_ID
            && let Ok(old) = resolve_with_runtime_catalogs(old_alias, catalogs)
        {
            assert!(
                !old.routeable_mappings()
                    .iter()
                    .any(|mapping| mapping.provider_id == provider)
            );
        }
    }
}

#[test]
fn builtin_override_removes_only_its_mapping_and_joins_existing_public_names() {
    let go = ["glm-5.2".into()];
    let zen = ["glm-5.2-free".into()];
    let goat = ["vendor/glm-5.2".to_string()];
    let overrides = [ExtraProviderCatalog {
        provider_id: COMMAND_CODE_PROVIDER_ID.into(),
        mappings: vec![("new-shared".into(), goat[0].clone())],
    }];
    let extras = [ExtraProviderCatalog {
        provider_id: "http-provider".into(),
        mappings: vec![("new-shared".into(), "vendor/http".into())],
    }];
    let cpa = ["new-shared".into()];
    let catalogs = RuntimeCatalogs {
        go: &go,
        zen_free: &zen,
        command_code: &goat,
        cpa: &cpa,
        builtin_aliases: &overrides,
        extra: &extras,
        ..RuntimeCatalogs::default()
    };
    let old = resolve_with_runtime_catalogs("glm-5.2", catalogs).unwrap();
    assert!(
        old.routeable_mappings()
            .iter()
            .any(|mapping| mapping.is_opencode_go())
    );
    assert!(
        old.routeable_mappings()
            .iter()
            .any(|mapping| mapping.is_zen_free())
    );
    assert!(
        !old.routeable_mappings()
            .iter()
            .any(|mapping| mapping.is_command_code_goat())
    );
    let shared = resolve_with_runtime_catalogs("new-shared", catalogs).unwrap();
    assert_eq!(shared.routeable_mappings().len(), 3);
    assert!(
        shared
            .routeable_mappings()
            .iter()
            .any(|mapping| mapping.is_cpa())
    );
    assert!(
        shared
            .routeable_mappings()
            .iter()
            .any(|mapping| mapping.is_command_code_goat() && mapping.upstream_model == goat[0])
    );
    assert!(
        shared
            .routeable_mappings()
            .iter()
            .any(|mapping| mapping.provider_id == "http-provider")
    );
}

#[test]
fn builtin_explicit_raw_shaped_names_have_publication_and_resolution_parity() {
    for public in ["org/Public", "my_model", "my model"] {
        let minimax = ["MiniMax-M3".to_string()];
        let overrides = [ExtraProviderCatalog {
            provider_id: MINIMAX_PROVIDER_ID.into(),
            mappings: vec![(public.into(), minimax[0].clone())],
        }];
        let catalogs = RuntimeCatalogs {
            minimax: &minimax,
            builtin_aliases: &overrides,
            ..RuntimeCatalogs::default()
        };
        let resolved = resolve_with_runtime_catalogs(public, catalogs).unwrap();
        assert_eq!(
            resolved.routeable_mappings(),
            vec![&minimax_mapping("MiniMax-M3")]
        );
        assert!(
            published_routeable_models_with_runtime_catalogs(catalogs)
                .iter()
                .any(|item| item.alias == public && item.owned_by == MINIMAX_PROVIDER_ID)
        );
        assert!(
            !published_routeable_aliases_with_runtime_catalogs(catalogs)
                .iter()
                .any(|item| item.alias == public)
        );
        assert!(
            routeable_models_for_with_runtime_catalogs(MINIMAX_PROVIDER_ID, catalogs)
                .contains(&public.to_string())
        );
    }
}

#[test]
fn builtin_overrides_keep_raw_collisions_ambiguous_and_unpublished() {
    let models = ["vendor/shared".to_string(), "collision".to_string()];
    let overrides = [
        ExtraProviderCatalog {
            provider_id: MINIMAX_PROVIDER_ID.into(),
            mappings: vec![
                ("first".into(), models[0].clone()),
                ("other".into(), models[1].clone()),
            ],
        },
        ExtraProviderCatalog {
            provider_id: KIMI_PROVIDER_ID.into(),
            mappings: vec![
                ("second".into(), models[0].clone()),
                ("collision".into(), "kimi-for-coding".into()),
            ],
        },
    ];
    let kimi = [models[0].clone(), "kimi-for-coding".into()];
    let catalogs = RuntimeCatalogs {
        minimax: &models,
        kimi: &kimi,
        builtin_aliases: &overrides,
        ..RuntimeCatalogs::default()
    };
    for requested in ["vendor/shared", "collision"] {
        assert_eq!(
            resolve_with_runtime_catalogs(requested, catalogs)
                .unwrap_err()
                .code(),
            Some(AMBIGUOUS_MODEL_ID)
        );
        assert!(
            !published_routeable_models_with_runtime_catalogs(catalogs)
                .iter()
                .any(|item| item.alias == requested)
        );
    }
    assert_eq!(
        resolve_with_runtime_catalogs("first", catalogs)
            .unwrap()
            .routeable_mappings(),
        vec![&minimax_mapping("vendor/shared")]
    );
    assert_eq!(
        resolve_with_runtime_catalogs("second", catalogs)
            .unwrap()
            .routeable_mappings(),
        vec![&kimi_mapping("vendor/shared")]
    );
}

#[test]
fn builtin_override_cannot_replace_an_unrelated_same_provider_raw_target() {
    for provider in [OPENCODE_PROVIDER_ID, COMMAND_CODE_PROVIDER_ID] {
        let models = ["vendor/one".to_string(), "custom-public".to_string()];
        let overrides = [ExtraProviderCatalog {
            provider_id: provider.into(),
            mappings: vec![("custom-public".into(), models[0].clone())],
        }];
        let mut catalogs = RuntimeCatalogs {
            builtin_aliases: &overrides,
            ..RuntimeCatalogs::default()
        };
        if provider == OPENCODE_PROVIDER_ID {
            catalogs.go = &models;
        } else {
            catalogs.command_code = &models;
        }
        assert_eq!(
            resolve_with_runtime_catalogs("custom-public", catalogs)
                .unwrap_err()
                .code(),
            Some(AMBIGUOUS_MODEL_ID)
        );
        assert!(
            !published_routeable_models_with_runtime_catalogs(catalogs)
                .iter()
                .any(|item| item.alias == "custom-public")
        );
    }
}

#[test]
fn builtin_overrides_require_a_saved_exact_upstream_and_static_provider() {
    let models = ["MiniMax-M3".into()];
    let overrides = [
        ExtraProviderCatalog {
            provider_id: MINIMAX_PROVIDER_ID.into(),
            mappings: vec![
                ("bad-case".into(), "minimax-m3".into()),
                ("stale".into(), "removed".into()),
            ],
        },
        ExtraProviderCatalog {
            provider_id: "http-provider".into(),
            mappings: vec![("wrong-provider".into(), "MiniMax-M3".into())],
        },
    ];
    let catalogs = RuntimeCatalogs {
        minimax: &models,
        builtin_aliases: &overrides,
        ..RuntimeCatalogs::default()
    };
    for requested in ["bad-case", "stale", "wrong-provider"] {
        assert!(matches!(
            resolve_with_runtime_catalogs(requested, catalogs),
            Err(ResolveError::Unknown { .. })
        ));
    }
    assert!(
        resolve_with_runtime_catalogs("minimax-m3", catalogs)
            .unwrap()
            .routeable_mappings()
            .iter()
            .any(|mapping| mapping.is_minimax_cn())
    );
}

#[test]
fn builtin_shared_explicit_names_keep_all_provider_mappings_and_one_publication() {
    for public in ["Shared-Name", "org/Shared"] {
        let minimax = ["MiniMax-M3".to_string()];
        let kimi = ["k3".to_string()];
        let mut overrides = [
            ExtraProviderCatalog {
                provider_id: MINIMAX_PROVIDER_ID.into(),
                mappings: vec![(public.into(), minimax[0].clone())],
            },
            ExtraProviderCatalog {
                provider_id: KIMI_PROVIDER_ID.into(),
                mappings: vec![(public.into(), kimi[0].clone())],
            },
        ];
        for _ in 0..2 {
            let catalogs = RuntimeCatalogs {
                minimax: &minimax,
                kimi: &kimi,
                builtin_aliases: &overrides,
                ..RuntimeCatalogs::default()
            };
            let resolved = resolve_with_runtime_catalogs(public, catalogs).unwrap();
            assert_eq!(resolved.routeable_mappings().len(), 2);
            assert!(
                resolved
                    .routeable_mappings()
                    .contains(&&minimax_mapping("MiniMax-M3"))
            );
            assert!(resolved.routeable_mappings().contains(&&kimi_mapping("k3")));
            assert_eq!(
                published_routeable_models_with_runtime_catalogs(catalogs)
                    .iter()
                    .filter(|item| item.alias == public)
                    .count(),
                1
            );
            overrides.reverse();
        }
    }
}

#[test]
fn builtin_explicit_case_variant_of_existing_alias_is_not_published_twice() {
    let models = ["MiniMax-M3".to_string()];
    let overrides = [ExtraProviderCatalog {
        provider_id: MINIMAX_PROVIDER_ID.into(),
        mappings: vec![("GLM-5.2".into(), models[0].clone())],
    }];
    let catalogs = RuntimeCatalogs {
        minimax: &models,
        builtin_aliases: &overrides,
        ..RuntimeCatalogs::default()
    };
    assert_eq!(
        published_routeable_models_with_runtime_catalogs(catalogs)
            .iter()
            .filter(|item| item.alias.eq_ignore_ascii_case("glm-5.2"))
            .count(),
        1
    );
    assert!(
        resolve_with_runtime_catalogs("GLM-5.2", catalogs)
            .unwrap()
            .routeable_mappings()
            .contains(&&minimax_mapping("MiniMax-M3"))
    );
}

#[test]
fn renaming_go_keeps_other_providers_on_its_previous_shared_alias() {
    let go = ["glm-5.2".to_string()];
    let zen = ["glm-5.2-free".to_string()];
    let overrides = [ExtraProviderCatalog {
        provider_id: OPENCODE_PROVIDER_ID.into(),
        mappings: vec![("private-go".into(), go[0].clone())],
    }];
    let catalogs = RuntimeCatalogs {
        go: &go,
        zen_free: &zen,
        builtin_aliases: &overrides,
        ..RuntimeCatalogs::default()
    };
    let old = resolve_with_runtime_catalogs("glm-5.2", catalogs).unwrap();
    assert_eq!(old.routeable_mappings(), vec![&zen_mapping("glm-5.2-free")]);
    assert_eq!(
        resolve_with_runtime_catalogs("private-go", catalogs)
            .unwrap()
            .routeable_mappings(),
        vec![&go_mapping("glm-5.2")]
    );
    assert!(
        published_routeable_models_with_runtime_catalogs(catalogs)
            .iter()
            .any(|item| item.alias == "glm-5.2" && item.owned_by == OPENCODE_ZEN_FREE_PROVIDER_ID)
    );
}

#[test]
fn publishing_many_go_catalog_ids_builds_the_runtime_registry_once() {
    let _ = take_runtime_registry_build_count();
    let go: Vec<String> = (0..256).map(|i| format!("unique-go-catalog-{i}")).collect();
    let extras = [
        ExtraProviderCatalog {
            provider_id: "11111111-1111-4111-8111-111111111111".into(),
            mappings: vec![("lab-public".into(), "vendor/lab".into())],
        },
        ExtraProviderCatalog {
            provider_id: "22222222-2222-4222-8222-222222222222".into(),
            mappings: vec![("lab-public".into(), "other/lab".into())],
        },
        ExtraProviderCatalog {
            provider_id: "33333333-3333-4333-8333-333333333333".into(),
            mappings: vec![("solo-public".into(), "vendor/solo".into())],
        },
        ExtraProviderCatalog {
            provider_id: "44444444-4444-4444-8444-444444444444".into(),
            mappings: vec![("shared/raw".into(), "shared/raw".into())],
        },
        ExtraProviderCatalog {
            provider_id: "55555555-5555-4555-8555-555555555555".into(),
            mappings: vec![("shared/raw".into(), "shared/raw".into())],
        },
    ];
    let catalogs = RuntimeCatalogs {
        go: &go,
        extra: &extras,
        ..RuntimeCatalogs::default()
    };
    let index = RuntimeCatalogIndex::from_catalogs(catalogs);
    assert_eq!(
        take_runtime_registry_build_count(),
        1,
        "one publication pass must not rebuild the alias table per catalog id"
    );
    let published = index.published_models();
    for id in &go {
        assert!(
            published.iter().any(|item| item.alias == *id),
            "{id} must stay a unique Go raw pin"
        );
        match index.resolve(id).expect(id) {
            ResolvedModel::PinnedRaw { mapping, .. } => {
                assert_eq!(mapping.provider_id, OPENCODE_PROVIDER_ID);
                assert_eq!(mapping.upstream_model, *id);
                assert!(mapping.routeable);
            }
            other => panic!("expected unique Go raw pin for {id}, got {other:?}"),
        }
    }
    match index.resolve("glm-5.2").expect("glm-5.2") {
        ResolvedModel::Alias { alias, .. } => assert_eq!(alias, "glm-5.2"),
        other => panic!("glm-5.2 must stay a code-owned alias, got {other:?}"),
    }
    match index.resolve("solo-public").expect("solo-public") {
        ResolvedModel::Alias { mappings, .. } => {
            assert_eq!(mappings.len(), 1);
            assert_eq!(mappings[0].provider_id, extras[2].provider_id);
            assert_eq!(mappings[0].upstream_model, "vendor/solo");
        }
        other => panic!("expected an independent public-name alias, got {other:?}"),
    }
    match index.resolve("lab-public").expect("lab-public") {
        ResolvedModel::Alias { mappings, .. } => {
            assert_eq!(mappings.len(), 2);
            assert!(
                mappings
                    .iter()
                    .any(|mapping| mapping.provider_id == extras[0].provider_id)
            );
            assert!(
                mappings
                    .iter()
                    .any(|mapping| mapping.provider_id == extras[1].provider_id)
            );
        }
        other => panic!("shared public names must aggregate, got {other:?}"),
    }
    let ambiguous = index.resolve("shared/raw").expect_err("shared/raw");
    assert_eq!(ambiguous.code(), Some(AMBIGUOUS_MODEL_ID));
    match ambiguous {
        ResolveError::Ambiguous { mappings, .. } => {
            assert!(
                mappings.len() >= 2,
                "raw public collisions must not pick a single extra, got {mappings:?}"
            );
        }
        other => panic!("expected Ambiguous, got {other:?}"),
    }
    assert!(
        !published.iter().any(|item| item.alias == "shared/raw"),
        "ambiguous raw public names stay unpublished"
    );
}

#[test]
fn new_catalog_aliases_preserve_other_models_saved_names() {
    let models = vec![
        "vendor/old-model".to_string(),
        "vendor/new-model".to_string(),
    ];
    for provider in [
        COMMAND_CODE_PROVIDER_ID,
        MINIMAX_PROVIDER_ID,
        KIMI_PROVIDER_ID,
        OLLAMA_PROVIDER_ID,
    ] {
        let saved = [ExtraProviderCatalog {
            provider_id: provider.to_string(),
            mappings: vec![("new-model".into(), models[0].clone())],
        }];
        let catalogs = RuntimeCatalogs {
            command_code: if provider == COMMAND_CODE_PROVIDER_ID {
                &models
            } else {
                &[]
            },
            minimax: if provider == MINIMAX_PROVIDER_ID {
                &models
            } else {
                &[]
            },
            kimi: if provider == KIMI_PROVIDER_ID {
                &models
            } else {
                &[]
            },
            ollama: if provider == OLLAMA_PROVIDER_ID {
                &models
            } else {
                &[]
            },
            builtin_aliases: &saved,
            ..RuntimeCatalogs::default()
        };
        let resolved = resolve_with_runtime_catalogs("new-model", catalogs).unwrap();
        let targets = resolved
            .routeable_mappings()
            .into_iter()
            .filter(|mapping| mapping.provider_id == provider)
            .collect::<Vec<_>>();
        assert_eq!(targets.len(), 1, "{provider}");
        assert_eq!(targets[0].upstream_model, models[0], "{provider}");
        assert!(
            !catalog_aliases(provider, catalogs).contains_key(&models[1]),
            "{provider}"
        );
        assert!(
            matches!(resolve_with_runtime_catalogs(&models[1], catalogs), Ok(ResolvedModel::PinnedRaw { mapping, .. }) if mapping.provider_id == provider && mapping.upstream_model == models[1])
        );
    }
}
