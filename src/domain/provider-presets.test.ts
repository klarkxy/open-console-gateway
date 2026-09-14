import assert from "node:assert/strict";
import test from "node:test";
import rawPresets from "../../resources/provider-presets.json" with { type: "json" };
import {
  PROVIDER_PRESETS,
  applyProviderPresetToDraft,
  filterProviderPresets,
  groupProviderPresetsByOffering,
  inferMappingPresetPrefix,
  normalizeProviderPresetEndpoint,
  parseProviderPresets,
  providerPresetEndpointPlaceholder,
  providerPresetImportPublicName,
  providerPresetModelDiscoveryEnabled,
  providerPresetNote,
  providerPresetDefaultModels,
  providerPresetOffering,
  providerPresetOfferingForId,
  providerPresetShapeIssues,
  resolveEditPreset,
  resolveProviderPreset,
  type ProviderPreset,
} from "./provider-presets.ts";
import { PROVIDER_FAMILIES } from "./provider-families.ts";
import { emptyProviderDefinitionDraft, buildProviderDefinitionUpdateBody, validateProviderDefinitionDraft, type ProviderDefinitionDraft } from "./dynamic-provider.ts";
import { buildOnboardingCommitPayload } from "./onboarding-draft.ts";

function samplePreset(extra: Partial<ProviderPreset> = {}): ProviderPreset {
  return {
    id: "anthropic",
    name: "Anthropic API",
    category: "official",
    endpointUrl: "https://api.anthropic.com/v1/messages",
    protocol: "messages",
    authKind: "x-api-key",
    docsUrl: "https://platform.claude.com/docs/en/api/overview",
    websiteUrl: "https://console.anthropic.com/",
    note: { en: "English note", zh: "中文备注" },
    ...extra,
  };
}

test("shipped preset data satisfies the frozen contract", () => {
  const issues = rawPresets.flatMap((row, index) => providerPresetShapeIssues(row, index));
  assert.deepEqual(issues, []);
  const parsed = parseProviderPresets(rawPresets);
  assert.equal(parsed.length, rawPresets.length);
  assert.equal(new Set(parsed.map((preset) => preset.id)).size, parsed.length);
});

test("every shipped preset's family resolves to a known PROVIDER_FAMILIES entry", () => {
  const familyIds = new Set(PROVIDER_FAMILIES.map((family) => family.id));
  const seenVariantsByFamily = new Map<string, Set<string>>();
  const multiPresetFamilies = new Set<string>();
  for (const preset of PROVIDER_PRESETS) {
    assert.ok(preset.family, `${preset.id} is missing a family id`);
    assert.ok(
      familyIds.has(preset.family),
      `${preset.id} references unknown family ${preset.family}`,
    );
    const groupSize = PROVIDER_PRESETS.filter((other) => other.family === preset.family).length;
    if (groupSize > 1) {
      multiPresetFamilies.add(preset.family);
      assert.ok(preset.variant, `${preset.id} is in a multi-preset family but has no variant`);
      const seen = seenVariantsByFamily.get(preset.family) ?? new Set();
      assert.ok(
        !seen.has(preset.variant!),
        `duplicate variant ${preset.variant} in family ${preset.family}`,
      );
      seen.add(preset.variant!);
      seenVariantsByFamily.set(preset.family, seen);
    } else {
      assert.equal(preset.variant, undefined, `${preset.id} is a singleton but carries a variant`);
    }
  }
  // Sanity: at least the well-known multi-preset vendors are grouped.
  for (const expected of ["tencent", "zhipu", "alibaba", "bytedance"]) {
    assert.ok(multiPresetFamilies.has(expected), `${expected} must be a multi-preset family`);
  }
});

