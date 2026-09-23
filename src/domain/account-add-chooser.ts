import type { Connection } from "../api/connections.ts";
import type { ProviderCatalogEntry } from "../api/providers.ts";
import type { MessageKey } from "../i18n/index.ts";
import {
  splitPlanOptionsByOffering,
  type PlanOfferingSplit,
  type PlanOption,
} from "./account-plan-options.ts";
import { connectionForLegacyProvider } from "./connections.ts";
import {
  buildPlatformKindOptions,
  PLATFORM_KIND_OPTION_ID_PREFIX,
  type PlatformKindOption,
} from "./platform-accounts.ts";
import {
  PROVIDER_PRESETS,
  filterProviderPresets,
  groupProviderPresetsByOffering,
  providerPresetOffering,
  type ProviderPreset,
  type ProviderPresetOffering,
} from "./provider-presets.ts";
import { familyOf, groupPresetsByFamily, type ProviderFamily } from "./provider-families.ts";
import { sortProvidersByName } from "./provider-sort.ts";

/**
 * Presentation logic for the Add Account chooser. Pure helpers only; the
 * component keeps Vue state (selection, busy guards) and maps icon keys to
 * icon components. Group labels are literal product terms, not translated
 * message keys.
 */

/**
 * Preset choices are deliberate new instances: their ids carry a prefix so
 * they can never be silently matched to a saved user-defined Provider by
 * display name.
 */
export interface PresetChooserOption {
  optionId: string;
  preset: ProviderPreset;
  label: string;
  /** Family option this preset row flattens back to when the search clears. */
  familyOptionId: string;
}

/**
 * Family-grouped preset option. One per vendor family per Plan/API group;
 * single-preset families still surface as one option (no picker rendered).
 */
export interface PresetFamilyOption {
  optionId: string;
  family: ProviderFamily;
  presets: ProviderPreset[];
  label: string;
}

/** Query value and chooser id for manual user-defined HTTP, never Custom API. */
export const MANUAL_PRESET_QUERY_VALUE = "manual";
export const MANUAL_CHOOSER_OPTION_ID = "manual";

/** Rail/detail/search copy; the view renders `t(MANUAL_CHOOSER_LABEL_KEYS.manual)`. */
export const MANUAL_CHOOSER_LABEL_KEYS = {
  manual: "手动配置",
} as const satisfies Record<"manual", MessageKey>;

/**
 * Create a user-defined Configurable HTTP Provider without a vendor preset.
 * Distinct from the built-in Custom API template. `label` is caller-localized
 * so search and sort follow the active locale, never a hardcoded string.
 */
export interface ManualChooserOption {
  optionId: typeof MANUAL_CHOOSER_OPTION_ID;
  source: "manual";
  label: string;
}

export function manualChooserOption(label: string): ManualChooserOption {
  return { optionId: MANUAL_CHOOSER_OPTION_ID, source: "manual", label };
}

export type ChooserOption =
  | PlanOption
  | PresetFamilyOption
  | PresetChooserOption
  | PlatformKindOption
  | ManualChooserOption;

/** Internal offering buckets; rendered as one flat list. */
export interface ChooserGroup {
  id: "plan" | "api";
  label: "Plan" | "API";
  options: ChooserOption[];
}

/**
 * User-visible chooser mode. "connections" lists the same V4 connection
 * projection as Providers: built-in families that still have at least one
 * account, plus saved user-defined Providers (with or without a Key).
 * "services" browses new services: unused built-in templates (including
 * Custom API), vendor preset families, and platform kinds.
 */
export type ChooserMode = "connections" | "services";

/**
 * Built-in Custom API is always a new-connection template. Other built-ins
 * are existing connections only while the V4 projection still lists them.
 * Saved user-defined Providers stay on the connections side from the catalog
 * even before the projection arrives, matching Providers (definition rows
 * remain after the last Key is removed).
 */
function isExistingConnectionOption(
  option: PlanOption,
  connections: readonly Connection[] | null | undefined,
): boolean {
  if (option.source === "user-defined") return true;
  if (option.plan.kind === "custom") return false;
  if (!connections) return false;
  return Boolean(connectionForLegacyProvider(connections, option.plan.provider_id));
}

