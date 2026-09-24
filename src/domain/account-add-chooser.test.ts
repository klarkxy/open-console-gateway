import assert from "node:assert/strict";
import test from "node:test";
import type { Connection } from "../api/connections.ts";
import type { ProviderCatalogEntry } from "../api/providers.ts";
import {
  buildChooserGroups,
  CHOOSER_TAG_LABEL_KEYS,
  chooserModeForOptionId,
  chooserOptionIconKey,
  chooserSelectOptions,
  chooserUniverse,
  defaultChooserMode,
  defaultChooserOptionId,
  describeChooserSelection,
  isValidChooserOption,
  resolveChooserInitialOpen,
  resolveChooserSelection,
  visibleChooserOptions,
  type ChooserOption,
  type PresetFamilyOption,
  MANUAL_CHOOSER_LABEL_KEYS,
  MANUAL_CHOOSER_OPTION_ID,
} from "./account-add-chooser.ts";
import { buildPlatformKindOptions } from "./platform-accounts.ts";
import { familyOf } from "./provider-families.ts";
import { splitPlanOptionsByOffering } from "./account-plan-options.ts";
import { PROVIDER_PRESETS, providerPresetOffering } from "./provider-presets.ts";

function catalogEntry(
  provider_id: string,
  extra: Partial<ProviderCatalogEntry> = {},
): ProviderCatalogEntry {
  return {
    provider_id,
    origin: "builtin",
    editable: false,
    deletable: false,
    offering: "plan",
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

function fullCatalog(): ProviderCatalogEntry[] {
  return [
    catalogEntry("opencode", { display_name: "", routable: true }),
    catalogEntry("command-code", { display_name: "", routable: true }),
    catalogEntry("minimax", { display_name: "", routable: true }),
    catalogEntry("kimi", { display_name: "", routable: true }),
    catalogEntry("ollama", { display_name: "", routable: true }),
    catalogEntry("custom", { display_name: "", routable: true, offering: "api" }),
  ];
}

function presetById(id: string) {
  const preset = PROVIDER_PRESETS.find((entry) => entry.id === id);
  if (!preset) throw new Error(`fixture missing preset ${id}`);
  return preset;
}

function builtinConnection(providerId: string, extra: Partial<Connection> = {}): Connection {
  return {
    id: `conn-${providerId}`,
    name: providerId,
    origin: "builtin",
    template_ref: { id: providerId, version: 1 },
    adapter_kind: "sealed",
    lifecycle: "configured",
    authorization: "valid",
    eligibility: { state: "eligible", reason: "none" },
    credential_count: 1,
    enabled_credential_count: 1,
    target_count: 1,
    endpoints: [],
    targets: [],
    legacy: { kind: "builtin_provider", id: providerId },
    display_family: providerId,
    offering: "plan",
    ...extra,
  };
}

function planConnections(): Connection[] {
  return [
    builtinConnection("opencode"),
    builtinConnection("command-code"),
    builtinConnection("minimax"),
    builtinConnection("kimi"),
    builtinConnection("ollama"),
  ];
}

test("connections mode lists only built-ins that still have a V4 connection", () => {
  const groups = buildChooserGroups(fullCatalog(), null, "", "connections", planConnections());
  assert.deepEqual(groups.map((group) => group.id), ["plan"]);

  const planIds = groups[0]!.options.map((option) => option.optionId);
  assert.deepEqual(planIds, [
    "opencode",
    "command-code",
    "minimax",
    "kimi",
    "ollama",
  ]);
});

test("deleting the last built-in account moves that family to new services", () => {
  const remaining = [
    builtinConnection("command-code"),
    builtinConnection("minimax"),
    builtinConnection("kimi"),
  ];
  const connections = buildChooserGroups(fullCatalog(), null, "", "connections", remaining);
  assert.deepEqual(
    connections[0]!.options.map((option) => option.optionId),
    ["command-code", "minimax", "kimi"],
  );

  const services = buildChooserGroups(fullCatalog(), null, "", "services", remaining);
  const serviceIds = visibleChooserOptions(services).map((option) => option.optionId);
  assert.ok(serviceIds.includes("opencode"));
  assert.ok(serviceIds.includes("ollama"));
  assert.ok(serviceIds.includes("custom"));
  assert.equal(serviceIds.includes("command-code"), false);
  assert.equal(defaultChooserMode(fullCatalog(), null, remaining), "connections");
});

test("unused catalog and empty projection put every built-in template on new services", () => {
  assert.deepEqual(buildChooserGroups(fullCatalog(), null, "", "connections", []), []);
  assert.equal(defaultChooserMode(fullCatalog(), null, []), "services");
  assert.equal(defaultChooserMode(fullCatalog(), null, null), "services");

  const services = buildChooserGroups(fullCatalog(), null, "", "services", []);
  assert.deepEqual(
    services[0]!.options.slice(0, 5).map((option) => option.optionId),
    ["opencode", "command-code", "minimax", "kimi", "ollama"],
  );
  assert.equal(services[1]!.options[0]!.optionId, "custom");
});

test("services mode: unused built-ins head each group; presets and platforms follow", () => {
  const groups = buildChooserGroups(fullCatalog(), null, "", "services", planConnections());
  const planIds = groups[0]!.options.map((option) => option.optionId);
  const planFamilyIds = planIds.filter((id) => id.startsWith("family:plan:"));
  const planFamilyCount = new Set(
    PROVIDER_PRESETS
      .filter((preset) => providerPresetOffering(preset) === "plan")
      .map((preset) => preset.family ?? preset.id),
  ).size;
  assert.equal(planFamilyIds.length, planFamilyCount);
  assert.ok(planIds.every((id) => id.startsWith("family:plan:")));

  const apiIds = groups[1]!.options.map((option) => option.optionId);
  assert.equal(apiIds[0], "custom");
  assert.ok(apiIds.includes(MANUAL_CHOOSER_OPTION_ID));
  assert.deepEqual(apiIds.slice(-2), ["platform:new_api", "platform:sub2api"]);
  const familyApiIds = apiIds.filter((id) => id.startsWith("family:api:"));
  assert.equal(familyApiIds.length, new Set(familyApiIds).size);

  const visible = visibleChooserOptions(groups);
  assert.equal(visible[0]!.optionId, "custom");
  assert.deepEqual(new Set(visible.map((option) => option.optionId)), new Set(groups.flatMap((group) => group.options.map((option) => option.optionId))));
});

test("family ids carry the offering: the same vendor appears once per group without collision", () => {
  const groups = buildChooserGroups(fullCatalog(), null, "", "services", planConnections());
  const planFamilyOptions = groups[0]!.options
    .filter((option): option is PresetFamilyOption => "family" in option);
  const apiFamilyOptions = groups[1]!.options
    .filter((option): option is PresetFamilyOption => "family" in option);

  const tencentPlan = planFamilyOptions.find((option) => option.optionId === "family:plan:tencent");
  const tencentApi = apiFamilyOptions.find((option) => option.optionId === "family:api:tencent");
  assert.ok(tencentPlan, "Plan group must contain family:plan:tencent");
  assert.ok(tencentApi, "API group must contain family:api:tencent");
  assert.equal(tencentPlan!.presets.length, 6);
  assert.equal(tencentApi!.presets.length, 1);
  assert.equal(tencentApi!.presets[0]!.id, "tencent-hunyuan");

  // Universe ids are unique across both groups and both modes.
  for (const mode of ["connections", "services"] as const) {
    const ids = chooserUniverse(fullCatalog(), null, mode, planConnections()).map((option) => option.optionId);
    assert.equal(ids.length, new Set(ids).size, `${mode} universe must have unique option ids`);
  }
});

test("Zhipu appears once per offering group with 2 variants each", () => {
  const groups = buildChooserGroups(fullCatalog(), null, "", "services", planConnections());
  const zhipuPlan = (groups[0]!.options as ChooserOption[]).find(
    (option) => "family" in option && option.optionId === "family:plan:zhipu",
  ) as PresetFamilyOption;
  const zhipuApi = (groups[1]!.options as ChooserOption[]).find(
    (option) => "family" in option && option.optionId === "family:api:zhipu",
  ) as PresetFamilyOption;
  assert.equal(zhipuPlan.presets.length, 2);
  assert.equal(zhipuApi.presets.length, 2);
  assert.deepEqual(
    new Set(zhipuPlan.presets.map((preset) => preset.id)),
    new Set(["zhipu-coding", "zai-coding"]),
  );
  assert.deepEqual(
    new Set(zhipuApi.presets.map((preset) => preset.id)),
    new Set(["zai", "zhipu"]),
  );
});

test("search flattens presets to variant rows carrying the offering-scoped familyOptionId", () => {
  const groups = buildChooserGroups(fullCatalog(), null, "enterprise lite", "services", planConnections());
  const presetRows = groups.flatMap((group) => group.options).filter(
    (option): option is Extract<ChooserOption, { preset: unknown }> => "preset" in option,
  );
  const liteRows = presetRows.filter((row) => row.preset.id === "tencent-enterprise-lite");
  assert.equal(liteRows.length, 1);
  assert.equal(liteRows[0]!.optionId, "preset:tencent-enterprise-lite");
  assert.equal(liteRows[0]!.familyOptionId, "family:plan:tencent");
  const tencent = familyOf(presetById("tencent-enterprise-lite"));
  assert.equal(liteRows[0]!.label, `${tencent.label} · Enterprise Lite (CN)`);
});

test("search matches family labels, variants, and endpoint hosts in a single pass", () => {
  // Family label: every tencent variant of both offerings flattens out.
  const byFamily = buildChooserGroups(fullCatalog(), null, "tencent", "services", planConnections());
  const familyRows = byFamily.flatMap((group) => group.options).filter(
    (option): option is Extract<ChooserOption, { preset: unknown }> => "preset" in option,
  );
  const tencent = familyOf(presetById("tencent-hunyuan"));
  const tencentPresetIds = PROVIDER_PRESETS.filter(
    (preset) => familyOf(preset).id === tencent.id,
  ).map((preset) => preset.id).sort();
  assert.deepEqual(familyRows.map((row) => row.preset.id).sort(), tencentPresetIds);

  // Endpoint host: a host query reaches the matching preset directly.
  const deepseek = presetById("deepseek");
  const host = new URL(deepseek.endpointUrl).host;
  const byHost = buildChooserGroups(fullCatalog(), null, host, "services", planConnections());
  const hostRows = byHost.flatMap((group) => group.options).filter(
    (option): option is Extract<ChooserOption, { preset: unknown }> => "preset" in option,
  );
  assert.ok(hostRows.some((row) => row.preset.id === "deepseek"));

  // Connections options and platform kinds still filter by label.
  for (const [query, expected] of [[" Custom ", "custom"], ["new api", "platform:new_api"], ["Ollama", "ollama"]] as const) {
    const mode = chooserModeForOptionId(expected, fullCatalog(), null, planConnections());
    assert.deepEqual(
      visibleChooserOptions(buildChooserGroups(fullCatalog(), null, query, mode, planConnections())).map((item) => item.optionId),
      [expected],
    );
  }
  assert.equal(visibleChooserOptions(buildChooserGroups(fullCatalog(), null, "no-such-preset", "services", planConnections())).length, 0);
});

test("manual option search matches the caller-provided localized label, not another vendor", () => {
  const localized = "localized-manual-row";
  const hit = visibleChooserOptions(
    buildChooserGroups(fullCatalog(), null, "LOCALIZED-MANUAL", "services", planConnections(), localized),
  );
  assert.deepEqual(hit.map((option) => option.optionId), [MANUAL_CHOOSER_OPTION_ID]);
  assert.equal(hit[0]!.label, localized);
  const customOnly = visibleChooserOptions(
    buildChooserGroups(fullCatalog(), null, "custom", "services", planConnections(), localized),
  );
  assert.equal(customOnly.some((option) => option.optionId === MANUAL_CHOOSER_OPTION_ID), false);
  assert.ok(customOnly.some((option) => option.optionId === "custom"));
});

test("resolveChooserSelection maps flattened rows to family + variant and keeps the family valid after the query clears", () => {
  const queried = visibleChooserOptions(buildChooserGroups(fullCatalog(), null, "enterprise lite", "services", planConnections()));
  const universe = chooserUniverse(fullCatalog(), null, "services", planConnections());
  // The flattened row is not in the family-shaped universe; resolution must
  // consult the visible options first.
  assert.equal(isValidChooserOption(universe, "preset:tencent-enterprise-lite"), false);
  const resolved = resolveChooserSelection(queried, universe, "preset:tencent-enterprise-lite");
  assert.deepEqual(resolved, { optionId: "family:plan:tencent", variantId: "tencent-enterprise-lite" });
  // Clearing the query: the resolved family is a valid universe option, so
  // the selection (and its variant) survives.
  assert.equal(isValidChooserOption(universe, resolved!.optionId), true);
  const family = universe.find((option) => option.optionId === resolved!.optionId) as PresetFamilyOption;
  assert.ok(family.presets.some((preset) => preset.id === resolved!.variantId));

  // A family pick resolves to the family itself without forcing a variant.
  assert.deepEqual(
    resolveChooserSelection(queried, universe, "family:api:zhipu"),
    { optionId: "family:api:zhipu", variantId: "" },
  );
  // Unknown ids are rejected.
  assert.equal(resolveChooserSelection(queried, universe, "preset:nope"), null);
});

test("chooserModeForOptionId routes deep links to the right tab", () => {
  const catalog = [
    ...fullCatalog(),
    catalogEntry("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", {
      display_name: "Lab",
      model_source: "dynamic_provider",
      routable: true,
    }),
  ];
  assert.equal(chooserModeForOptionId("custom", catalog, null, planConnections()), "services");
  assert.equal(chooserModeForOptionId("opencode", catalog, null, planConnections()), "connections");
  assert.equal(chooserModeForOptionId("opencode", catalog, null, []), "services");
  assert.equal(chooserModeForOptionId("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", catalog, null, []), "connections");
  assert.equal(chooserModeForOptionId("family:plan:tencent"), "services");
  assert.equal(chooserModeForOptionId("preset:azure-openai"), "services");
  assert.equal(chooserModeForOptionId("preset:manual"), "services");
  assert.equal(chooserModeForOptionId("manual"), "services");
  assert.equal(chooserModeForOptionId("platform:new_api"), "services");
});

test("saved user-defined Providers follow their persisted preset offering in connections mode", () => {
  const planPreset = PROVIDER_PRESETS.find((preset) => providerPresetOffering(preset) === "plan")!;
  const catalog = [
    ...fullCatalog(),
    catalogEntry("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", {
      display_name: "Coding Plan",
      model_source: "dynamic_provider",
      routable: true,
    }),
    catalogEntry("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb", {
      display_name: "Manual API",
      offering: "api",
      model_source: "dynamic_provider",
      routable: true,
    }),
  ];
  const presetIds = new Map<string, string | null>([
    ["aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", planPreset.id],
  ]);
  const split = splitPlanOptionsByOffering(catalog, presetIds);
  const groups = buildChooserGroups(catalog, presetIds, "", "connections", planConnections());
  assert.deepEqual(
    groups[0]!.options.map((option) => option.optionId),
    split.plan.map((option) => option.optionId),
  );
  assert.deepEqual(
    groups[1]!.options.map((option) => option.optionId),
    split.api.filter((option) => option.source === "user-defined").map((option) => option.optionId),
  );
});

test("default selection uses the first option and falls back to empty", () => {
  assert.equal(defaultChooserOptionId(chooserUniverse(null, null, "connections", planConnections())), "opencode");
  assert.equal(defaultChooserOptionId(chooserUniverse(fullCatalog(), null, "connections", planConnections())), "command-code");
  assert.equal(defaultChooserOptionId(chooserUniverse(fullCatalog(), null, "services", planConnections())), "custom");
  assert.equal(defaultChooserOptionId(chooserUniverse(fullCatalog(), null, "services", [])), "custom");
  assert.equal(defaultChooserOptionId([]), "");
});

test("phone select options mirror the flat services rail and suffix family variant counts", () => {
  const groups = buildChooserGroups(fullCatalog(), null, "", "services", planConnections());
  const select = chooserSelectOptions(groups, "user-defined");
  assert.deepEqual(select.map((option) => option.value), visibleChooserOptions(groups).map((option) => option.optionId));
  assert.ok(select.every((option) => !("children" in option)));
  // Family options with > 1 variant get a count suffix; single-variant ones
  // stay clean.
  const tencentPlan = select
    .find((child) => child.value === "family:plan:tencent")!;
  assert.ok(tencentPlan);
  const longcat = select
    .find((child) => child.value === "family:api:longcat")!;
  assert.ok(longcat);
});

test("phone select marks user-defined entries in connections mode", () => {
  const catalog = [
    ...fullCatalog(),
    catalogEntry("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", {
      display_name: "Lab",
      model_source: "dynamic_provider",
      routable: true,
    }),
  ];
  const groups = buildChooserGroups(catalog, null, "", "connections");
  const select = chooserSelectOptions(groups, "user-defined");
  const lab = select
    .find((child) => child.value === "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa")!;
  assert.equal(lab.label, "Lab · user-defined");
});

test("describeChooserSelection covers plan, family, preset, and platform details", () => {
  const connections = chooserUniverse(fullCatalog(), null, "connections", planConnections());
  const services = chooserUniverse(fullCatalog(), null, "services", planConnections(), "localized-manual-row");
  const byId = (options: ChooserOption[], id: string): ChooserOption => (
    options.find((option) => option.optionId === id)!
  );

  const go = describeChooserSelection(byId(connections, "opencode"));
  assert.deepEqual(go, {
    kind: "plan",
    iconKey: "opencode",
    title: "opencode",
    tag: null,
    links: null,
  });

  const custom = describeChooserSelection(byId(services, "custom"));
  assert.equal(custom.kind, "plan");
  assert.deepEqual(custom.tag, { label: "custom_endpoint", type: "default" });

  const kimi = describeChooserSelection(byId(connections, "kimi"));
  assert.equal(kimi.iconKey, "family:moonshot");
  const minimax = describeChooserSelection(byId(connections, "minimax"));
  assert.equal(minimax.iconKey, "family:minimax");
  const ollama = describeChooserSelection(byId(connections, "ollama"));
  assert.equal(ollama.iconKey, "family:ollama");

  const familyTencent = describeChooserSelection(byId(services, "family:plan:tencent"));
  assert.equal(familyTencent.kind, "family");
  assert.equal(familyTencent.iconKey, "family:tencent");
  assert.equal(familyTencent.title, "Tencent");
  assert.deepEqual(familyTencent.tag, { label: "provider_preset", type: "default" });
  assert.equal(familyTencent.links!.docsUrl.startsWith("https://"), true);
  assert.equal(familyTencent.links!.websiteUrl.startsWith("https://"), true);
  // Passing a non-default variant swaps the links to that preset.
  const alt = describeChooserSelection(byId(services, "family:plan:tencent"), presetById("tencent-enterprise-lite-intl"));
  assert.equal(alt.links!.docsUrl, presetById("tencent-enterprise-lite-intl").docsUrl);

  const platform = describeChooserSelection(byId(services, "platform:new_api"));
  assert.deepEqual(platform, {
    kind: "platform",
    iconKey: "database",
    title: "New API",
    tag: null,
    links: null,
  });

  const manual = describeChooserSelection(byId(services, MANUAL_CHOOSER_OPTION_ID));
  assert.equal(manual.kind, "manual");
  assert.equal(manual.iconKey, "api");
  assert.equal(manual.title, "localized-manual-row");
  assert.deepEqual(manual.tag, { label: "user_defined", type: "default" });
  assert.equal(manual.links, null);
});

test("user-defined plan options carry the user_defined tag and family brand icon keys", () => {
  const catalog = [
    ...fullCatalog(),
    catalogEntry("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", {
      display_name: "Lab",
      model_source: "dynamic_provider",
      routable: true,
    }),
  ];
  const universe = chooserUniverse(catalog, null, "connections");
  const lab = universe.find((option) => option.optionId === "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa")!;
  const detail = describeChooserSelection(lab);
  assert.equal(detail.kind, "plan");
  assert.deepEqual(detail.tag, { label: "user_defined", type: "default" });
  assert.equal(detail.title, "Lab");

  const services = chooserUniverse(catalog, null, "services");
  const tencentFamily = services.find((option) => option.optionId === "family:plan:tencent")! as PresetFamilyOption;
  assert.equal(chooserOptionIconKey(tencentFamily), "family:tencent");
  assert.equal(chooserOptionIconKey(buildPlatformKindOptions()[0]!), "database");
  assert.equal(chooserOptionIconKey(lab), "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa");
});

test("every chooser tag label code has a message key", () => {
  const catalog = [
    ...fullCatalog(),
    catalogEntry("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", {
      display_name: "Lab",
      model_source: "dynamic_provider",
      routable: true,
    }),
  ];
  const universe = chooserUniverse(catalog, null, "connections");
  const tags = universe
    .map((option) => describeChooserSelection(option).tag)
    .filter((tag) => tag !== null);
  for (const tag of tags) assert.ok(CHOOSER_TAG_LABEL_KEYS[tag.label]);
  assert.deepEqual(
    Object.keys(CHOOSER_TAG_LABEL_KEYS).sort(),
    ["custom_endpoint", "provider_preset", "user_defined"],
  );
  assert.equal(MANUAL_CHOOSER_LABEL_KEYS.manual, "手动配置");
});

test("chooser initial open honors exact preset variants and manual HTTP, not another vendor", () => {
  const catalog = fullCatalog();
  const connections = planConnections();
  const openai = resolveChooserInitialOpen("preset:openai", catalog, null, connections);
  assert.deepEqual(openai, {
    mode: "services",
    optionId: "family:api:openai",
    variantId: "openai",
  });
  const lite = resolveChooserInitialOpen("preset:tencent-enterprise-lite", catalog, null, connections);
  assert.deepEqual(lite, {
    mode: "services",
    optionId: "family:plan:tencent",
    variantId: "tencent-enterprise-lite",
  });
  assert.deepEqual(
    resolveChooserInitialOpen("preset:manual", catalog, null, connections),
    { mode: "services", optionId: MANUAL_CHOOSER_OPTION_ID, variantId: "" },
  );
  assert.deepEqual(
    resolveChooserInitialOpen("manual", catalog, null, connections),
    { mode: "services", optionId: MANUAL_CHOOSER_OPTION_ID, variantId: "" },
  );
  const unknown = resolveChooserInitialOpen("preset:not-a-preset", catalog, null, connections);
  assert.equal(unknown.mode, "services");
  assert.equal(unknown.optionId, "");
  assert.equal(unknown.variantId, "");
  assert.notEqual(unknown.optionId, "custom");
  assert.equal(unknown.optionId.startsWith("family:"), false);

  const saved = resolveChooserInitialOpen("opencode", catalog, null, connections);
  assert.deepEqual(saved, { mode: "connections", optionId: "opencode", variantId: "" });
  const byConnection = resolveChooserInitialOpen("connection:conn-opencode", catalog, null, connections);
  assert.deepEqual(byConnection, { mode: "connections", optionId: "opencode", variantId: "" });
  assert.deepEqual(
    resolveChooserInitialOpen("custom", catalog, null, connections),
    { mode: "services", optionId: "custom", variantId: "" },
  );
  assert.equal(resolveChooserInitialOpen("custom", null, null, connections).optionId, "");

  const noTarget = resolveChooserInitialOpen(null, catalog, null, connections);
  assert.equal(noTarget.mode, "connections");
  assert.equal(noTarget.optionId, "command-code");
});