test("shape issues flag malformed family and variant fields", () => {
  // Non-empty trimmed string is accepted; absent stays accepted.
  assert.deepEqual(providerPresetShapeIssues(samplePreset({ family: "anthropic" })), []);
  assert.deepEqual(providerPresetShapeIssues(samplePreset({ family: "anthropic", variant: "X" })), []);
  // Empty or whitespace-only family / variant is rejected.
  assert.ok(providerPresetShapeIssues(samplePreset({ family: "" })).length > 0);
  assert.ok(providerPresetShapeIssues(samplePreset({ family: "  " })).length > 0);
  assert.ok(providerPresetShapeIssues(samplePreset({ family: "anthropic", variant: "" })).length > 0);
  assert.ok(providerPresetShapeIssues(samplePreset({ family: "anthropic", variant: "  " })).length > 0);
  // Non-string family / variant is rejected.
  assert.ok(providerPresetShapeIssues(samplePreset({ family: 0 as never })).length > 0);
  assert.ok(providerPresetShapeIssues(samplePreset({ family: "anthropic", variant: 1 as never })).length > 0);
  // A variant without a family is a shape issue: it has nothing to scope to.
  assert.ok(providerPresetShapeIssues(samplePreset({ variant: "X" })).length > 0);
});

test("shape issues flag bad rows and the parser skips them", () => {
  assert.ok(providerPresetShapeIssues(null).length > 0);
  assert.ok(providerPresetShapeIssues(samplePreset({ category: "unknown" as never })).length > 0);
  assert.ok(providerPresetShapeIssues(samplePreset({ endpointUrl: "ftp://example.com" })).length > 0);
  assert.ok(
    providerPresetShapeIssues(samplePreset({ note: { en: "", zh: "仅中文" } })).length > 0,
  );
  assert.ok(
    providerPresetShapeIssues(samplePreset({ modelDiscovery: "yes" as never })).length > 0,
  );
  const parsed = parseProviderPresets([
    samplePreset(),
    samplePreset({ id: "broken", category: "unknown" as never }),
    samplePreset(),
  ]);
  assert.deepEqual(parsed.map((preset) => preset.id), ["anthropic"]);
});

test("applying a preset fills identity fields and clears Key, mappings, and account edits", () => {
  const dirty: ProviderDefinitionDraft = {
    ...emptyProviderDefinitionDraft(),
    name: "旧名字",
    endpoint_url: "https://old.example.com",
    upstream_protocol: "chat_completions",
    auth_kind: "bearer",
    key: "sk-secret-must-not-cross",
    models: [
      { public_model: "old-a", upstream_model: "old-a" },
      { public_model: "old-b", upstream_model: "old-b" },
    ],
    account_name: "主号",
    notes: "保留备注",
  };
  const applied = applyProviderPresetToDraft(dirty, samplePreset());
  assert.equal(applied.name, "Anthropic API");
  assert.equal(applied.endpoint_url, "https://api.anthropic.com/v1/messages");
  assert.equal(applied.upstream_protocol, "messages");
  assert.equal(applied.auth_kind, "x-api-key");
  assert.equal(applied.key, "");
  assert.deepEqual(applied.models, [{ public_model: "", upstream_model: "" }]);
  assert.equal(applied.account_name, "主号");
  assert.equal(applied.notes, "保留备注");
  // The source preset object is never mutated into a draft.
  assert.ok(!("key" in samplePreset()));
  assert.deepEqual(dirty.models.length, 2);
});

test("a blank preset endpoint stays blank; the placeholder is never applied as a value", () => {
  const azure = samplePreset({
    id: "azure-openai",
    endpointUrl: "",
    endpointPlaceholder: "https://<your-resource>.openai.azure.com/...",
  });
  const applied = applyProviderPresetToDraft(emptyProviderDefinitionDraft(), azure);
  assert.equal(applied.endpoint_url, "");
  assert.equal(providerPresetEndpointPlaceholder(azure), "https://<your-resource>.openai.azure.com/...");
  assert.notEqual(applied.endpoint_url, providerPresetEndpointPlaceholder(azure));
  assert.equal(providerPresetEndpointPlaceholder(samplePreset()), "");
});