function partitionPlanOptions(
  catalog: readonly ProviderCatalogEntry[] | null | undefined,
  dynamicPresetIds: ReadonlyMap<string, string | null> | null | undefined,
  connections: readonly Connection[] | null | undefined,
): { existing: PlanOfferingSplit; unused: PlanOfferingSplit } {
  const split = splitPlanOptionsByOffering(catalog, dynamicPresetIds);
  const pick = (options: readonly PlanOption[], existing: boolean): PlanOption[] => (
    options.filter((option) => isExistingConnectionOption(option, connections) === existing)
  );
  return {
    existing: { plan: pick(split.plan, true), api: pick(split.api, true) },
    unused: { plan: pick(split.plan, false), api: pick(split.api, false) },
  };
}

function chooserGroupsFrom(
  plan: readonly ChooserOption[],
  api: readonly ChooserOption[],
): ChooserGroup[] {
  const groups: ChooserGroup[] = [];
  if (plan.length > 0) groups.push({ id: "plan", label: "Plan", options: [...plan] });
  if (api.length > 0) groups.push({ id: "api", label: "API", options: [...api] });
  return groups;
}

/**
 * Mode an option id belongs to. Preset and platform options are always new
 * services. Built-in plan ids follow the V4 projection: a family with at
 * least one account stays on existing connections; an unused template
 * (including Custom API) opens the new-service tab.
 */
export function chooserModeForOptionId(
  optionId: string,
  catalog?: readonly ProviderCatalogEntry[] | null,
  dynamicPresetIds?: ReadonlyMap<string, string | null> | null,
  connections?: readonly Connection[] | null,
): ChooserMode {
  if (
    optionId === MANUAL_CHOOSER_OPTION_ID
    || optionId === `preset:${MANUAL_PRESET_QUERY_VALUE}`
    || optionId.startsWith("family:")
    || optionId.startsWith("preset:")
    || optionId.startsWith(PLATFORM_KIND_OPTION_ID_PREFIX)
  ) {
    return "services";
  }
  const existing = chooserUniverse(catalog, dynamicPresetIds, "connections", connections);
  return existing.some((option) => option.optionId === optionId) ? "connections" : "services";
}

/** Default tab: existing connections when any remain, otherwise new services. */
export function defaultChooserMode(
  catalog: readonly ProviderCatalogEntry[] | null | undefined,
  dynamicPresetIds: ReadonlyMap<string, string | null> | null | undefined,
  connections: readonly Connection[] | null | undefined,
): ChooserMode {
  return chooserUniverse(catalog, dynamicPresetIds, "connections", connections).length > 0
    ? "connections"
    : "services";
}

/**
 * Family option ids carry the offering so the same vendor family can appear
 * once in the Plan group and once in the API group without an id collision.
 */
function presetFamilyOptionId(
  family: ProviderFamily,
  offering: ProviderPresetOffering,
): string {
  return `family:${offering}:${family.id}`;
}

function toPresetChooserOption(
  preset: ProviderPreset,
  offering: ProviderPresetOffering,
  family: ProviderFamily = familyOf(preset),
): PresetChooserOption {
  return {
    optionId: `preset:${preset.id}`,
    preset,
    familyOptionId: presetFamilyOptionId(family, offering),
    label: `${family.label} · ${preset.variant ?? preset.name}`,
  };
}

function toPresetFamilyOption(
  family: ProviderFamily,
  presets: ProviderPreset[],
  offering: ProviderPresetOffering,
): PresetFamilyOption {
  return {
    optionId: presetFamilyOptionId(family, offering),
    family,
    presets,
    label: family.label,
  };
}

/** Plan options whose rail/detail icon is the vendor brand mark, not a generic glyph. */
const PLAN_BRAND_FAMILY_ID: ReadonlyMap<string, string> = new Map([
  ["kimi", "moonshot"],
  ["minimax", "minimax"],
  ["ollama", "ollama"],
]);

function planBrandIconKey(planId: string): string | null {
  const familyId = PLAN_BRAND_FAMILY_ID.get(planId);
  return familyId ? `family:${familyId}` : null;
}

