import assert from "node:assert/strict";
import test from "node:test";
import type { ProviderCatalogEntry } from "../api/providers.ts";
import {
  MANUAL_PRESET_QUERY_VALUE,
  accountCountByProviderId,
  catalogEntryCredentialState,
  catalogEntryFamily,
  filterCatalogEntries,
  groupCatalogEntriesByOffering,
  providerAddStageFromQuery,
  providerAddStageToQuery,
  railCatalogEntries,
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

test("catalog entries group by offering and preserve catalog order within a group", () => {
  const entries = [
    catalogEntry("opencode", { offering: "plan" }),
    catalogEntry("ollama"),
    catalogEntry("minimax", { offering: "plan" }),
    catalogEntry("custom"),
    catalogEntry("acme", { origin: "custom", offering: "plan" }),
  ];
  const groups = groupCatalogEntriesByOffering(entries);
  assert.deepEqual(groups.plan.map((entry) => entry.provider_id), ["opencode", "minimax", "acme"]);
  assert.deepEqual(groups.api.map((entry) => entry.provider_id), ["ollama", "custom"]);
});

test("rail hides unused builtins and keeps saved preset or custom connections", () => {
  const entries = [
    catalogEntry("opencode", { display_name: "OpenCode Go", offering: "plan" }),
    catalogEntry("kimi", { display_name: "Kimi Code CN", offering: "plan" }),
    catalogEntry("tencent", { origin: "preset", display_name: "Tencent Token Plan", offering: "plan" }),
    catalogEntry("lab", { origin: "custom", display_name: "Lab HTTP" }),
  ];
  assert.deepEqual(
    railCatalogEntries(entries, ["opencode"]).map((entry) => entry.provider_id),
    ["opencode", "tencent", "lab"],
  );
  assert.deepEqual(
    railCatalogEntries(entries, ["OPENCODE", "tencent"]).map((entry) => entry.provider_id),
    ["opencode", "tencent", "lab"],
  );
  assert.deepEqual(
    railCatalogEntries(entries, ["lab"]).map((entry) => entry.provider_id),
    ["tencent", "lab"],
  );
  assert.deepEqual(
    railCatalogEntries(entries, []).map((entry) => entry.provider_id),
    ["tencent", "lab"],
  );
  assert.deepEqual(
    railCatalogEntries(entries, ["KIMI"]).map((entry) => entry.provider_id),
    ["kimi", "tencent", "lab"],
  );
});

test("credential state is not_required, missing, or present from kind and count", () => {
  const keyed = catalogEntry("lab", { credential_kind: "api_key" });
  const none = catalogEntry("local", { credential_kind: "none" });
  assert.equal(catalogEntryCredentialState(none, 0), "not_required");
  assert.equal(catalogEntryCredentialState(none, 2), "not_required");
  assert.equal(catalogEntryCredentialState(keyed, 0), "missing");
  assert.equal(catalogEntryCredentialState(keyed, 1), "present");
  const counts = accountCountByProviderId([
    { provider_id: "LAB" },
    { provider_id: " lab " },
    { provider_id: "other" },
  ]);
  assert.equal(counts.get("lab"), 2);
  assert.equal(counts.get("other"), 1);
  assert.equal(counts.get("missing"), undefined);
});

test("rail filtering matches display name and provider id case-insensitively", () => {
  const entries = [
    catalogEntry("opencode", { display_name: "OpenCode Go" }),
    catalogEntry("kimi", { display_name: "Kimi Code CN" }),
  ];
  assert.deepEqual(filterCatalogEntries(entries, "  ").map((entry) => entry.provider_id), ["opencode", "kimi"]);
  assert.deepEqual(filterCatalogEntries(entries, "OPENCODE").map((entry) => entry.provider_id), ["opencode"]);
  assert.deepEqual(filterCatalogEntries(entries, "code cn").map((entry) => entry.provider_id), ["kimi"]);
  assert.deepEqual(filterCatalogEntries(entries, "nope"), []);
});

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