test("switching back to manual resets preset fields but still clears secrets and mappings", () => {
  const applied = applyProviderPresetToDraft(emptyProviderDefinitionDraft(), samplePreset());
  assert.equal(applied.preset_id, "anthropic");
  applied.key = "sk-typed-after-apply";
  applied.models = [{ public_model: "claude", upstream_model: "claude" }];
  const manual = applyProviderPresetToDraft(applied, null);
  const empty = emptyProviderDefinitionDraft();
  assert.equal(manual.name, "");
  assert.equal(manual.endpoint_url, "");
  assert.equal(manual.upstream_protocol, empty.upstream_protocol);
  assert.equal(manual.auth_kind, empty.auth_kind);
  assert.equal(manual.key, "");
  assert.deepEqual(manual.models, [{ public_model: "", upstream_model: "" }]);
  assert.equal(manual.preset_id, "");
});

test("shipped presets keep official or aggregator category", () => {
  assert.ok(PROVIDER_PRESETS.some((preset) => preset.category === "official"));
  assert.ok(PROVIDER_PRESETS.some((preset) => preset.category === "aggregator"));
  for (const preset of PROVIDER_PRESETS) {
    assert.ok(preset.category === "official" || preset.category === "aggregator");
  }
  assert.equal(filterProviderPresets(PROVIDER_PRESETS, "  ").length, PROVIDER_PRESETS.length);
});

test("preset search matches family labels, variants, and endpoint hosts", () => {
  // Family label reaches every variant of the family.
  const tencentHits = filterProviderPresets(PROVIDER_PRESETS, "tencent");
  assert.ok(tencentHits.length > 1);
  assert.ok(tencentHits.some((preset) => preset.id === "tencent-hunyuan"));
  // Variant label reaches one exact preset.
  const variantHits = filterProviderPresets(PROVIDER_PRESETS, "enterprise lite");
  assert.ok(variantHits.some((preset) => preset.id === "tencent-enterprise-lite"));
  // Endpoint host reaches the preset whose endpoint uses it.
  const deepseek = PROVIDER_PRESETS.find((preset) => preset.id === "deepseek")!;
  const hostHits = filterProviderPresets(PROVIDER_PRESETS, new URL(deepseek.endpointUrl).host);
  assert.ok(hostHits.some((preset) => preset.id === "deepseek"));
});

test("defaultModels validate as a non-empty array of trimmed unique IDs", () => {
  assert.deepEqual(providerPresetShapeIssues(samplePreset({ defaultModels: ["a", "b"] })), []);
  assert.deepEqual(providerPresetDefaultModels(samplePreset()), []);
  assert.deepEqual(providerPresetDefaultModels(samplePreset({ defaultModels: ["a"] })), ["a"]);
  assert.ok(providerPresetShapeIssues(samplePreset({ defaultModels: [] })).length > 0);
  assert.ok(providerPresetShapeIssues(samplePreset({ defaultModels: [""] })).length > 0);
  assert.ok(providerPresetShapeIssues(samplePreset({ defaultModels: ["  "] })).length > 0);
  assert.ok(providerPresetShapeIssues(samplePreset({ defaultModels: [" a"] })).length > 0);
  assert.ok(providerPresetShapeIssues(samplePreset({ defaultModels: ["a "] })).length > 0);
  assert.ok(providerPresetShapeIssues(samplePreset({ defaultModels: ["a", "a"] })).length > 0);
  assert.ok(providerPresetShapeIssues(samplePreset({ defaultModels: [1] as never })).length > 0);
  assert.ok(providerPresetShapeIssues(samplePreset({ defaultModels: "a" as never })).length > 0);
  const parsed = parseProviderPresets([
    samplePreset({ id: "seeded", defaultModels: ["a"] }),
    samplePreset({ id: "bad-seeds", defaultModels: [] }),
  ]);
  assert.deepEqual(parsed.map((preset) => preset.id), ["seeded"]);
});