/**
 * Internal offering buckets for the given mode, flattened for display. "connections": the V4
 * connection projection only — built-in families that still have an account
 * head the Plan group, and saved user-defined Providers follow their catalog
 * offering. `dynamicPresetIds` is retained only for caller-side brand artwork.
 * "services": unused
 * built-in templates (Custom API included) plus vendor preset families
 * grouped by offering, with platform kinds trailing the API group. Empty
 * offering groups are omitted. The preset search query filters every visible
 * option in the active mode, including plans and platform kinds. When the
 * query is empty, presets in each offering group are collapsed into
 * per-family options so the rail shows vendors instead of every variant;
 * when the query is non-empty, presets flatten to variant-level rows so a
 * matching endpoint host is reachable in one click. Preset matching is the
 * single `filterProviderPresets` pass (family label, name, id, variant, and
 * endpoint host) — never a second filter that could drop family/host hits.
 */
export function buildChooserGroups(
  catalog: readonly ProviderCatalogEntry[] | null | undefined,
  dynamicPresetIds: ReadonlyMap<string, string | null> | null | undefined,
  query: string,
  mode: ChooserMode,
  connections?: readonly Connection[] | null,
  manualLabel = "",
): ChooserGroup[] {
  const { existing, unused } = partitionPlanOptions(catalog, dynamicPresetIds, connections);
  const normalized = query.trim().toLocaleLowerCase();
  const matches = (label: string) => label.toLocaleLowerCase().includes(normalized);
  const manual = manualChooserOption(manualLabel);

  if (mode === "connections") {
    return chooserGroupsFrom(
      existing.plan.filter((option) => matches(option.label)),
      existing.api.filter((option) => matches(option.label)),
    );
  }

  let planPresetOptions: ChooserOption[];
  let apiPresetOptions: ChooserOption[];
  if (normalized) {
    const offeringPresets = groupProviderPresetsByOffering(
      filterProviderPresets(PROVIDER_PRESETS, query),
    );
    planPresetOptions = offeringPresets.plan
      .map((preset) => toPresetChooserOption(preset, "plan"));
    apiPresetOptions = offeringPresets.api
      .map((preset) => toPresetChooserOption(preset, "api"));
  } else {
    const offeringPresets = groupProviderPresetsByOffering(PROVIDER_PRESETS);
    planPresetOptions = groupPresetsByFamily(offeringPresets.plan)
      .map((group) => toPresetFamilyOption(group.family, group.presets, "plan"));
    apiPresetOptions = groupPresetsByFamily(offeringPresets.api)
      .map((group) => toPresetFamilyOption(group.family, group.presets, "api"));
  }

  return chooserGroupsFrom(
    [
      ...unused.plan.filter((option) => matches(option.label)),
      ...planPresetOptions,
    ],
    [
      ...unused.api.filter((option) => matches(option.label)),
      ...(matches(manual.label) ? [manual] : []),
      ...apiPresetOptions,
      ...buildPlatformKindOptions().filter((option) => matches(option.label)),
    ],
  );
}

/**
 * The unfiltered option universe for one mode. Selection validity and the
 * default selection use this full list so typing in the preset search never
 * blanks the selected detail. The services universe holds the query-empty
 * shape (unused built-in templates plus family options), so picking a
 * flattened search row resolves back to its parent family and stays valid
 * when the query is cleared.
 */
export function chooserUniverse(
  catalog: readonly ProviderCatalogEntry[] | null | undefined,
  dynamicPresetIds: ReadonlyMap<string, string | null> | null | undefined,
  mode: ChooserMode,
  connections?: readonly Connection[] | null,
  manualLabel = "",
): ChooserOption[] {
  return visibleChooserOptions(
    buildChooserGroups(catalog, dynamicPresetIds, "", mode, connections, manualLabel),
  );
}

