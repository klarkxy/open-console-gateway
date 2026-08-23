//! I/O-free Stage 1 kernels: identities, protocol catalogs, pricing types,
//! and Zen catalog parse/normalize.
//!
//! `catalog`, `ids`, `protocol`, and `zen` live in `ocg_domain` and are
//! re-exported here through explicit facade modules so
//! `ocg_core::kernel::{catalog,ids,protocol,zen}` keep compiling without
//! widening crate-private items. `pricing` stays in this crate so
//! [`pricing::PricingSnapshot`] retains inherent `estimate`/`estimate_at`
//! methods in the host pricing module.
//!
//! Domain sources must not import db, state, dashboard, gateway execution,
//! reqwest, rusqlite, tokio, axum, filesystem, clocks, or process/host code.

pub mod catalog;
pub mod ids;
pub mod pricing;
pub mod protocol;
pub mod zen;

#[cfg(test)]
mod dependency_guard {
    use std::collections::{BTreeMap, BTreeSet};
    use std::fs;
    use std::path::{Path, PathBuf};
    use syn::parse::Parser;
    use syn::punctuated::Punctuated;
    use syn::visit::Visit;
    use syn::{Attribute, Item, Meta, Token, UseTree, Visibility};
    use toml::{Table, Value};

    const ALLOWED_DOMAIN_DEPENDENCIES: &[&str] = &["chrono", "serde", "serde_json"];
    const ALLOWED_CHRONO_FEATURES: &[&str] = &["serde", "std"];
    const FORBIDDEN_EXTERNAL_CRATES: &[&str] = &["reqwest", "rusqlite", "tokio", "axum"];
    const FORBIDDEN_STD_MODULES: &[&str] = &["fs", "process"];

    const FORBIDDEN_KERNEL_CRATE_MODULES: &[&str] = &[
        "db",
        "state",
        "dashboard",
        "host_router",
        "host_gateway",
        "gateway_runtime",
        "routing_runtime",
        "gateway",
        "http_client",
        "custom",
        "custom_http",
        "auth",
        "browser",
        "console_usage",
        "go_usage",
        "usage_sync",
        "gateway_keys",
        "pricing",
    ];

    const EXPECTED_HOST_SCC: &[&str] = &[];

    #[test]
    fn kernel_modules_do_not_import_io_or_control_plane() {
        let kernel_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/kernel");
        let domain_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("ocg-domain")
            .join("src");

        let mut scanned = Vec::new();
        visit_rust_files(&kernel_root, &mut |path| {
            scanned.push(path.to_path_buf());
            assert_production_is_io_free(path);
            assert_no_blanket_domain_reexport(path);
        });
        visit_rust_files(&domain_root, &mut |path| {
            scanned.push(path.to_path_buf());
            assert_production_is_io_free(path);
        });

        assert!(
            scanned.iter().any(|path| {
                path.file_name().and_then(|name| name.to_str()) == Some("lib.rs")
                    && path.components().any(|component| {
                        component.as_os_str() == std::ffi::OsStr::new("ocg-domain")
                    })
            }),
            "domain purity guard must recursively scan lib.rs, scanned={scanned:?}"
        );
        assert!(
            kernel_root.join("pricing.rs").is_file(),
            "kernel pricing.rs must remain in ocg-core"
        );
        assert!(
            !domain_root.join("pricing.rs").exists(),
            "PricingSnapshot must not move into ocg-domain"
        );

        let kernel_pricing = read_to_string(&kernel_root.join("pricing.rs"));
        assert!(
            kernel_pricing.contains("pub struct PricingSnapshot"),
            "PricingSnapshot must stay owned by ocg-core kernel pricing"
        );
        assert!(
            !kernel_pricing.contains("ocg_domain::pricing"),
            "kernel pricing must not forward to ocg_domain::pricing"
        );

        assert_domain_manifest_clock_free();
    }

    #[test]
    fn syntax_guard_rejects_grouped_std_io_imports() {
        assert_guard_rejects("use std::{fs, process};");
    }

    #[test]
    fn syntax_guard_rejects_multiline_grouped_domain_glob_reexports() {
        let source = r#"
            pub use ocg_domain::{
                catalog::{
                    *,
                },
            };
        "#;
        assert!(
            std::panic::catch_unwind(|| {
                let path = Path::new("compat.rs");
                assert_no_blanket_domain_reexport_source(path, source);
            })
            .is_err(),
            "nested grouped glob reexports must not bypass the compatibility facade guard"
        );
    }

    #[test]
    fn syntax_guard_rejects_whole_domain_crate_reexports() {
        for source in [
            "pub use ocg_domain as domain;",
            "pub use ocg_domain::{self};",
        ] {
            assert!(
                std::panic::catch_unwind(|| {
                    assert_no_blanket_domain_reexport_source(Path::new("compat.rs"), source);
                })
                .is_err(),
                "whole-crate reexports must not bypass the compatibility facade guard: {source}"
            );
        }
    }

    #[test]
    fn syntax_guard_skips_cfg_test_associated_items_and_statements() {
        let test_only = r#"
            struct Fixture;

            impl Fixture {
                #[cfg(test)]
                fn helper() {
                    std::fs::read("fixture");
                }

                fn production() {
                    #[cfg(test)]
                    let _: std::fs::File = unreachable!();

                    #[cfg(test)]
                    std::fs::read("fixture");
                }
            }

            trait FixtureTrait {
                #[cfg(test)]
                fn helper() {
                    std::fs::read("fixture");
                }
            }

            unsafe extern "C" {
                #[cfg(test)]
                static TEST_FILE: std::fs::File;
            }
        "#;
        assert_source_is_io_free(Path::new("fixture.rs"), test_only);

        for production in [
            r#"
                struct Fixture;
                impl Fixture {
                    fn helper() {
                        std::fs::read("fixture");
                    }
                }
            "#,
            r#"
                fn production() {
                    let _: std::fs::File = unreachable!();
                }
            "#,
            r#"
                fn production() {
                    std::fs::read("fixture");
                }
            "#,
            r#"
                trait FixtureTrait {
                    fn helper() {
                        std::fs::read("fixture");
                    }
                }
            "#,
            r#"
                unsafe extern "C" {
                    static TEST_FILE: std::fs::File;
                }
            "#,
        ] {
            assert_guard_rejects(production);
        }
    }

    #[test]
    fn manifest_guard_rejects_target_specific_dependencies() {
        let manifest = format!(
            "{}\n[target.'cfg(windows)'.dependencies]\nreqwest = \"0.12\"\n",
            valid_domain_manifest()
        );
        assert_manifest_rejects(&manifest);
    }

    #[test]
    fn manifest_guard_rejects_feature_based_chrono_clock_activation() {
        let manifest = format!(
            "{}\n[features]\nclock = [\"chrono/clock\"]\n",
            valid_domain_manifest()
        );
        assert_manifest_rejects(&manifest);
    }

    #[test]
    fn alias_does_not_import_custom_module() {
        let src_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
        let alias = read_to_string(&src_root.join("alias.rs"));
        assert!(
            !alias_imports_custom(&alias),
            "alias.rs must not import or reach crate::custom; use kernel::ids for the shared matcher"
        );
        assert!(
            alias.contains("kernel::ids") && alias.contains("custom_model_id_matches"),
            "alias.rs must use the shared matcher from kernel::ids"
        );
    }

    #[test]
    fn alias_custom_import_guard_catches_supported_forms() {
        for source in [
            "use crate::custom::custom_model_id_matches;",
            "use crate::{kernel::ids::looks_raw_shaped, custom::custom_model_id_matches};",
            "use super::custom::custom_model_id_matches;",
            "use super::{kernel::ids::looks_raw_shaped, custom::custom_model_id_matches};",
        ] {
            assert!(
                alias_imports_custom(source),
                "alias->custom guard must catch supported import form: {source}"
            );
        }
        assert!(
            !alias_imports_custom("use crate::kernel::ids::custom_model_id_matches;"),
            "the narrow guard must allow the kernel matcher import"
        );
    }

