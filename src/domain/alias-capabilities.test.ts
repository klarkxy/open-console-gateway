import assert from "node:assert/strict";
import test from "node:test";
import type {
  Destination,
  DestinationModelMetadataEntryView,
  DestinationModelMetadataSnapshot,
} from "../api/destinations.ts";
import {
  ALIAS_CAPABILITY_STATE_KEYS,
  aliasCapabilityView,
  aliasRowDestinationId,
} from "./alias-capabilities.ts";
import { CPA_PROVIDER_ID } from "./destination-providers.ts";
import type { ProviderAliasRow } from "./provider-aliases.ts";

function destination(overrides: Partial<Destination> = {}): Destination {
  return {
    account_controls: { toggleWrite: "account", configurationOwner: "destination", consoleLink: null, browserProfile: false },
    adapter: "http",
    auth_scheme: "bearer",
    base_url: "https://api.lab.example/v1",
    brand_family: null,
    capabilities: {
      billing_tier_required: false,
      discoverable_models: true,
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
    id: "dest-1",
    legacy: { kind: "dynamic", id: "prov-lab" },
    max_credentials: null,
    name: "Lab HTTP",
    observer_credential_id: null,
    plan: null,
    protocols: ["chat_completions"],
    ...overrides,
  };
}

function row(overrides: Partial<ProviderAliasRow> = {}): ProviderAliasRow {
  return {
    provider_id: "prov-lab",
    key: "k",
    public_model: "opus",
    provider_plan: "Lab",
    custom_account: null,
    upstream_model: "vendor/opus",
    routable: true,
    custom_account_id: null,
    ...overrides,
  };
}

function entry(overrides: Partial<DestinationModelMetadataEntryView> = {}): DestinationModelMetadataEntryView {
  return {
    public_model: "opus",
    upstream_model: "vendor/opus",
    metadata: {
      name: null,
      context_window: null,
      max_output_tokens: null,
      input_modalities: ["text", "image"],
      output_modalities: ["text"],
      reasoning: null,
      reasoning_efforts: null,
      tool_calling: null,
      parallel_tool_calls: null,
    },
    source: "upstream",
    ...overrides,
  };
}

function snapshot(models: DestinationModelMetadataEntryView[], id = "dest-1"): DestinationModelMetadataSnapshot {
  return { destination_id: id, models, expectation: { expectedRevision: 1, processGeneration: 1 } };
}

test("provider rows resolve their builtin or dynamic destination by legacy id", () => {
  const destinations = [
    destination({ id: "dest-builtin", legacy: { kind: "builtin", id: "opencode" } }),
    destination({ id: "dest-dyn", legacy: { kind: "dynamic", id: "prov-lab" } }),
  ];
  assert.equal(aliasRowDestinationId(row(), destinations), "dest-dyn");
  assert.equal(aliasRowDestinationId(row({ provider_id: "opencode" }), destinations), "dest-builtin");
  assert.equal(aliasRowDestinationId(row({ provider_id: "missing" }), destinations), null);
});

test("custom account rows join the account-owned destination; CPA rows own none", () => {
  const destinations = [
    destination({ id: "dest-acc", legacy: { kind: "custom_account", id: "acc-1" } }),
  ];
  assert.equal(
    aliasRowDestinationId(row({ provider_id: "custom", custom_account_id: "acc-1" }), destinations),
    "dest-acc",
  );
  assert.equal(aliasRowDestinationId(row({ provider_id: CPA_PROVIDER_ID }), destinations), null);
});

test("view is pending before the snapshot arrives, error after a failed load", () => {
  const destinations = [destination({ id: "dest-1", legacy: { kind: "dynamic", id: "prov-lab" } })];
  assert.equal(aliasCapabilityView(row(), destinations, {}, {}).state, "pending");
  assert.equal(aliasCapabilityView(row(), destinations, {}, { "dest-1": "boom" }).state, "error");
});

test("CPA rows are unavailable without consulting metadata", () => {
  const view = aliasCapabilityView(row({ provider_id: CPA_PROVIDER_ID }), [], {}, {});
  assert.equal(view.state, "unavailable");
  assert.equal(view.destination_id, null);
});

test("ready view carries modalities and provenance, matched by public model first", () => {
  const destinations = [destination({ id: "dest-1", legacy: { kind: "dynamic", id: "prov-lab" } })];
  const metadata = { "dest-1": snapshot([entry()]) };
  const view = aliasCapabilityView(row(), destinations, metadata, {});
  assert.equal(view.state, "ready");
  assert.equal(view.source, "upstream");
  assert.deepEqual(view.input_modalities, ["text", "image"]);
});

test("builtin rows fall back to upstream-model matching like the editor", () => {
  const destinations = [destination({ id: "dest-1", legacy: { kind: "builtin", id: "prov-lab" } })];
  const metadata = {
    "dest-1": snapshot([entry({ public_model: "other", upstream_model: "vendor/opus" })]),
  };
  const view = aliasCapabilityView(row(), destinations, metadata, {});
  assert.equal(view.state, "ready");
});

test("a route with unknown modalities stays unknown even when the source is known", () => {
  const destinations = [destination({ id: "dest-1", legacy: { kind: "dynamic", id: "prov-lab" } })];
  const metadata = {
    "dest-1": snapshot([entry({
      source: "modelsdev",
      metadata: { ...entry().metadata, input_modalities: null },
    })]),
  };
  const view = aliasCapabilityView(row(), destinations, metadata, {});
  assert.equal(view.state, "unknown");
  assert.equal(view.source, "modelsdev");
});

test("every non-ready state and source has a copy key", () => {
  assert.deepEqual(Object.keys(ALIAS_CAPABILITY_STATE_KEYS).sort(), ["error", "pending", "unavailable", "unknown"]);
});