test("a seeded fixed preset replaces Key and old mappings with exact prefixed IDs", () => {
  const preset = samplePreset({ defaultModels: ["claude-opus-4-1", "claude-sonnet-4-5"] });
  const dirty: ProviderDefinitionDraft = {
    ...emptyProviderDefinitionDraft(),
    key: "sk-must-clear",
    models: [
      { public_model: "old", upstream_model: "old" },
      {
        public_model: "old-override",
        upstream_model: "old-override",
        upstream_override: { protocol: "responses", endpoint_url: "https://x.example.com/v1" },
      },
    ],
    account_name: "",
    notes: "保留备注",
  };
  const applied = applyProviderPresetToDraft(dirty, preset);
  assert.equal(applied.key, "");
  assert.deepEqual(applied.models, [
    { public_model: "anthropic/claude-opus-4-1", upstream_model: "claude-opus-4-1", upstream_override: null },
    { public_model: "anthropic/claude-sonnet-4-5", upstream_model: "claude-sonnet-4-5", upstream_override: null },
  ]);
  // An empty account name defaults to the preset name; typed notes survive.
  assert.equal(applied.account_name, "Anthropic API");
  assert.equal(applied.notes, "保留备注");
  // The live commit payload carries exact upstream IDs with null overrides.
  const payload = buildOnboardingCommitPayload({
    draft: { ...applied, key: "sk-new" },
    operationId: "11111111-1111-4111-8111-111111111111",
    mode: "complete",
  });
  assert.deepEqual(payload.targets, [
    { publicModel: "anthropic/claude-opus-4-1", upstreamModel: "claude-opus-4-1", upstreamOverride: null },
    { publicModel: "anthropic/claude-sonnet-4-5", upstreamModel: "claude-sonnet-4-5", upstreamOverride: null },
  ]);
  assert.equal(payload.authorization?.kind, "api_key");
  if (payload.authorization?.kind === "api_key") {
    assert.equal(payload.authorization.accountLabel, "Anthropic API");
  }
  assert.equal(payload.connection.kind, "new");
  if (payload.connection.kind === "new") {
    assert.equal(payload.connection.templateId, "anthropic");
  }
});

test("auto-generated account names follow the new preset; typed names stick", () => {
  const first = applyProviderPresetToDraft(emptyProviderDefinitionDraft(), samplePreset());
  assert.equal(first.account_name, "Anthropic API");
  // The auto-generated name is replaced on switch instead of carried over.
  const switched = applyProviderPresetToDraft(first, samplePreset({ id: "openai", name: "OpenAI API" }));
  assert.equal(switched.account_name, "OpenAI API");
  // A manual switch clears the auto name rather than naming a custom row after a preset.
  const manual = applyProviderPresetToDraft(switched, null);
  assert.equal(manual.account_name, "");
  // A typed name is never rewritten, and notes always survive.
  const typed = { ...first, account_name: "我的主号", notes: "n" };
  assert.equal(applyProviderPresetToDraft(typed, samplePreset({ id: "openai", name: "OpenAI API" })).account_name, "我的主号");
  assert.equal(applyProviderPresetToDraft(typed, null).account_name, "我的主号");
  assert.equal(applyProviderPresetToDraft(typed, null).notes, "n");
});

test("offering by persisted preset ID is metadata-only; unknown IDs are API", () => {
  const planPreset = samplePreset({ id: "coding-plan", offering: "plan" });
  const apiPreset = samplePreset({ id: "plain-api" });
  const presets = [planPreset, apiPreset];
  assert.equal(providerPresetOfferingForId("coding-plan", presets), "plan");
  assert.equal(providerPresetOfferingForId("plain-api", presets), "api");
  assert.equal(providerPresetOfferingForId("missing", presets), "api");
  assert.equal(providerPresetOfferingForId(null, presets), "api");
  assert.equal(providerPresetOfferingForId(undefined, presets), "api");
  assert.equal(providerPresetOfferingForId("", presets), "api");
});

test("switching reseeds models and a manual switch clears seeds", () => {
  const first = applyProviderPresetToDraft(
    emptyProviderDefinitionDraft(),
    samplePreset({ defaultModels: ["a"] }),
  );
  const switched = applyProviderPresetToDraft(
    first,
    samplePreset({ id: "openai", name: "OpenAI API", defaultModels: ["gpt-5"] }),
  );
  assert.deepEqual(switched.models, [
    { public_model: "openai/gpt-5", upstream_model: "gpt-5", upstream_override: null },
  ]);
  const manual = applyProviderPresetToDraft(switched, null);
  assert.deepEqual(manual.models, [{ public_model: "", upstream_model: "" }]);
});