    #[test]
    fn contract_and_v3_account_sources_do_not_import_gateway_utilities() {
        let src_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
        for relative in ["provider_contracts.rs", "dashboard_v3/accounts.rs"] {
            let path = src_root.join(relative);
            let production = production_source(&read_to_string(&path));
            assert!(
                !crate_path_roots(&production).contains("gateway"),
                "{relative} production source must not contain crate::gateway"
            );
            for line in production.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("use ") {
                    assert!(
                        !trimmed.starts_with("use crate::gateway"),
                        "{relative} imports gateway: {trimmed}"
                    );
                }
            }
        }
    }

    #[test]
    fn redaction_module_is_a_pure_dag_leaf() {
        let src_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
        let path = src_root.join("redaction.rs");
        let production = production_source(&read_to_string(&path));
        assert_production_is_io_free(&path);
        for module in crate_path_roots(&production) {
            assert!(
                !FORBIDDEN_KERNEL_CRATE_MODULES.contains(&module.as_str()),
                "redaction.rs has a qualified production path into `{module}`"
            );
            assert_ne!(module, "gateway", "redaction.rs must not depend on gateway");
        }
        for needle in ["Utc::now", "Instant::now", "SystemTime::now"] {
            assert!(
                !production.contains(needle),
                "redaction.rs must not read a clock (`{needle}`)"
            );
        }
        assert!(
            crate_path_roots(&production).is_empty(),
            "redaction.rs must remain a crate-level DAG leaf, got {:?}",
            crate_path_roots(&production)
        );
    }

    #[test]
    fn upstream_limit_is_a_pure_dag_leaf_consumed_by_dashboard_without_gateway() {
        let src_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
        let path = src_root.join("upstream_limit.rs");
        let production = production_source(&read_to_string(&path));
        assert_production_is_io_free(&path);
        let roots = crate_path_roots(&production);
        for module in &roots {
            assert!(
                !FORBIDDEN_KERNEL_CRATE_MODULES.contains(&module.as_str()),
                "upstream_limit.rs has a qualified production path into `{module}`"
            );
            assert_ne!(
                module.as_str(),
                "gateway",
                "upstream_limit.rs must not depend on gateway"
            );
            assert_ne!(
                module.as_str(),
                "dashboard",
                "upstream_limit.rs must not depend on dashboard"
            );
            assert_ne!(
                module.as_str(),
                "dashboard_v3",
                "upstream_limit.rs must not depend on dashboard_v3"
            );
            assert_ne!(
                module.as_str(),
                "state",
                "upstream_limit.rs must not depend on state"
            );
            assert_ne!(
                module.as_str(),
                "db",
                "upstream_limit.rs must not depend on db"
            );
            assert_ne!(
                module.as_str(),
                "provider",
                "upstream_limit.rs must not depend on provider adapters"
            );
        }
        assert_eq!(
            roots,
            named_set(&["models"]),
            "upstream_limit.rs may depend only on models for UsageWindowKind, got {roots:?}"
        );
        for needle in ["Utc::now", "Instant::now", "SystemTime::now"] {
            assert!(
                !production.contains(needle),
                "upstream_limit.rs must not read a clock (`{needle}`)"
            );
        }

        let dashboard = production_source(&read_to_string(&src_root.join("dashboard.rs")));
        assert!(
            crate_path_roots(&dashboard).contains("upstream_limit"),
            "dashboard production source must consume crate::upstream_limit directly"
        );
        assert!(
            !dashboard.contains("gateway::limit"),
            "dashboard must not reach limit parsers through crate::gateway"
        );
        for line in dashboard.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("use ") {
                assert!(
                    !trimmed.contains("gateway::limit")
                        && !trimmed.starts_with("use crate::gateway::limit"),
                    "dashboard imports gateway limit parsers: {trimmed}"
                );
            }
        }
    }

    #[test]
    fn production_graph_has_the_expected_remaining_scc() {
        let src_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
        let lib_source = production_source(&read_to_string(&src_root.join("lib.rs")));
        let modules = declared_modules(&lib_source);
        assert!(
            modules.contains("db")
                && modules.contains("pricing")
                && modules.contains("kernel")
                && modules.contains("redaction")
                && modules.contains("upstream_limit")
                && modules.contains("host_router")
                && modules.contains("host_gateway")
                && modules.contains("gateway_runtime")
                && modules.contains("routing_runtime"),
            "lib.rs should declare the production modules under test, got {modules:?}"
        );

        let db_source = production_source(&read_to_string(&src_root.join("db.rs")));
        assert!(
            !crate_path_roots(&db_source).contains("pricing"),
            "db production source must not reference the clocked pricing module"
        );
        assert!(
            !crate_path_roots(&db_source).contains("gateway_keys"),
            "db production source must not reference gateway_keys"
        );
        assert!(
            db_source.contains("CURRENT_SCHEMA_VERSION: i32 = 26"),
            "schema version must remain 26"
        );

        let graph = production_graph(&src_root, &modules);
        assert!(
            EXPECTED_HOST_SCC.is_empty(),
            "Phase 1 cut must not whitelist a multi-node host SCC"
        );
        assert!(
            !graph
                .get("provider")
                .is_some_and(|edges| edges.contains("go_usage")),
            "provider catalog facts must not depend on the Go usage HTTP client"
        );
        let db_component = tarjan(&graph)
            .into_iter()
            .find(|component| component.contains("db"))
            .expect("db module should exist in the production graph");
        assert_eq!(
            db_component.len(),
            1,
            "db must not remain in a production SCC after the contract/redaction inversion, db_component={db_component:?}"
        );
        assert!(
            !graph
                .get("db")
                .is_some_and(|edges| edges.contains("pricing")),
            "db must not depend on pricing"
        );
        assert!(
            !graph
                .get("db")
                .is_some_and(|edges| edges.contains("gateway_keys")),
            "db must not depend on gateway_keys"
        );
        assert!(
            !graph
                .get("gateway_keys")
                .is_some_and(|edges| edges.contains("state") || edges.contains("db")),
            "gateway_keys must not depend on state or db"
        );
        assert!(
            !graph
                .get("usage_sync")
                .is_some_and(|edges| edges.contains("state") || edges.contains("db")),
            "usage_sync must not depend on state or db"
        );
        assert!(
            !graph
                .get("provider_contracts")
                .is_some_and(|edges| edges.contains("gateway")),
            "provider_contracts must not depend on gateway"
        );
        assert!(
            !graph
                .get("dashboard_v3")
                .is_some_and(|edges| edges.contains("gateway") || edges.contains("dashboard")),
            "dashboard_v3 must not depend on gateway or dashboard"
        );
        assert!(
            !graph.get("protocol_probe").is_some_and(|edges| {
                edges.contains("dashboard") || edges.contains("dashboard_v3")
            }),
            "protocol_probe must not depend on dashboard or dashboard_v3"
        );
        assert!(
            graph
                .get("protocol_probe")
                .is_some_and(|edges| { edges.contains("gateway") && edges.contains("state") }),
            "protocol_probe still depends on gateway/state for probe transport"
        );
        assert!(
            !graph.get("gateway").is_some_and(|edges| {
                edges.contains("dashboard")
                    || edges.contains("dashboard_v3")
                    || edges.contains("host_router")
                    || edges.contains("host_gateway")
                    || edges.contains("protocol_probe")
            }),
            "gateway must not import dashboard mounts, host composition, or protocol_probe, graph={graph:?}"
        );
        assert!(
            !graph.get("state").is_some_and(|edges| {
                edges.contains("dashboard")
                    || edges.contains("dashboard_v3")
                    || edges.contains("host_router")
                    || edges.contains("protocol_probe")
            }),
            "state must not import dashboard mounts, host_router, or protocol_probe, graph={graph:?}"
        );
        assert!(
            !graph.get("state").is_some_and(|edges| {
                edges.contains("gateway") || edges.contains("host_gateway")
            }),
            "state must not depend on gateway or the host rebind adapter, graph={graph:?}"
        );
        assert!(
            graph
                .get("gateway")
                .is_some_and(|edges| edges.contains("state")),
            "gateway -> state may remain one-way after the Phase 1 cut, graph={graph:?}"
        );
        assert!(
            graph.get("state").is_some_and(|edges| {
                edges.contains("gateway_runtime") && edges.contains("routing_runtime")
            }),
            "state must own the extracted runtime slots without a gateway edge, graph={graph:?}"
        );
        assert!(
            graph
                .get("host_gateway")
                .is_some_and(|edges| { edges.contains("gateway") && edges.contains("state") }),
            "host_gateway must adapt gateway lifecycle onto state, graph={graph:?}"
        );
        assert!(
            !graph.get("host_gateway").is_some_and(|edges| {
                edges.contains("dashboard")
                    || edges.contains("dashboard_v3")
                    || edges.contains("protocol_probe")
                    || edges.contains("usage_sync")
                    || edges.contains("db")
                    || edges.contains("http_client")
            }),
            "host_gateway must stay a rebind adapter, graph={graph:?}"
        );
        assert!(
            !graph.get("gateway_runtime").is_some_and(|edges| {
                edges.contains("gateway")
                    || edges.contains("state")
                    || edges.contains("host_gateway")
                    || edges.contains("host_router")
            }),
            "gateway_runtime must stay outside state/gateway, graph={graph:?}"
        );
        assert!(
            !graph.get("routing_runtime").is_some_and(|edges| {
                edges.contains("gateway")
                    || edges.contains("state")
                    || edges.contains("host_gateway")
                    || edges.contains("host_router")
            }),
            "routing_runtime must stay outside state/gateway, graph={graph:?}"
        );
        assert!(
            graph.get("host_router").is_some_and(|edges| {
                edges.contains("dashboard")
                    && edges.contains("dashboard_v3")
                    && edges.contains("gateway")
            }),
            "host_router must compose dashboard mounts onto gateway, graph={graph:?}"
        );
        assert!(
            !graph.get("host_router").is_some_and(|edges| {
                edges.contains("protocol_probe")
                    || edges.contains("usage_sync")
                    || edges.contains("db")
                    || edges.contains("http_client")
            }),
            "host_router must stay composition-only, graph={graph:?}"
        );
        let protocol_probe_source =
            production_source(&read_to_string(&src_root.join("protocol_probe.rs")));
        for needle in [
            "forward_once",
            "client_for",
            "gateway::executor",
            "gateway::forwarder",
        ] {
            assert!(
                !protocol_probe_source.contains(needle),
                "protocol_probe must not call {needle}"
            );
        }
        assert_eq!(
            graph.get("redaction").cloned().unwrap_or_default(),
            BTreeSet::new(),
            "redaction must be a production DAG leaf, graph={graph:?}"
        );
        assert_eq!(
            graph.get("upstream_limit").cloned().unwrap_or_default(),
            named_set(&["models"]),
            "upstream_limit must stay an I/O-free DAG leaf beside models, graph={graph:?}"
        );
        assert!(
            graph
                .get("dashboard")
                .is_some_and(|edges| edges.contains("upstream_limit")),
            "dashboard must consume upstream_limit without a gateway parser edge"
        );
        assert!(
            graph
                .get("gateway")
                .is_some_and(|edges| edges.contains("upstream_limit")),
            "gateway must still depend on the extracted upstream_limit leaf"
        );
        assert!(
            !graph.get("upstream_limit").is_some_and(|edges| {
                edges.contains("gateway")
                    || edges.contains("dashboard")
                    || edges.contains("dashboard_v3")
                    || edges.contains("state")
                    || edges.contains("db")
                    || edges.contains("provider")
            }),
            "upstream_limit must not grow a control-plane or adapter edge, graph={graph:?}"
        );

        let gateway_keys_source =
            production_source(&read_to_string(&src_root.join("gateway_keys.rs")));
        let usage_sync_source = production_source(&read_to_string(&src_root.join("usage_sync.rs")));
        let account_control_source =
            production_source(&read_to_string(&src_root.join("account_control.rs")));
        for (name, source) in [
            ("gateway_keys.rs", &gateway_keys_source),
            ("usage_sync.rs", &usage_sync_source),
            ("account_control.rs", &account_control_source),
        ] {
            assert!(
                !source.contains("crate::state"),
                "{name} production source must not import crate::state"
            );
            assert!(
                !source.contains("CoreState"),
                "{name} production source must not name CoreState"
            );
        }
        for forbidden in ["state", "dashboard", "dashboard_v3", "gateway"] {
            assert!(
                !crate_path_roots(&account_control_source).contains(forbidden),
                "account_control production source must not import {forbidden}"
            );
        }
        assert!(
            !graph.get("account_control").is_some_and(|edges| {
                edges.contains("state")
                    || edges.contains("dashboard")
                    || edges.contains("dashboard_v3")
                    || edges.contains("gateway")
            }),
            "account_control must not depend on host SCC modules, graph={graph:?}"
        );
        let account_control_component = tarjan(&graph)
            .into_iter()
            .find(|component| component.contains("account_control"))
            .expect("account_control module should exist in the production graph");
        assert_eq!(
            account_control_component.len(),
            1,
            "account_control must not join a production SCC, account_control_component={account_control_component:?}"
        );

        // The Phase 1 cut moved GatewayHandle and RoutingRuntime out of both
        // `gateway` and `state`. Settings rebind goes through GatewayRebindHost,
        // implemented only in host_gateway. gateway -> state remains one-way.
        // HTTP router composition stays in host_router. Tarjan runs against the
        // complete measured production graph; any multi-node SCC is a failure.
        let mut nontrivial: Vec<BTreeSet<String>> = tarjan(&graph)
            .into_iter()
            .filter(|component| component.len() > 1)
            .collect();
        nontrivial.sort();
        assert!(
            nontrivial.is_empty(),
            "production graph must have no multi-node SCC, sccs={nontrivial:?}, graph={graph:?}"
        );
        for name in [
            "gateway",
            "state",
            "dashboard",
            "dashboard_v3",
            "protocol_probe",
            "host_router",
            "host_gateway",
            "gateway_runtime",
            "routing_runtime",
        ] {
            let component = tarjan(&graph)
                .into_iter()
                .find(|component| component.contains(name))
                .unwrap_or_else(|| panic!("{name} module should exist in the production graph"));
            assert_eq!(
                component.len(),
                1,
                "{name} must not remain in a multi-node production SCC, component={component:?}, graph={graph:?}"
            );
        }
    }

    #[test]
    fn host_router_is_composition_only() {
        let src_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
        let host_router_raw = read_to_string(&src_root.join("host_router.rs"));
        let host_router = production_source(&host_router_raw);
        let gateway_mod = production_source(&read_to_string(&src_root.join("gateway/mod.rs")));
        let listener = production_source(&read_to_string(&src_root.join("gateway/listener.rs")));

        assert!(
            host_router_raw.contains("\"/dashboard/api/v3\"")
                && host_router_raw.contains("\"/dashboard/api\"")
                && host_router_raw.contains("\"/dashboard\"")
                && host_router_raw.contains("\"/dashboard/\"")
                && host_router_raw.contains("\"/dashboard/assets/{*path}\"")
                && host_router.contains("dashboard_v3::api_router")
                && host_router.contains("dashboard::api_router")
                && host_router.contains("dashboard::serve_index")
                && host_router.contains("dashboard::serve_asset")
                && host_router.contains("inference_router")
                && host_router.contains("fn build_router")
                && host_router.contains("impl GatewayRouterHost for CoreState")
                && host_router.contains("fn compose_router"),
            "host_router must own the dashboard mounts and inference merge"
        );
        assert!(
            !host_router.contains("impl GatewayLifecycle"),
            "host_router must not add inherent GatewayLifecycle methods"
        );
        for needle in [
            "forward_once",
            "GatewayExecutor",
            "axum::serve",
            "TcpListener",
            "start_gateway",
            "ControlPlaneWorkers",
            "protocol_probe",
        ] {
            assert!(
                !host_router.contains(needle),
                "host_router must not take on runtime work ({needle})"
            );
        }

        let host_roots = crate_path_roots(&host_router);
        for required in ["dashboard", "dashboard_v3", "gateway"] {
            assert!(
                host_roots.contains(required),
                "host_router must depend on {required}, roots={host_roots:?}"
            );
        }
        for forbidden in [
            "protocol_probe",
            "usage_sync",
            "db",
            "http_client",
            "provider_contracts",
        ] {
            assert!(
                !host_roots.contains(forbidden),
                "host_router must not depend on {forbidden}, roots={host_roots:?}"
            );
        }

        assert!(
            gateway_mod.contains("fn inference_router")
                && !gateway_mod.contains("crate::dashboard")
                && !crate_path_roots(&gateway_mod).contains("dashboard")
                && !crate_path_roots(&gateway_mod).contains("dashboard_v3")
                && !crate_path_roots(&gateway_mod).contains("host_router"),
            "gateway inference assembly must not mount dashboards"
        );
        assert!(
            listener.contains("GatewayRouterHost")
                && listener.contains("GatewayRouterHost>::compose_router")
                && !crate_path_roots(&listener).contains("dashboard")
                && !crate_path_roots(&listener).contains("dashboard_v3")
                && !crate_path_roots(&listener).contains("host_router"),
            "listener must consume composed routes through GatewayRouterHost without importing dashboard mounts"
        );

        let mut gateway_source = String::new();
        visit_rust_files(&src_root.join("gateway"), &mut |path| {
            gateway_source.push_str(&production_source(&read_to_string(path)));
            gateway_source.push('\n');
        });
        let gateway_roots = crate_path_roots(&gateway_source);
        for forbidden in [
            "dashboard",
            "dashboard_v3",
            "host_router",
            "host_gateway",
            "protocol_probe",
        ] {
            assert!(
                !gateway_roots.contains(forbidden),
                "gateway production sources must not import {forbidden}, roots={gateway_roots:?}"
            );
        }
    }

    #[test]
    fn host_gateway_is_rebind_adapter() {
        let src_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
        let host_gateway_raw = read_to_string(&src_root.join("host_gateway.rs"));
        let host_gateway = production_source(&host_gateway_raw);
        let state = production_source(&read_to_string(&src_root.join("state.rs")));
        let runtime = production_source(&read_to_string(&src_root.join("gateway_runtime.rs")));

        assert!(
            host_gateway.contains("impl GatewayRebindHost for CoreState")
                && host_gateway.contains("GatewayLifecycle::rebind")
                && host_gateway.contains("rebind_from_serving_request"),
            "host_gateway must implement the rebind port against GatewayLifecycle"
        );
        assert!(
            !host_gateway.contains("impl GatewayLifecycle"),
            "host_gateway must not add inherent GatewayLifecycle methods"
        );
        for needle in [
            "forward_once",
            "GatewayExecutor",
            "axum::serve",
            "TcpListener",
            "start_gateway",
            "ControlPlaneWorkers",
            "protocol_probe",
            "inference_router",
            "dashboard::",
        ] {
            assert!(
                !host_gateway.contains(needle),
                "host_gateway must not take on runtime or dashboard work ({needle})"
            );
        }

        let host_roots = crate_path_roots(&host_gateway);
        for required in ["gateway", "state"] {
            assert!(
                host_roots.contains(required),
                "host_gateway must depend on {required}, roots={host_roots:?}"
            );
        }
        for forbidden in [
            "dashboard",
            "dashboard_v3",
            "protocol_probe",
            "usage_sync",
            "db",
            "http_client",
            "provider_contracts",
        ] {
            assert!(
                !host_roots.contains(forbidden),
                "host_gateway must not depend on {forbidden}, roots={host_roots:?}"
            );
        }

        assert!(
            state.contains("GatewayRebindHost::rebind")
                && state.contains("rebind_from_serving_request")
                && state.contains("rebind_gateway_listener_if_port_changed"),
            "state must rebind through GatewayRebindHost"
        );
        assert!(
            !crate_path_roots(&state).contains("gateway"),
            "state production source must not have a crate::gateway edge"
        );
        assert!(
            !crate_path_roots(&state).contains("host_gateway"),
            "state must not import the host rebind adapter"
        );
        assert!(
            runtime.contains("pub trait GatewayRebindHost")
                && runtime.contains("pub struct GatewayHandle")
                && crate_path_roots(&runtime).is_empty(),
            "gateway_runtime must stay a crate-level DAG leaf, roots={:?}",
            crate_path_roots(&runtime)
        );
    }

    fn named_set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| (*name).to_string()).collect()
    }

    fn assert_production_is_io_free(path: &Path) {
        assert_source_is_io_free(path, &read_to_string(path));
    }

    fn assert_no_blanket_domain_reexport(path: &Path) {
        assert_no_blanket_domain_reexport_source(path, &read_to_string(path));
    }

    fn assert_no_blanket_domain_reexport_source(path: &Path, source: &str) {
        let parsed = parse_rust_file(path, source);
        let mut visitor = DomainReexportVisitor::default();
        visitor.visit_file(&parsed);
        assert!(
            visitor.blanket_reexports.is_empty(),
            "{} must not blanket-reexport ocg_domain: {:?}",
            path.display(),
            visitor.blanket_reexports
        );
    }

    fn assert_guard_rejects(source: &str) {
        assert!(
            std::panic::catch_unwind(|| {
                assert_source_is_io_free(Path::new("fixture.rs"), source);
            })
            .is_err(),
            "syntax guard must reject {source:?}"
        );
    }

    fn assert_manifest_rejects(manifest: &str) {
        assert!(
            std::panic::catch_unwind(|| {
                assert_domain_manifest_is_pure(Path::new("ocg-domain/Cargo.toml"), manifest);
            })
            .is_err(),
            "manifest guard must reject adversarial manifest: {manifest}"
        );
    }

    fn valid_domain_manifest() -> &'static str {
        r#"
            [package]
            name = "ocg-domain"
            version = "0.1.0"

            [dependencies]
            chrono = { version = "0.4", default-features = false, features = ["serde", "std"] }
            serde = { version = "1", features = ["derive"] }
            serde_json = "1"
        "#
    }

    fn assert_domain_manifest_clock_free() {
        let manifest_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("ocg-domain")
            .join("Cargo.toml");
        assert_domain_manifest_is_pure(&manifest_path, &read_to_string(&manifest_path));
    }

    fn assert_domain_manifest_is_pure(manifest_path: &Path, manifest: &str) {
        let manifest: Value = toml::from_str(manifest)
            .unwrap_or_else(|error| panic!("parse {}: {error}", manifest_path.display()));
        let root = manifest
            .as_table()
            .unwrap_or_else(|| panic!("{} must be a TOML table", manifest_path.display()));
        let dependencies = root.get("dependencies").and_then(Value::as_table);
        assert!(
            dependencies.is_some_and(|table| !table.is_empty()),
            "{} is missing [dependencies]",
            manifest_path.display()
        );
        let dependency_tables = domain_dependency_tables(root, manifest_path);
        let mut names = BTreeSet::new();
        for (table_name, table) in &dependency_tables {
            for (name, spec) in *table {
                assert!(
                    ALLOWED_DOMAIN_DEPENDENCIES.contains(&name.as_str()),
                    "{} declares unexpected dependency `{name}` in [{table_name}]",
                    manifest_path.display()
                );
                names.insert(name.as_str());
                if name == "chrono" {
                    assert_chrono_dependency_is_clock_free(manifest_path, table_name, spec);
                }
            }
        }
        for required in ["chrono", "serde", "serde_json"] {
            assert!(
                names.contains(required),
                "{} must declare `{required}`",
                manifest_path.display()
            );
        }
        if let Some(features) = root.get("features") {
            assert_features_do_not_activate_chrono(manifest_path, features);
        }
    }

    fn domain_dependency_tables<'a>(
        root: &'a Table,
        manifest_path: &Path,
    ) -> Vec<(String, &'a Table)> {
        let mut tables = Vec::new();
        for key in ["dependencies", "build-dependencies", "dev-dependencies"] {
            if let Some(value) = root.get(key) {
                let table = value.as_table().unwrap_or_else(|| {
                    panic!("{} [{key}] must be a table", manifest_path.display())
                });
                tables.push((key.to_string(), table));
            }
        }
        if let Some(targets) = root.get("target") {
            let targets = targets
                .as_table()
                .unwrap_or_else(|| panic!("{} [target] must be a table", manifest_path.display()));
            for (target, value) in targets {
                let target_table = value.as_table().unwrap_or_else(|| {
                    panic!(
                        "{} [target.{target:?}] must be a table",
                        manifest_path.display()
                    )
                });
                for key in ["dependencies", "build-dependencies", "dev-dependencies"] {
                    if let Some(value) = target_table.get(key) {
                        let table = value.as_table().unwrap_or_else(|| {
                            panic!(
                                "{} [target.{target:?}.{key}] must be a table",
                                manifest_path.display()
                            )
                        });
                        tables.push((format!("target.{target:?}.{key}"), table));
                    }
                }
            }
        }
        tables
    }

    fn assert_chrono_dependency_is_clock_free(
        manifest_path: &Path,
        table_name: &str,
        spec: &Value,
    ) {
        let spec = spec.as_table().unwrap_or_else(|| {
            panic!(
                "{} [{table_name}] chrono must use an inline table with explicit clock-free settings",
                manifest_path.display()
            )
        });
        assert_eq!(
            spec.get("default-features").and_then(Value::as_bool),
            Some(false),
            "{} [{table_name}] chrono must set default-features = false",
            manifest_path.display()
        );
        let mut features = spec
            .get("features")
            .and_then(Value::as_array)
            .unwrap_or_else(|| {
                panic!(
                    "{} [{table_name}] chrono must declare features = [\"serde\", \"std\"]",
                    manifest_path.display()
                )
            })
            .iter()
            .map(|value| {
                value.as_str().unwrap_or_else(|| {
                    panic!(
                        "{} [{table_name}] chrono features must be strings",
                        manifest_path.display()
                    )
                })
            })
            .collect::<Vec<_>>();
        features.sort_unstable();
        let mut allowed = ALLOWED_CHRONO_FEATURES.to_vec();
        allowed.sort_unstable();
        assert_eq!(
            features,
            allowed,
            "{} [{table_name}] chrono features must be {ALLOWED_CHRONO_FEATURES:?}",
            manifest_path.display()
        );
    }

    fn assert_features_do_not_activate_chrono(manifest_path: &Path, features: &Value) {
        let features = features
            .as_table()
            .unwrap_or_else(|| panic!("{} [features] must be a table", manifest_path.display()));
        for (feature, members) in features {
            let members = members.as_array().unwrap_or_else(|| {
                panic!(
                    "{} [features] `{feature}` must be an array",
                    manifest_path.display()
                )
            });
            for member in members {
                let member = member.as_str().unwrap_or_else(|| {
                    panic!(
                        "{} [features] `{feature}` members must be strings",
                        manifest_path.display()
                    )
                });
                let dependency = member.strip_prefix("dep:").unwrap_or(member);
                let package = dependency
                    .split('/')
                    .next()
                    .unwrap_or_default()
                    .trim_end_matches('?');
                assert!(
                    package != "chrono",
                    "{} [features] `{feature}` must not activate chrono defaults or clock features ({member:?})",
                    manifest_path.display()
                );
            }
        }
    }

    fn assert_source_is_io_free(path: &Path, source: &str) {
        let parsed = parse_rust_file(path, source);
        let mut visitor = PurityVisitor::default();
        visitor.visit_file(&parsed);
        assert!(
            visitor.forbidden_imports.is_empty(),
            "{} imports I/O or control-plane code: {:?}",
            path.display(),
            visitor.forbidden_imports
        );
        assert!(
            visitor.forbidden_paths.is_empty(),
            "{} has a qualified I/O or control-plane path: {:?}",
            path.display(),
            visitor.forbidden_paths
        );
        assert!(
            visitor.clock_paths.is_empty(),
            "{} must not read a clock: {:?}",
            path.display(),
            visitor.clock_paths
        );
    }

    fn parse_rust_file(path: &Path, source: &str) -> syn::File {
        syn::parse_file(source)
            .unwrap_or_else(|error| panic!("parse {} as Rust: {error}", path.display()))
    }

    #[derive(Default)]
    struct PurityVisitor {
        forbidden_imports: Vec<ImportPath>,
        forbidden_paths: Vec<String>,
        clock_paths: Vec<String>,
    }

    macro_rules! visit_cfg_attributed_node {
        ($method:ident, $node:ty) => {
            fn $method(&mut self, node: &'ast $node) {
                if attributes_exclude_production(&node.attrs) {
                    return;
                }
                syn::visit::$method(self, node);
            }
        };
    }

    impl<'ast> Visit<'ast> for PurityVisitor {
        fn visit_file(&mut self, file: &'ast syn::File) {
            if attributes_exclude_production(&file.attrs) {
                return;
            }
            syn::visit::visit_file(self, file);
        }

        fn visit_item(&mut self, item: &'ast Item) {
            if item_is_test_only(item) {
                return;
            }
            syn::visit::visit_item(self, item);
        }

        fn visit_foreign_item(&mut self, item: &'ast syn::ForeignItem) {
            if attributes_exclude_production(foreign_item_attributes(item)) {
                return;
            }
            syn::visit::visit_foreign_item(self, item);
        }

        fn visit_impl_item(&mut self, item: &'ast syn::ImplItem) {
            if attributes_exclude_production(impl_item_attributes(item)) {
                return;
            }
            syn::visit::visit_impl_item(self, item);
        }

        fn visit_trait_item(&mut self, item: &'ast syn::TraitItem) {
            if attributes_exclude_production(trait_item_attributes(item)) {
                return;
            }
            syn::visit::visit_trait_item(self, item);
        }

        fn visit_expr(&mut self, expr: &'ast syn::Expr) {
            if attributes_exclude_production(expr_attributes(expr)) {
                return;
            }
            syn::visit::visit_expr(self, expr);
        }

        fn visit_pat(&mut self, pat: &'ast syn::Pat) {
            if attributes_exclude_production(pat_attributes(pat)) {
                return;
            }
            syn::visit::visit_pat(self, pat);
        }

        fn visit_generic_param(&mut self, param: &'ast syn::GenericParam) {
            if attributes_exclude_production(generic_param_attributes(param)) {
                return;
            }
            syn::visit::visit_generic_param(self, param);
        }

        visit_cfg_attributed_node!(visit_arm, syn::Arm);
        visit_cfg_attributed_node!(visit_bare_fn_arg, syn::BareFnArg);
        visit_cfg_attributed_node!(visit_bare_variadic, syn::BareVariadic);
        visit_cfg_attributed_node!(visit_field, syn::Field);
        visit_cfg_attributed_node!(visit_field_pat, syn::FieldPat);
        visit_cfg_attributed_node!(visit_field_value, syn::FieldValue);
        visit_cfg_attributed_node!(visit_local, syn::Local);
        visit_cfg_attributed_node!(visit_receiver, syn::Receiver);
        visit_cfg_attributed_node!(visit_stmt_macro, syn::StmtMacro);
        visit_cfg_attributed_node!(visit_variadic, syn::Variadic);
        visit_cfg_attributed_node!(visit_variant, syn::Variant);

        fn visit_item_use(&mut self, item_use: &'ast syn::ItemUse) {
            for import in flatten_use_tree(&item_use.tree) {
                if import_is_forbidden(&import) {
                    self.forbidden_imports.push(import);
                }
            }
            syn::visit::visit_item_use(self, item_use);
        }

        fn visit_path(&mut self, path: &'ast syn::Path) {
            let segments = path
                .segments
                .iter()
                .map(|segment| segment.ident.to_string())
                .collect::<Vec<_>>();
            if path_is_forbidden(&segments) {
                self.forbidden_paths.push(segments.join("::"));
            }
            if path_is_clock_read(&segments) {
                self.clock_paths.push(segments.join("::"));
            }
            syn::visit::visit_path(self, path);
        }
    }

    #[derive(Default)]
    struct DomainReexportVisitor {
        blanket_reexports: Vec<ImportPath>,
    }

    impl<'ast> Visit<'ast> for DomainReexportVisitor {
        fn visit_item(&mut self, item: &'ast Item) {
            if item_is_test_only(item) {
                return;
            }
            syn::visit::visit_item(self, item);
        }

        fn visit_item_use(&mut self, item_use: &'ast syn::ItemUse) {
            if matches!(item_use.vis, Visibility::Public(_)) {
                self.blanket_reexports.extend(
                    flatten_use_tree(&item_use.tree)
                        .into_iter()
                        .filter(is_blanket_domain_reexport),
                );
            }
            syn::visit::visit_item_use(self, item_use);
        }
    }

    fn item_is_test_only(item: &Item) -> bool {
        attributes_exclude_production(item_attributes(item))
    }

    fn attributes_exclude_production(attributes: &[Attribute]) -> bool {
        attributes.iter().any(attribute_excludes_production)
    }

    fn item_attributes(item: &Item) -> &[Attribute] {
        match item {
            Item::Const(item) => &item.attrs,
            Item::Enum(item) => &item.attrs,
            Item::ExternCrate(item) => &item.attrs,
            Item::Fn(item) => &item.attrs,
            Item::ForeignMod(item) => &item.attrs,
            Item::Impl(item) => &item.attrs,
            Item::Macro(item) => &item.attrs,
            Item::Mod(item) => &item.attrs,
            Item::Static(item) => &item.attrs,
            Item::Struct(item) => &item.attrs,
            Item::Trait(item) => &item.attrs,
            Item::TraitAlias(item) => &item.attrs,
            Item::Type(item) => &item.attrs,
            Item::Union(item) => &item.attrs,
            Item::Use(item) => &item.attrs,
            Item::Verbatim(_) => &[],
            _ => &[],
        }
    }

    fn foreign_item_attributes(item: &syn::ForeignItem) -> &[Attribute] {
        match item {
            syn::ForeignItem::Fn(item) => &item.attrs,
            syn::ForeignItem::Static(item) => &item.attrs,
            syn::ForeignItem::Type(item) => &item.attrs,
            syn::ForeignItem::Macro(item) => &item.attrs,
            _ => &[],
        }
    }

    fn impl_item_attributes(item: &syn::ImplItem) -> &[Attribute] {
        match item {
            syn::ImplItem::Const(item) => &item.attrs,
            syn::ImplItem::Fn(item) => &item.attrs,
            syn::ImplItem::Type(item) => &item.attrs,
            syn::ImplItem::Macro(item) => &item.attrs,
            _ => &[],
        }
    }

    fn trait_item_attributes(item: &syn::TraitItem) -> &[Attribute] {
        match item {
            syn::TraitItem::Const(item) => &item.attrs,
            syn::TraitItem::Fn(item) => &item.attrs,
            syn::TraitItem::Type(item) => &item.attrs,
            syn::TraitItem::Macro(item) => &item.attrs,
            _ => &[],
        }
    }

    fn generic_param_attributes(param: &syn::GenericParam) -> &[Attribute] {
        match param {
            syn::GenericParam::Lifetime(param) => &param.attrs,
            syn::GenericParam::Type(param) => &param.attrs,
            syn::GenericParam::Const(param) => &param.attrs,
        }
    }

    fn expr_attributes(expr: &syn::Expr) -> &[Attribute] {
        match expr {
            syn::Expr::Array(expr) => &expr.attrs,
            syn::Expr::Assign(expr) => &expr.attrs,
            syn::Expr::Async(expr) => &expr.attrs,
            syn::Expr::Await(expr) => &expr.attrs,
            syn::Expr::Binary(expr) => &expr.attrs,
            syn::Expr::Block(expr) => &expr.attrs,
            syn::Expr::Break(expr) => &expr.attrs,
            syn::Expr::Call(expr) => &expr.attrs,
            syn::Expr::Cast(expr) => &expr.attrs,
            syn::Expr::Closure(expr) => &expr.attrs,
            syn::Expr::Const(expr) => &expr.attrs,
            syn::Expr::Continue(expr) => &expr.attrs,
            syn::Expr::Field(expr) => &expr.attrs,
            syn::Expr::ForLoop(expr) => &expr.attrs,
            syn::Expr::Group(expr) => &expr.attrs,
            syn::Expr::If(expr) => &expr.attrs,
            syn::Expr::Index(expr) => &expr.attrs,
            syn::Expr::Infer(expr) => &expr.attrs,
            syn::Expr::Let(expr) => &expr.attrs,
            syn::Expr::Lit(expr) => &expr.attrs,
            syn::Expr::Loop(expr) => &expr.attrs,
            syn::Expr::Macro(expr) => &expr.attrs,
            syn::Expr::Match(expr) => &expr.attrs,
            syn::Expr::MethodCall(expr) => &expr.attrs,
            syn::Expr::Paren(expr) => &expr.attrs,
            syn::Expr::Path(expr) => &expr.attrs,
            syn::Expr::Range(expr) => &expr.attrs,
            syn::Expr::RawAddr(expr) => &expr.attrs,
            syn::Expr::Reference(expr) => &expr.attrs,
            syn::Expr::Repeat(expr) => &expr.attrs,
            syn::Expr::Return(expr) => &expr.attrs,
            syn::Expr::Struct(expr) => &expr.attrs,
            syn::Expr::Try(expr) => &expr.attrs,
            syn::Expr::TryBlock(expr) => &expr.attrs,
            syn::Expr::Tuple(expr) => &expr.attrs,
            syn::Expr::Unary(expr) => &expr.attrs,
            syn::Expr::Unsafe(expr) => &expr.attrs,
            syn::Expr::While(expr) => &expr.attrs,
            syn::Expr::Yield(expr) => &expr.attrs,
            _ => &[],
        }
    }

    fn pat_attributes(pat: &syn::Pat) -> &[Attribute] {
        match pat {
            syn::Pat::Const(pat) => &pat.attrs,
            syn::Pat::Ident(pat) => &pat.attrs,
            syn::Pat::Lit(pat) => &pat.attrs,
            syn::Pat::Macro(pat) => &pat.attrs,
            syn::Pat::Or(pat) => &pat.attrs,
            syn::Pat::Paren(pat) => &pat.attrs,
            syn::Pat::Path(pat) => &pat.attrs,
            syn::Pat::Range(pat) => &pat.attrs,
            syn::Pat::Reference(pat) => &pat.attrs,
            syn::Pat::Rest(pat) => &pat.attrs,
            syn::Pat::Slice(pat) => &pat.attrs,
            syn::Pat::Struct(pat) => &pat.attrs,
            syn::Pat::Tuple(pat) => &pat.attrs,
            syn::Pat::TupleStruct(pat) => &pat.attrs,
            syn::Pat::Type(pat) => &pat.attrs,
            syn::Pat::Wild(pat) => &pat.attrs,
            _ => &[],
        }
    }

    fn attribute_excludes_production(attribute: &Attribute) -> bool {
        let Meta::List(meta) = &attribute.meta else {
            return false;
        };
        if attribute.path().is_ident("cfg") {
            return cfg_truth_in_production(meta) == CfgTruth::False;
        }
        if !attribute.path().is_ident("cfg_attr") {
            return false;
        }
        let Ok(arguments) =
            Punctuated::<Meta, Token![,]>::parse_terminated.parse2(meta.tokens.clone())
        else {
            return false;
        };
        let mut arguments = arguments.into_iter();
        let Some(condition) = arguments.next() else {
            return false;
        };
        if meta_truth_in_production(&condition) != CfgTruth::True {
            return false;
        }
        arguments.any(|argument| {
            matches!(
                argument,
                Meta::List(action) if action.path.is_ident("cfg")
                    && cfg_truth_in_production(&action) == CfgTruth::False
            )
        })
    }

    #[derive(Clone, Copy, Eq, PartialEq)]
    enum CfgTruth {
        True,
        False,
        Unknown,
    }

    fn cfg_truth_in_production(meta: &syn::MetaList) -> CfgTruth {
        meta.parse_args::<Meta>()
            .map(|meta| meta_truth_in_production(&meta))
            .unwrap_or(CfgTruth::Unknown)
    }

    fn meta_truth_in_production(meta: &Meta) -> CfgTruth {
        match meta {
            Meta::Path(path) if path.is_ident("test") => CfgTruth::False,
            Meta::List(meta) if meta.path.is_ident("not") => {
                let Ok(inner) = syn::parse2::<Meta>(meta.tokens.clone()) else {
                    return CfgTruth::Unknown;
                };
                match meta_truth_in_production(&inner) {
                    CfgTruth::True => CfgTruth::False,
                    CfgTruth::False => CfgTruth::True,
                    CfgTruth::Unknown => CfgTruth::Unknown,
                }
            }
            Meta::List(meta) if meta.path.is_ident("all") => {
                combine_cfg_truths(meta, CfgTruth::True, |left, right| match (left, right) {
                    (CfgTruth::False, _) | (_, CfgTruth::False) => CfgTruth::False,
                    (CfgTruth::True, CfgTruth::True) => CfgTruth::True,
                    _ => CfgTruth::Unknown,
                })
            }
            Meta::List(meta) if meta.path.is_ident("any") => {
                combine_cfg_truths(meta, CfgTruth::False, |left, right| match (left, right) {
                    (CfgTruth::True, _) | (_, CfgTruth::True) => CfgTruth::True,
                    (CfgTruth::False, CfgTruth::False) => CfgTruth::False,
                    _ => CfgTruth::Unknown,
                })
            }
            _ => CfgTruth::Unknown,
        }
    }

    fn combine_cfg_truths(
        meta: &syn::MetaList,
        initial: CfgTruth,
        combine: impl Fn(CfgTruth, CfgTruth) -> CfgTruth,
    ) -> CfgTruth {
        let Ok(members) =
            Punctuated::<Meta, Token![,]>::parse_terminated.parse2(meta.tokens.clone())
        else {
            return CfgTruth::Unknown;
        };
        members
            .iter()
            .map(meta_truth_in_production)
            .fold(initial, combine)
    }

    #[derive(Debug, Clone)]
    struct ImportPath {
        segments: Vec<String>,
        glob: bool,
    }

    fn flatten_use_tree(tree: &UseTree) -> Vec<ImportPath> {
        let mut imports = Vec::new();
        flatten_use_tree_into(tree, Vec::new(), &mut imports);
        imports
    }

    fn flatten_use_tree_into(
        tree: &UseTree,
        mut prefix: Vec<String>,
        imports: &mut Vec<ImportPath>,
    ) {
        match tree {
            UseTree::Path(path) => {
                prefix.push(path.ident.to_string());
                flatten_use_tree_into(&path.tree, prefix, imports);
            }
            UseTree::Name(name) => {
                prefix.push(name.ident.to_string());
                imports.push(ImportPath {
                    segments: prefix,
                    glob: false,
                });
            }
            UseTree::Rename(rename) => {
                prefix.push(rename.ident.to_string());
                imports.push(ImportPath {
                    segments: prefix,
                    glob: false,
                });
            }
            UseTree::Glob(_) => imports.push(ImportPath {
                segments: prefix,
                glob: true,
            }),
            UseTree::Group(group) => {
                for tree in &group.items {
                    flatten_use_tree_into(tree, prefix.clone(), imports);
                }
            }
        }
    }

    fn import_is_forbidden(import: &ImportPath) -> bool {
        path_is_forbidden(&import.segments)
    }

    fn path_is_forbidden(segments: &[String]) -> bool {
        match segments {
            [root, module, ..]
                if root == "crate" && FORBIDDEN_KERNEL_CRATE_MODULES.contains(&module.as_str()) =>
            {
                true
            }
            [root, ..] if FORBIDDEN_EXTERNAL_CRATES.contains(&root.as_str()) => true,
            [root, module, ..]
                if root == "std" && FORBIDDEN_STD_MODULES.contains(&module.as_str()) =>
            {
                true
            }
            _ => false,
        }
    }

    fn path_is_clock_read(segments: &[String]) -> bool {
        matches!(
            segments,
            [.., clock, now]
                if matches!(clock.as_str(), "Utc" | "Local" | "Instant" | "SystemTime")
                    && now == "now"
        )
    }

    fn is_blanket_domain_reexport(import: &ImportPath) -> bool {
        matches!(
            import.segments.as_slice(),
            [domain] if domain == "ocg_domain"
        ) || matches!(
            import.segments.as_slice(),
            [domain, self_segment] if domain == "ocg_domain" && self_segment == "self"
        ) || matches!(
            import.segments.as_slice(),
            [domain, module]
                if domain == "ocg_domain"
                    && ["catalog", "ids", "pricing", "protocol", "zen"].contains(&module.as_str())
        ) || (import
            .segments
            .first()
            .is_some_and(|segment| segment == "ocg_domain")
            && import.glob)
    }

    fn production_graph(
        src_root: &Path,
        modules: &BTreeSet<String>,
    ) -> BTreeMap<String, BTreeSet<String>> {
        let mut graph: BTreeMap<String, BTreeSet<String>> = modules
            .iter()
            .cloned()
            .map(|name| (name, BTreeSet::new()))
            .collect();
        visit_rust_files(src_root, &mut |path| {
            let Some(from) = module_of(src_root, path, modules) else {
                return;
            };
            let production = production_source(&read_to_string(path));
            for target in crate_path_roots(&production) {
                if target != from && modules.contains(&target) {
                    graph.entry(from.clone()).or_default().insert(target);
                }
            }
        });
        graph
    }

    fn declared_modules(lib_source: &str) -> BTreeSet<String> {
        let mut modules = BTreeSet::new();
        let chars: Vec<char> = lib_source.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            skip_ws_idx(&chars, &mut i);
            if match_keyword(&chars, i, "pub") {
                i += 3;
                skip_ws_idx(&chars, &mut i);
                if match_prefix(&chars, i, "(crate)") {
                    i += "(crate)".len();
                    skip_ws_idx(&chars, &mut i);
                }
            }
            if match_keyword(&chars, i, "mod") {
                i += 3;
                skip_ws_idx(&chars, &mut i);
                if let Some(name) = take_ident_idx(&chars, &mut i) {
                    skip_ws_idx(&chars, &mut i);
                    if i < chars.len() && chars[i] == ';' {
                        modules.insert(name);
                    }
                }
                continue;
            }
            i += 1;
        }
        modules
    }

    fn module_of(src_root: &Path, path: &Path, modules: &BTreeSet<String>) -> Option<String> {
        let rel = path.strip_prefix(src_root).ok()?;
        let mut components = rel.components();
        let first = components.next()?.as_os_str().to_string_lossy();
        let name = first.strip_suffix(".rs").unwrap_or(&first);
        if name == "lib" {
            return None;
        }
        modules.contains(name).then(|| name.to_string())
    }

    fn visit_rust_files(root: &Path, visit: &mut impl FnMut(&Path)) {
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let entries = fs::read_dir(&dir)
                .unwrap_or_else(|error| panic!("read {}: {error}", dir.display()));
            for entry in entries {
                let entry = entry.unwrap_or_else(|error| panic!("dir entry: {error}"));
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                    visit(&path);
                }
            }
        }
    }

    fn production_source(source: &str) -> String {
        strip_cfg_test_items(&strip_comments_and_strings(source))
    }

    fn strip_comments_and_strings(source: &str) -> String {
        let chars: Vec<char> = source.chars().collect();
        let mut out = String::with_capacity(chars.len());
        let mut i = 0;
        while i < chars.len() {
            let ch = chars[i];
            let next = chars.get(i + 1).copied();
            if ch == '/' && next == Some('/') {
                while i < chars.len() && chars[i] != '\n' {
                    out.push(' ');
                    i += 1;
                }
                continue;
            }
            if ch == '/' && next == Some('*') {
                let mut depth = 1;
                out.push(' ');
                out.push(' ');
                i += 2;
                while i < chars.len() && depth > 0 {
                    if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
                        depth += 1;
                        out.push(' ');
                        out.push(' ');
                        i += 2;
                        continue;
                    }
                    if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                        depth -= 1;
                        out.push(' ');
                        out.push(' ');
                        i += 2;
                        continue;
                    }
                    out.push(if chars[i] == '\n' { '\n' } else { ' ' });
                    i += 1;
                }
                continue;
            }
            if (ch == 'r' || (ch == 'b' && next == Some('r'))) && looks_like_raw_string(&chars, i) {
                let start = if ch == 'b' { i + 1 } else { i };
                let mut hashes = 0;
                let mut j = start + 1;
                while j < chars.len() && chars[j] == '#' {
                    hashes += 1;
                    j += 1;
                }
                while i < j + 1 {
                    out.push(' ');
                    i += 1;
                }
                while i < chars.len() {
                    if chars[i] == '"' && raw_string_end(&chars, i + 1, hashes) {
                        out.push(' ');
                        i += 1;
                        for _ in 0..hashes {
                            out.push(' ');
                            i += 1;
                        }
                        break;
                    }
                    out.push(if chars[i] == '\n' { '\n' } else { ' ' });
                    i += 1;
                }
                continue;
            }
            if (ch == 'b' && next == Some('"')) || ch == '"' {
                if ch == 'b' {
                    out.push(' ');
                    i += 1;
                }
                out.push(' ');
                i += 1;
                while i < chars.len() {
                    if chars[i] == '\\' {
                        out.push(' ');
                        out.push(' ');
                        i += 2;
                        continue;
                    }
                    if chars[i] == '"' {
                        out.push(' ');
                        i += 1;
                        break;
                    }
                    out.push(if chars[i] == '\n' { '\n' } else { ' ' });
                    i += 1;
                }
                continue;
            }
            if ch == '\'' {
                if let Some(&ident_start) = chars.get(i + 1) {
                    if is_ident_start(ident_start) {
                        if chars.get(i + 2) == Some(&'\'') {
                            out.push(' ');
                            out.push(' ');
                            out.push(' ');
                            i += 3;
                            continue;
                        }
                        out.push(ch);
                        i += 1;
                        continue;
                    }
                }
                out.push(' ');
                i += 1;
                if i < chars.len() && chars[i] == '\\' {
                    out.push(' ');
                    i += 1;
                }
                if i < chars.len() {
                    out.push(if chars[i] == '\n' { '\n' } else { ' ' });
                    i += 1;
                }
                if i < chars.len() && chars[i] == '\'' {
                    out.push(' ');
                    i += 1;
                }
                continue;
            }
            out.push(ch);
            i += 1;
        }
        out
    }

    fn looks_like_raw_string(chars: &[char], i: usize) -> bool {
        let start = if chars[i] == 'b' { i + 1 } else { i };
        if start >= chars.len() || chars[start] != 'r' {
            return false;
        }
        let mut j = start + 1;
        while j < chars.len() && chars[j] == '#' {
            j += 1;
        }
        j < chars.len() && chars[j] == '"'
    }

    fn raw_string_end(chars: &[char], mut i: usize, hashes: usize) -> bool {
        for _ in 0..hashes {
            if i >= chars.len() || chars[i] != '#' {
                return false;
            }
            i += 1;
        }
        true
    }

    fn strip_cfg_test_items(source: &str) -> String {
        let chars: Vec<char> = source.chars().collect();
        let mut out = String::with_capacity(chars.len());
        let mut i = 0;
        while i < chars.len() {
            if chars[i] == '#' {
                let start = i;
                let mut cursor = i;
                let mut saw_cfg_test = false;
                loop {
                    skip_ws_idx(&chars, &mut cursor);
                    let Some(end) = parse_attribute(&chars, cursor) else {
                        break;
                    };
                    let attr: String = chars[cursor..end].iter().collect();
                    if attr_is_cfg_test(&attr) {
                        saw_cfg_test = true;
                    }
                    cursor = end;
                }
                if saw_cfg_test {
                    let end = skip_item(&chars, cursor);
                    for ch in &chars[start..end] {
                        out.push(if *ch == '\n' { '\n' } else { ' ' });
                    }
                    i = end;
                    continue;
                }
            }
            out.push(chars[i]);
            i += 1;
        }
        out
    }

    fn parse_attribute(chars: &[char], i: usize) -> Option<usize> {
        if i >= chars.len() || chars[i] != '#' {
            return None;
        }
        let mut j = i + 1;
        if j < chars.len() && chars[j] == '!' {
            j += 1;
        }
        skip_ws_idx(chars, &mut j);
        if j >= chars.len() || chars[j] != '[' {
            return None;
        }
        let mut depth = 0;
        while j < chars.len() {
            match chars[j] {
                '[' => depth += 1,
                ']' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(j + 1);
                    }
                }
                _ => {}
            }
            j += 1;
        }
        None
    }

    fn attr_is_cfg_test(attr: &str) -> bool {
        let compact: String = attr.chars().filter(|ch| !ch.is_whitespace()).collect();
        if compact.contains("cfg(not(test)") {
            return false;
        }
        compact.contains("cfg(test)")
            || compact.contains("cfg(all(test")
            || compact.contains("cfg(any(test")
    }

    fn skip_item(chars: &[char], mut i: usize) -> usize {
        skip_ws_idx(chars, &mut i);
        let mut paren = 0i32;
        while i < chars.len() {
            match chars[i] {
                '(' => paren += 1,
                ')' => paren = paren.saturating_sub(1),
                '{' if paren == 0 => return skip_balanced(chars, i, '{', '}'),
                ';' if paren == 0 => return i + 1,
                _ => {}
            }
            i += 1;
        }
        chars.len()
    }

    fn skip_balanced(chars: &[char], mut i: usize, open: char, close: char) -> usize {
        let mut depth = 0;
        while i < chars.len() {
            if chars[i] == open {
                depth += 1;
            } else if chars[i] == close {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            i += 1;
        }
        chars.len()
    }

    fn crate_path_roots(source: &str) -> BTreeSet<String> {
        let chars: Vec<char> = source.chars().collect();
        let mut roots = BTreeSet::new();
        let mut i = 0;
        while i + 7 <= chars.len() {
            if match_prefix(&chars, i, "crate::") {
                let boundary = i == 0 || !is_ident_continue(chars[i - 1]);
                if boundary {
                    i += 7;
                    if let Some(name) = take_ident_idx(&chars, &mut i) {
                        roots.insert(name);
                        continue;
                    }
                }
            }
            i += 1;
        }
        roots
    }

    fn alias_imports_custom(source: &str) -> bool {
        let parsed = parse_rust_file(Path::new("alias.rs"), source);
        parsed.items.into_iter().any(|item| {
            let Item::Use(item_use) = item else {
                return false;
            };
            flatten_use_tree(&item_use.tree).into_iter().any(|import| {
                matches!(
                    import.segments.as_slice(),
                    [root, module, ..]
                        if matches!(root.as_str(), "crate" | "super") && module == "custom"
                )
            })
        })
    }

    fn tarjan(graph: &BTreeMap<String, BTreeSet<String>>) -> Vec<BTreeSet<String>> {
        let mut index = 0usize;
        let mut stack = Vec::new();
        let mut indices = BTreeMap::new();
        let mut lowlink = BTreeMap::new();
        let mut on_stack = BTreeSet::new();
        let mut sccs = Vec::new();

        #[allow(clippy::too_many_arguments)]
        fn connect(
            node: &str,
            graph: &BTreeMap<String, BTreeSet<String>>,
            index: &mut usize,
            stack: &mut Vec<String>,
            indices: &mut BTreeMap<String, usize>,
            lowlink: &mut BTreeMap<String, usize>,
            on_stack: &mut BTreeSet<String>,
            sccs: &mut Vec<BTreeSet<String>>,
        ) {
            indices.insert(node.to_string(), *index);
            lowlink.insert(node.to_string(), *index);
            *index += 1;
            stack.push(node.to_string());
            on_stack.insert(node.to_string());
            for next in graph.get(node).into_iter().flatten() {
                if !indices.contains_key(next) {
                    connect(next, graph, index, stack, indices, lowlink, on_stack, sccs);
                    let next_low = lowlink[next];
                    let current = lowlink.get_mut(node).expect("lowlink");
                    *current = (*current).min(next_low);
                } else if on_stack.contains(next) {
                    let next_index = indices[next];
                    let current = lowlink.get_mut(node).expect("lowlink");
                    *current = (*current).min(next_index);
                }
            }
            if lowlink[node] == indices[node] {
                let mut component = BTreeSet::new();
                loop {
                    let item = stack.pop().expect("scc stack");
                    on_stack.remove(&item);
                    let done = item == node;
                    component.insert(item);
                    if done {
                        break;
                    }
                }
                sccs.push(component);
            }
        }

        for node in graph.keys() {
            if !indices.contains_key(node) {
                connect(
                    node,
                    graph,
                    &mut index,
                    &mut stack,
                    &mut indices,
                    &mut lowlink,
                    &mut on_stack,
                    &mut sccs,
                );
            }
        }
        sccs
    }

    fn read_to_string(path: &Path) -> String {
        fs::read_to_string(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
    }

    fn skip_ws_idx(chars: &[char], i: &mut usize) {
        while *i < chars.len() && chars[*i].is_whitespace() {
            *i += 1;
        }
    }

    fn take_ident_idx(chars: &[char], i: &mut usize) -> Option<String> {
        if *i >= chars.len() || !is_ident_start(chars[*i]) {
            return None;
        }
        let start = *i;
        *i += 1;
        while *i < chars.len() && is_ident_continue(chars[*i]) {
            *i += 1;
        }
        Some(chars[start..*i].iter().collect())
    }

    fn match_prefix(chars: &[char], i: usize, needle: &str) -> bool {
        for (offset, ch) in needle.chars().enumerate() {
            if chars.get(i + offset) != Some(&ch) {
                return false;
            }
        }
        true
    }

    fn match_keyword(chars: &[char], i: usize, needle: &str) -> bool {
        if !match_prefix(chars, i, needle) {
            return false;
        }
        let end = i + needle.chars().count();
        end == chars.len() || !is_ident_continue(chars[end])
    }

    fn is_ident_start(ch: char) -> bool {
        ch == '_' || ch.is_ascii_alphabetic()
    }

    fn is_ident_continue(ch: char) -> bool {
        ch == '_' || ch.is_ascii_alphanumeric()
    }
}