/** Visible (filtered) options in rail order; arrow-key navigation follows it. */
export function visibleChooserOptions(groups: readonly ChooserGroup[]): ChooserOption[] {
  const options = groups.flatMap((group) => group.options);
  const labels = new Map<string, number>();
  for (const option of options) labels.set(option.label, (labels.get(option.label) ?? 0) + 1);
  const labeled = groups.flatMap((group) => group.options.map((option) => (
    "family" in option && (labels.get(option.label) ?? 0) > 1
      ? { ...option, label: `${option.label} · ${group.label}` }
      : option
  )));
  const isCustom = (option: ChooserOption) => "plan" in option && option.plan.kind === "custom";
  return [
    ...labeled.filter(isCustom),
    ...sortProvidersByName(labeled.filter((option) => !isCustom(option)), (option) => option.label),
  ];
}

export function isChooserOptionDisabled(option: ChooserOption): boolean {
  return "disabled" in option && Boolean(option.disabled);
}

export function isValidChooserOption(
  options: readonly ChooserOption[],
  optionId: string,
): boolean {
  return options.some((option) => option.optionId === optionId);
}

export interface ChooserSelectionResolution {
  /** Rail option that owns the selection (family id for preset rows). */
  optionId: string;
  /** Exact preset variant, or "" when the selection is not a preset. */
  variantId: string;
}

/**
 * Resolve a picked option id — from the rail, arrow-key navigation, or the
 * phone selector — to the rail selection plus exact preset variant. The
 * visible (possibly search-flattened) options are consulted first so a
 * flattened preset row resolves to its parent family and variant instead of
 * being rejected by the family-shaped universe; the universe is the fallback
 * for family-shaped values the filter no longer shows.
 */
export function resolveChooserSelection(
  visible: readonly ChooserOption[],
  universe: readonly ChooserOption[],
  value: string,
): ChooserSelectionResolution | null {
  const target = visible.find((option) => option.optionId === value)
    ?? universe.find((option) => option.optionId === value);
  if (!target) return null;
  if ("preset" in target) {
    return { optionId: target.familyOptionId, variantId: target.preset.id };
  }
  // A family pick never forces a variant: the caller keeps the current one
  // while it still belongs to the family.
  return { optionId: target.optionId, variantId: "" };
}

export interface ChooserInitialOpen {
  mode: ChooserMode;
  optionId: string;
  variantId: string;
}

function optionIdForSavedConnection(connection: Connection): string {
  return connection.legacy.kind === "custom_account" ? "custom" : connection.legacy.id;
}

/**
 * Closed-to-open chooser selection. Exact preset variants map onto the family
 * rail row plus that variant; `manual` / `preset:manual` open manual
 * user-defined HTTP (not Custom API). Unknown ids do not fall back to another
 * vendor. A null target uses the ordinary default.
 */
export function resolveChooserInitialOpen(
  initialOptionId: string | null | undefined,
  catalog?: readonly ProviderCatalogEntry[] | null,
  dynamicPresetIds?: ReadonlyMap<string, string | null> | null,
  connections?: readonly Connection[] | null,
): ChooserInitialOpen {
  if (!initialOptionId) {
    const mode = defaultChooserMode(catalog, dynamicPresetIds, connections);
    return {
      mode,
      optionId: defaultChooserOptionId(chooserUniverse(catalog, dynamicPresetIds, mode, connections)),
      variantId: "",
    };
  }
  if (
    initialOptionId === MANUAL_CHOOSER_OPTION_ID
    || initialOptionId === `preset:${MANUAL_PRESET_QUERY_VALUE}`
  ) {
    return { mode: "services", optionId: MANUAL_CHOOSER_OPTION_ID, variantId: "" };
  }
  if (initialOptionId.startsWith("preset:")) {
    const presetId = initialOptionId.slice("preset:".length);
    const preset = PROVIDER_PRESETS.find((entry) => entry.id === presetId);
    if (!preset) {
      return { mode: "services", optionId: "", variantId: "" };
    }
    return {
      mode: "services",
      optionId: presetFamilyOptionId(familyOf(preset), providerPresetOffering(preset)),
      variantId: preset.id,
    };
  }
  let optionId = initialOptionId;
  if (initialOptionId.startsWith("connection:")) {
    const connectionId = initialOptionId.slice("connection:".length);
    const connection = (connections ?? []).find((row) => row.id === connectionId);
    if (!connection) {
      return { mode: "connections", optionId: "", variantId: "" };
    }
    optionId = optionIdForSavedConnection(connection);
  }
  const mode = chooserModeForOptionId(optionId, catalog, dynamicPresetIds, connections);
  const universe = chooserUniverse(catalog, dynamicPresetIds, mode, connections);
  if (!isValidChooserOption(universe, optionId)) {
    return { mode, optionId: "", variantId: "" };
  }
  return { mode, optionId, variantId: "" };
}

