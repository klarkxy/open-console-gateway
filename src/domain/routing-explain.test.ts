import assert from "node:assert/strict";
import test from "node:test";
import type {
  RoutingChannel,
  RoutingClientProtocol,
  RoutingExclusionCode,
  RoutingExplanationView,
  RoutingMode,
  RoutingResolvedKind,
  RuntimeOnlyUncertainty,
} from "../api/destinations.ts";
import {
  ROUTING_CHANNEL_KEYS,
  ROUTING_CLIENT_PROTOCOL_KEYS,
  ROUTING_EXCLUSION_KEYS,
  ROUTING_EXPLANATION_STATE_KEYS,
  ROUTING_MODE_KEYS,
  ROUTING_RESOLVED_KIND_KEYS,
  ROUTING_UNCERTAINTY_KEYS,
  routingEligibleOrdered,
  routingExplanationState,
} from "./routing-explain.ts";

test("copy tables cover every wire code", () => {
  const modes: RoutingMode[] = ["strict-priority", "sticky-global", "round-robin"];
  for (const code of modes) assert.ok(ROUTING_MODE_KEYS[code]);
  const channels: RoutingChannel[] = ["go", "free"];
  for (const code of channels) assert.ok(ROUTING_CHANNEL_KEYS[code]);
  const kinds: RoutingResolvedKind[] = ["alias", "pinned_raw"];
  for (const code of kinds) assert.ok(ROUTING_RESOLVED_KIND_KEYS[code]);
  const protocols: RoutingClientProtocol[] = ["chat_completions", "responses", "messages", "gemini"];
  for (const code of protocols) assert.ok(ROUTING_CLIENT_PROTOCOL_KEYS[code]);
  const exclusions: RoutingExclusionCode[] = [
    "mapping_protocol_incompatible",
    "credential_disabled",
    "binding_disabled",
    "model_scope_denied",
    "goat_not_eligible",
    "goat_unverified",
    "candidate_materialization_failed",
    "production_route_unsupported",
    "account_disabled",
    "setup_not_ready",
    "channel_mismatch",
    "credential_missing",
    "auth_error",
    "cooling_down",
    "free_channel_unavailable",
  ];
  for (const code of exclusions) assert.ok(ROUTING_EXCLUSION_KEYS[code]);
  const uncertainties: RuntimeOnlyUncertainty[] = [
    "state_changed_after_snapshot",
    "conversation_binding_not_evaluated",
    "retry_exclusions_not_applied",
    "credential_recheck_pending",
    "upstream_result_unknown",
  ];
  for (const code of uncertainties) assert.ok(ROUTING_UNCERTAINTY_KEYS[code]);
});

function explanation(
  overrides: Partial<RoutingExplanationView> = {},
): RoutingExplanationView {
  return {
    client_protocol: "chat_completions",
    conversation_binding: "not_evaluated",
    conversation_sticky: false,
    eligible: [],
    exclusions: [],
    expected_base_policy_first_pick: null,
    observed_at: "2026-09-20T00:00:00Z",
    requested_model: "lab-opus",
    resolved: { alias: "lab-opus", kind: "alias", mappings: [] },
    expectation: { expectedRevision: 1, processGeneration: 1 },
    routing_mode: "strict-priority",
    runtime_only_uncertainty: [],
    ...overrides,
  };
}

test("explanation state distinguishes routeable, excluded, and unresolved", () => {
  assert.equal(routingExplanationState(explanation()), "unresolved");
  assert.equal(
    routingExplanationState(explanation({
      resolved: {
        alias: "lab-opus",
        kind: "alias",
        mappings: [{ provider_id: "lab", routeable: true, upstream_model: "vendor/opus" }],
      },
    })),
    "excluded",
  );
  assert.equal(
    routingExplanationState(explanation({
      eligible: [{
        account_id: "acct-1",
        account_name: "Lab Key",
        adapter_kind: "http",
        channel: "go",
        destination_id: "dest-1",
        destination_name: "Lab HTTP",
        provider_id: "lab",
        resolved_model: "vendor/opus",
        routing_rank: 1,
        upstream_protocol: "chat_completions",
      }],
    })),
    "routeable",
  );
  for (const state of ["routeable", "excluded", "unresolved"] as const) {
    assert.ok(ROUTING_EXPLANATION_STATE_KEYS[state]);
  }
});

test("eligible candidates order by ascending global routing rank without mutating the snapshot", () => {
  const candidate = (accountId: string, routingRank: number) => ({
    account_id: accountId,
    account_name: `Key ${accountId}`,
    adapter_kind: "http",
    channel: "go" as const,
    destination_id: "dest-1",
    destination_name: "Lab HTTP",
    provider_id: "lab",
    resolved_model: "vendor/opus",
    routing_rank: routingRank,
    upstream_protocol: "chat_completions" as const,
  });
  const eligible = [candidate("b", 3), candidate("a", 1), candidate("c", 2)];
  const view = { eligible };
  const ordered = routingEligibleOrdered(view);
  assert.deepEqual(ordered.map((row) => row.account_id), ["a", "c", "b"]);
  assert.deepEqual(eligible.map((row) => row.account_id), ["b", "a", "c"]);
  assert.equal(ordered.length, 3);
});
