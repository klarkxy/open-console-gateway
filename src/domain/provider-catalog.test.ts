import assert from "node:assert/strict";
import test from "node:test";
import type { ProviderCatalogEntry } from "../api/providers.ts";
import {
  MANUAL_PRESET_QUERY_VALUE,
  catalogEntryFamily,
  providerAddStageFromQuery,
  providerAddStageToQuery,
} from "./provider-catalog.ts";
import { PROVIDER_PRESETS } from "./provider-presets.ts";

function catalogEntry(
  provider_id: string,
  extra: Partial<ProviderCatalogEntry> = {},
): ProviderCatalogEntry {
  return {
    provider_id,
    origin: "builtin",
    editable: false,
    deletable: false,
    offering: "api",
    display_name: provider_id,
    display_family: provider_id,
    credential_kind: "api_key",
    quota_scope: "key",
    singleton: false,
    creation_availability: "available",
    verification_policy: "required",
    verification_runtime_availability: "unavailable",
    routable: false,
    managed_registration: false,
    pricing_availability: "unavailable",
    usage_availability: "unavailable",
    manual_usage_calibration: false,
    quota_unit: "credits",
    model_source: "test",
    auth_schemes: ["bearer"],
    upstream_protocols: ["chat_completions"],
    form_fields: [],
    model_aliases: [],
    ...extra,
  };
}

test("brand families use the vendor family when known and a monogram otherwise", () => {
  const minimax = catalogEntryFamily(catalogEntry("minimax", { display_family: "MiniMax" }));
  assert.equal(minimax.id, "minimax");
  // Kimi Code CN carries the Moonshot / Kimi vendor brand.
  const kimi = catalogEntryFamily(catalogEntry("kimi", { display_family: "Kimi" }));
  assert.equal(kimi.id, "moonshot");
  const custom = catalogEntryFamily(
    catalogEntry("my-lab", { display_family: "", display_name: "My Lab" }),
  );
  assert.equal(custom.id, "my-lab");
  assert.equal(custom.label, "My Lab");
  assert.match(custom.tint, /^#[0-9A-Fa-f]{6}$/);
});

test("add-flow stages round-trip through the URL query", () => {
  assert.equal(providerAddStageFromQuery(false, null), null);
  assert.deepEqual(providerAddStageFromQuery(true, null), { stage: "browse" });
  assert.deepEqual(
    providerAddStageFromQuery(true, MANUAL_PRESET_QUERY_VALUE),
    { stage: "form", presetId: null },
  );
  const preset = PROVIDER_PRESETS[0];
  assert.ok(preset, "fixture presets must exist");
  assert.deepEqual(
    providerAddStageFromQuery(true, preset.id),
    { stage: "form", presetId: preset.id },
  );
  // Unknown preset ids degrade to the browser, never a blank form.
  assert.deepEqual(providerAddStageFromQuery(true, "not-a-preset"), { stage: "browse" });
  assert.deepEqual(providerAddStageToQuery({ stage: "browse" }), { add: true });
  assert.deepEqual(
    providerAddStageToQuery({ stage: "form", presetId: null }),
    { add: true, preset: MANUAL_PRESET_QUERY_VALUE },
  );
  assert.deepEqual(
    providerAddStageToQuery({ stage: "form", presetId: preset.id }),
    { add: true, preset: preset.id },
  );
});