/** First selectable option, or the first option when every one is disabled. */
export function defaultChooserOptionId(options: readonly ChooserOption[]): string {
  return options.find((option) => !isChooserOptionDisabled(option))?.optionId
    ?? options[0]?.optionId
    ?? "";
}

export interface ChooserSelectChild {
  label: string;
  value: string;
  /** Naive UI option objects carry an open index signature. */
  [key: string]: unknown;
}

/** Narrow-screen fallback follows the same flat order as the desktop rail. */
export function chooserSelectOptions(
  groups: readonly ChooserGroup[],
  userDefinedLabel: string,
): ChooserSelectChild[] {
  return visibleChooserOptions(groups).map((option) => {
    let label = option.label;
    if ("source" in option && option.source === "user-defined") {
      label = `${label} · ${userDefinedLabel}`;
    } else if ("family" in option && option.presets.length > 1) {
      label = `${label} · ${option.presets.length}`;
    }
    return { label, value: option.optionId };
  });
}

/** Component-side icon map key; the component owns the actual components. */
export function chooserOptionIconKey(option: ChooserOption): string {
  if ("plan" in option) {
    return planBrandIconKey(option.plan.id) ?? option.plan.id;
  }
  if ("family" in option) return `family:${option.family.id}`;
  if ("preset" in option) return `family:${familyOf(option.preset).id}`;
  if (option.optionId === MANUAL_CHOOSER_OPTION_ID) return "api";
  return "database";
}

export type ChooserTagLabelCode = "provider_preset" | "user_defined" | "custom_endpoint";

export const CHOOSER_TAG_LABEL_KEYS: Record<ChooserTagLabelCode, MessageKey> = {
  provider_preset: "供应商预设",
  user_defined: "用户定义",
  custom_endpoint: "自定义端点",
};

export interface ChooserDetail {
  kind: "plan" | "family" | "preset" | "platform" | "manual";
  iconKey: string;
  title: string;
  tag: { label: ChooserTagLabelCode; type: "warning" | "default" } | null;
  links: { docsUrl: string; websiteUrl: string } | null;
}

/**
 * Unified detail-header description for whichever option is selected. For
 * family options the second arg is the variant currently picked in the
 * detail pane; defaulting to the family's first preset keeps call sites that
 * only have the family honest without forcing a second parameter.
 */
export function describeChooserSelection(
  option: ChooserOption,
  selectedPreset?: ProviderPreset,
): ChooserDetail {
  if ("family" in option) {
    const preset = selectedPreset ?? option.presets[0]!;
    return {
      kind: "family",
      iconKey: `family:${option.family.id}`,
      title: option.family.label,
      tag: { label: "provider_preset", type: "default" },
      links: { docsUrl: preset.docsUrl, websiteUrl: preset.websiteUrl },
    };
  }
  if ("preset" in option) {
    return {
      kind: "preset",
      iconKey: `family:${familyOf(option.preset).id}`,
      title: option.preset.name,
      tag: { label: "provider_preset", type: "default" },
      links: { docsUrl: option.preset.docsUrl, websiteUrl: option.preset.websiteUrl },
    };
  }
  if ("source" in option && option.source === "manual") {
    return {
      kind: "manual",
      iconKey: "api",
      title: option.label,
      tag: { label: "user_defined", type: "default" },
      links: null,
    };
  }
  if (!("plan" in option)) {
    return { kind: "platform", iconKey: "database", title: option.label, tag: null, links: null };
  }
  let tag: ChooserDetail["tag"] = null;
  if (option.source === "user-defined") tag = { label: "user_defined", type: "default" };
  else if (option.plan.kind === "custom") tag = { label: "custom_endpoint", type: "default" };
  return {
    kind: "plan",
    iconKey: planBrandIconKey(option.plan.id) ?? option.plan.id,
    title: option.label,
    tag,
    links: null,
  };
}
