import assert from "node:assert/strict";
import test from "node:test";
import type { ConnectionEndpoint } from "../api/connections.ts";
import type { Destination } from "../api/destinations.ts";
import {
  providerProtocolGrantCaptureIsCurrent,
  providerProtocolEndpointId,
  providerProtocolGrantCandidates,
} from "./provider-protocol-grants.ts";

const destination = (overrides: Partial<Destination> = {}): Destination => ({
  account_controls: { toggleWrite: "account", configurationOwner: "destination", consoleLink: null, browserProfile: false },
  adapter: "opencode_go",
  auth_scheme: "bearer",
  base_url: "https://go.example/v1",
  brand_family: null,
  capabilities: {
    billing_tier_required: false,
    discoverable_models: false,
    external_integration: false,
    identity_headers: false,
    managed_signup: false,
    observer: false,
    official_balance_probe: [],
    redirect_policy: "no_follow",
    testable: true,
  },
  catalog: [],
  enabled: true,
  id: "go-destination",
  legacy: { kind: "builtin", id: "opencode" },
  max_credentials: null,
  name: "Go",
  observer_credential_id: null,
  plan: null,
  protocols: ["chat_completions", "responses"],
  ...overrides,
});

test("a deferred grant choice becomes stale after the provider revision changes", () => {
  const capture = {
    connectionId: "go",
    destinationId: "go-destination",
    scopeKey: "provider:opencode",
    expectation: { expectedRevision: 8, processGeneration: 22 },
  };
  assert.equal(providerProtocolGrantCaptureIsCurrent(capture, capture), true);
  assert.equal(providerProtocolGrantCaptureIsCurrent(capture, {
    ...capture,
    expectation: { expectedRevision: 9, processGeneration: 22 },
  }), false);
});

const endpoints: ConnectionEndpoint[] = [
  { id: "chat", connection_id: "go", auth_scheme: "bearer", locked: true, operation: "chat_create", url: "https://go.example/v1", wire_protocol: "chat_completions" },
  { id: "response", connection_id: "go", auth_scheme: "bearer", locked: true, operation: "response_create", url: "https://go.example/v1", wire_protocol: "responses" },
  // A response operation with the wrong wire protocol must not authorize Responses.
  { id: "wrong-wire", connection_id: "go", auth_scheme: "bearer", locked: true, operation: "response_create", url: "https://go.example/v1", wire_protocol: "chat_completions" },
];

const key = (id: string, destinationId = "go-destination", grants: string[] = []) => ({
  id,
  name: `Key ${id}`,
  destination_id: destinationId,
  has_secret: true,
  grants: { allowed_endpoint_ids: grants, allowed_origins: ["https://go.example"] },
});

test("sealed protocol grants only flag the newly enabled endpoint operation", () => {
  const candidates = providerProtocolGrantCandidates(destination(), [
    key("already", "go-destination", ["chat", "response"]),
    key("needs-response", "go-destination", ["chat"]),
    key("other-provider", "other-destination", []),
  ], endpoints, [
    { model_id: "mimo-v2.6-flash", protocol: "responses", state: "force_on" },
  ]);

  assert.deepEqual(candidates, [{
    id: "needs-response",
    name: "Key needs-response",
    missingProtocols: ["responses"],
  }]);
  assert.equal(providerProtocolEndpointId(endpoints, "responses"), "response");
  assert.equal(
    providerProtocolEndpointId(endpoints.filter((endpoint) => endpoint.id !== "response"), "responses"),
    null,
    "a matching operation with another wire protocol is not an endpoint grant",
  );
});

test("HTTP and observer destinations never infer a provider endpoint grant", () => {
  const overrides = [{ model_id: "model", protocol: "responses" as const, state: "force_on" as const }];
  assert.deepEqual(
    providerProtocolGrantCandidates(destination({ adapter: "http" }), [key("http")], endpoints, overrides),
    [],
  );
  assert.deepEqual(
    providerProtocolGrantCandidates(destination({ capabilities: { ...destination().capabilities, observer: true } }), [key("observer")], endpoints, overrides),
    [],
  );
});

test("a Key without a stored secret is not an authorization candidate", () => {
  assert.deepEqual(providerProtocolGrantCandidates(destination(), [{
    ...key("empty"),
    has_secret: false,
  }], endpoints, [{ model_id: "model", protocol: "responses", state: "force_on" }]), []);
});