test("an unseeded preset keeps the empty mapping row so model editing stays required", () => {
  const applied = applyProviderPresetToDraft(emptyProviderDefinitionDraft(), samplePreset());
  assert.deepEqual(applied.models, [{ public_model: "", upstream_model: "" }]);
  // Save validation still blocks a create without a complete mapping.
  assert.equal(
    validateProviderDefinitionDraft({ ...applied, key: "sk-x" }, { mode: "create" }),
    "missing_mappings",
  );
});

test("model discovery opt-out defaults to enabled and respects an explicit false", () => {
  assert.equal(providerPresetModelDiscoveryEnabled(samplePreset()), true);
  assert.equal(providerPresetModelDiscoveryEnabled(samplePreset({ modelDiscovery: true })), true);
  assert.equal(providerPresetModelDiscoveryEnabled(samplePreset({ modelDiscovery: false })), false);
});

test("offering comes from metadata only and defaults to api", () => {
  assert.equal(providerPresetOffering(samplePreset()), "api");
  assert.equal(providerPresetOffering(samplePreset({ offering: "plan" })), "plan");
  assert.equal(providerPresetOffering(samplePreset({ offering: "api" })), "api");
  // An explicit invalid value fails shape validation and the row is skipped.
  assert.ok(providerPresetShapeIssues(samplePreset({ offering: "bundle" as never })).length > 0);
  assert.deepEqual(providerPresetShapeIssues(samplePreset({ offering: "plan" })), []);
  const parsed = parseProviderPresets([
    samplePreset({ id: "bad-offering", offering: "bundle" as never }),
    samplePreset({ id: "good-offering", offering: "plan" }),
  ]);
  assert.deepEqual(parsed.map((preset) => preset.id), ["good-offering"]);
  const grouped = groupProviderPresetsByOffering([
    samplePreset({ id: "paid-plan", offering: "plan" }),
    samplePreset({ id: "plain-api" }),
  ]);
  assert.deepEqual(grouped.plan.map((preset) => preset.id), ["paid-plan"]);
  assert.deepEqual(grouped.api.map((preset) => preset.id), ["plain-api"]);
});

test("preset imports use a predictable preset-prefixed public name and keep the exact upstream ID", () => {
  assert.equal(providerPresetImportPublicName("anthropic", "claude-opus-4-1"), "anthropic/claude-opus-4-1");
  assert.equal(providerPresetImportPublicName(null, "claude-opus-4-1"), "claude-opus-4-1");
});

test("notes localize between Chinese and English", () => {
  const preset = samplePreset();
  assert.equal(providerPresetNote(preset, "zh-CN"), "中文备注");
  assert.equal(providerPresetNote(preset, "zh-TW"), "中文备注");
  assert.equal(providerPresetNote(preset, "en-US"), "English note");
  assert.equal(providerPresetNote(preset, "ja-JP"), "English note");
});

test("preset endpoints normalize to origin plus pathname for comparison only", () => {
  assert.equal(
    normalizeProviderPresetEndpoint("https://api.openai.com/v1/responses/"),
    "https://api.openai.com/v1/responses",
  );
  assert.equal(
    normalizeProviderPresetEndpoint("HTTPS://API.OPENAI.COM/v1/responses"),
    "https://api.openai.com/v1/responses",
  );
  // Credentials, query, and fragment reject the match instead of being dropped into it.
  assert.equal(normalizeProviderPresetEndpoint("https://api.openai.com/v1/responses?x=1#f"), null);
  assert.equal(normalizeProviderPresetEndpoint("https://user:pw@api.openai.com/v1/responses"), null);
  assert.equal(normalizeProviderPresetEndpoint("https://api.openai.com/v1/responses?api-version=1"), null);
  assert.equal(normalizeProviderPresetEndpoint(""), null);
  assert.equal(normalizeProviderPresetEndpoint("not a url"), null);
  assert.equal(normalizeProviderPresetEndpoint("ftp://api.openai.com/v1"), null);
});

