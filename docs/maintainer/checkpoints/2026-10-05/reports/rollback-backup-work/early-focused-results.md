# Early focused results (resume after coordinator isolation follow-up)

Frozen coherent_apply_fallback before source edits: exit 101, 0 passed, 1 failed, 1878 filtered, 19.42s, tests.rs:3188 unwrap_err on Ok connection. Classified obsolete blanket connectionErr.

Substring filter containing `|` ran 0 tests / exit 0. That is not PASS. Later exact/substring names were used.

Compiled fixture libtest `target/ocg3-cli-fixture/debug/deps/ocg_core-0d69f2dd74c63e71.exe`:

| pattern | passed | failed | filtered | seconds | exit |
| --- | --- | --- | --- | --- | --- |
| exact failed_apply_restores_the_accepted_port_not_the_candidate | 1 | 0 | 1885 | 20.02 | 0 |
| exact failed_apply_persist_error_does_not_publish_ready | 1 | 0 | 1885 | 15.68 | 0 |
| exact coherent_apply_fallback_serves_the_previous_child_until_credentials_move | 1 | 0 | 1885 | 19.58 | 0 |
| exact accepted_history_rollback_refuses_a_foreign_artifact_before_mutation | 1 | 0 | 1885 | 17.63 | 0 |
| exact accepted_snapshot_rejects_a_shared_keyed_identity_contradiction | 1 | 0 | 1885 | 0.00 | 0 |
| exact accepted_snapshot_keeps_native_only_routes_and_unknown_none | 1 | 0 | 1885 | 0.00 | 0 |
| exact parse_rejects_an_oversized_execution_record | 1 | 0 | 1885 | 0.00 | 0 |
| exact rebound_yaml_debug_redacts_private_projection | 1 | 0 | 1885 | 0.00 | 0 |
| exact cpa_execution::explain::tests::failed_apply_keeps_applied_client_route_distinct_from_desired | 0 | 1 | 1885 | 0.20 | 101 |

Explain isolate: left Excluded, right Client at explain/tests.rs:955. That world saves an in-memory Record without previous_accepted; maps_coherent snapshot identity is not on that load path. Not treated as a verified REVISE4 defect and not patched here.

Foreign cargo 33804 and 55520 left running.