test("preset resolution matches an exact normalized endpoint plus auth kind", () => {
  const anthropic = resolveProviderPreset("https://api.anthropic.com/v1/messages/", "x-api-key");
  assert.equal(anthropic?.id, "anthropic");
  // Auth kind is part of the identity: the same URL with Bearer matches nothing.
  assert.equal(resolveProviderPreset("https://api.anthropic.com/v1/messages", "bearer"), null);
  // No-auth providers never resolve to a keyed preset.
  assert.equal(resolveProviderPreset("https://api.anthropic.com/v1/messages", "none"), null);
  assert.equal(resolveProviderPreset("https://api.anthropic.com/v1/messages", ""), null);
});

test("preset resolution requires an exact endpoint match and never derives siblings", () => {
  // The OpenAI preset pins Responses; only its exact documented URL matches.
  assert.equal(resolveProviderPreset("https://api.openai.com/v1/responses", "bearer")?.id, "openai");
  // A sibling protocol path is not the preset endpoint and must not match.
  assert.equal(resolveProviderPreset("https://api.openai.com/v1/chat/completions", "bearer"), null);
  // A bare root or /v1 base carries no protocol evidence and must not match.
  assert.equal(resolveProviderPreset("https://api.openai.com", "bearer"), null);
  assert.equal(resolveProviderPreset("https://api.openai.com/v1", "bearer"), null);
});

test("ambiguous or unknown endpoints resolve to null so the user chooses manually", () => {
  const dupA = samplePreset({ id: "dup-a", endpointUrl: "https://shared.example.com/v1/chat/completions", authKind: "bearer" });
  const dupB = samplePreset({ id: "dup-b", endpointUrl: "https://shared.example.com/v1/chat/completions", authKind: "bearer" });
  assert.equal(resolveProviderPreset("https://shared.example.com/v1/chat/completions", "bearer", [dupA, dupB]), null);
  // Same endpoint but distinct auth kinds still resolve uniquely.
  const keyed = samplePreset({ id: "keyed", endpointUrl: "https://shared.example.com/v1/chat/completions", authKind: "x-api-key" });
  assert.equal(resolveProviderPreset("https://shared.example.com/v1/chat/completions", "x-api-key", [dupA, keyed])?.id, "keyed");
  // Blank-endpoint presets (Azure, Bedrock) can never be inferred from a URL.
  assert.equal(resolveProviderPreset("https://res.openai.azure.com/openai/v1/responses", "bearer"), null);
  // Arbitrary unknown hosts match nothing.
  assert.equal(resolveProviderPreset("https://unknown.example.com/v1/chat/completions", "bearer"), null);
});

test("edit-mode prefix evidence keeps import naming consistent only when unambiguous", () => {
  assert.equal(
    inferMappingPresetPrefix([
      { public_model: "anthropic/claude-opus-4-1" },
      { public_model: "anthropic/claude-sonnet-4-5" },
    ]),
    "anthropic",
  );
  // Mixed prefixes, unknown prefixes, and unprefixed rows are all manual.
  assert.equal(inferMappingPresetPrefix([
    { public_model: "anthropic/claude-opus-4-1" },
    { public_model: "openai/gpt-5" },
  ]), null);
  assert.equal(inferMappingPresetPrefix([{ public_model: "not-a-preset/model" }]), null);
  assert.equal(inferMappingPresetPrefix([{ public_model: "plain-model" }]), null);
  assert.equal(inferMappingPresetPrefix([{ public_model: "/leading-slash" }]), null);
  assert.equal(inferMappingPresetPrefix([]), null);
  assert.equal(inferMappingPresetPrefix([{ public_model: "  " }]), null);
  // Blank-endpoint presets are still valid naming evidence from existing rows.
  assert.equal(inferMappingPresetPrefix([{ public_model: "azure-openai/my-deployment" }]), "azure-openai");
});

test("an Azure draft roundtrip keeps the template ID while manual deployment mappings stay bare", () => {
  const azure = PROVIDER_PRESETS.find((preset) => preset.id === "azure-openai");
  assert.ok(azure);
  const draft = applyProviderPresetToDraft(emptyProviderDefinitionDraft(), azure!);
  // The user supplies the resource-specific URL and a bare deployment name.
  draft.endpoint_url = "https://my-resource.openai.azure.com/openai/v1/responses";
  draft.models = [{ public_model: "my-deployment", upstream_model: "my-deployment" }];
  draft.name = "Azure 主号";
  draft.key = "sk-azure";
  const createPayload = buildOnboardingCommitPayload({
    draft,
    operationId: "11111111-1111-4111-8111-111111111111",
    mode: "complete",
  });
  assert.equal(createPayload.connection.kind, "new");
  if (createPayload.connection.kind === "new") {
    assert.equal(createPayload.connection.templateId, "azure-openai");
  }
  assert.deepEqual(createPayload.targets, [
    { publicModel: "my-deployment", upstreamModel: "my-deployment", upstreamOverride: null },
  ]);
  // Reopen keeps the persisted template: discovery stays disabled and import
  // naming follows the template even though the custom URL matches no preset.
  const edit = resolveEditPreset(
    "azure-openai",
    draft.endpoint_url,
    "bearer",
    [{ public_model: "my-deployment" }],
  );
  assert.equal(edit.template?.id, "azure-openai");
  assert.equal(edit.endpointMatch, null);
  assert.equal(edit.importPresetId, "azure-openai");
  assert.equal(edit.discoveryPreset?.id, "azure-openai");
  assert.equal(providerPresetModelDiscoveryEnabled(edit.discoveryPreset!), false);
  // The roundtripped update body carries the ID through unchanged.
  const updateBody = buildProviderDefinitionUpdateBody({ ...draft, preset_id: "azure-openai" }, "bearer");
  assert.equal(updateBody.presetId, "azure-openai");
});

test("update provenance: omitted preserves, an explicit empty string clears", () => {
  const base = applyProviderPresetToDraft(emptyProviderDefinitionDraft(), samplePreset());
  base.key = "sk-edit";
  base.models = [{ public_model: "claude-opus-4-1", upstream_model: "claude-opus-4-1" }];
  const cleared = buildProviderDefinitionUpdateBody({ ...base, preset_id: "" }, "x-api-key");
  assert.equal(cleared.presetId, "");
  const preserved = buildProviderDefinitionUpdateBody({ ...base, preset_id: undefined }, "x-api-key");
  assert.ok(!("presetId" in preserved));
  const manualCreate = buildOnboardingCommitPayload({
    draft: { ...base, preset_id: "" },
    operationId: "11111111-1111-4111-8111-111111111111",
    mode: "complete",
  });
  assert.equal(manualCreate.connection.kind, "new");
  if (manualCreate.connection.kind === "new") {
    assert.equal(manualCreate.connection.templateId, "custom-http");
  }
});

test("legacy rows keep bare import names even when the endpoint matches an official preset", () => {
  const legacy = resolveEditPreset(
    null,
    "https://api.openai.com/v1/responses",
    "bearer",
    [{ public_model: "gpt-5" }],
  );
  // The live endpoint match still informs discovery, but never renames imports.
  assert.equal(legacy.template, null);
  assert.equal(legacy.endpointMatch?.id, "openai");
  assert.equal(legacy.importPresetId, null);
  assert.equal(legacy.discoveryPreset?.id, "openai");
  // Mapping prefix evidence still applies for legacy rows without a stored ID.
  const prefixed = resolveEditPreset(
    undefined,
    "https://custom-proxy.example.com/v1/chat/completions",
    "bearer",
    [{ public_model: "anthropic/claude-opus-4-1" }],
  );
  assert.equal(prefixed.importPresetId, "anthropic");
  assert.equal(prefixed.discoveryPreset?.id, "anthropic");
});
