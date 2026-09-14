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
  type ProviderPreset,
  type ProviderPresetOffering,
} from "./provider-presets.ts";
import { familyOf, groupPresetsByFamily, type ProviderFamily } from "./provider-families.ts";

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

export type ChooserOption = PlanOption | PresetFamilyOption | PresetChooserOption | PlatformKindOption;

/** Exactly two user-visible groups, Plan above API. */
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
    optionId.startsWith("family:")
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
 * Visible groups for the rail in the given mode. "connections": the V4
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
): ChooserGroup[] {
  const { existing, unused } = partitionPlanOptions(catalog, dynamicPresetIds, connections);
  const normalized = query.trim().toLocaleLowerCase();
  const matches = (label: string) => label.toLocaleLowerCase().includes(normalized);

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
): ChooserOption[] {
  return visibleChooserOptions(buildChooserGroups(catalog, dynamicPresetIds, "", mode, connections));
}

/** Visible (filtered) options in rail order; arrow-key navigation follows it. */
export function visibleChooserOptions(groups: readonly ChooserGroup[]): ChooserOption[] {
  return groups.flatMap((group) => group.options);
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

/** Structurally compatible with Naive UI SelectGroupOption. */
export interface ChooserSelectGroup {
  type: "group";
  key: string;
  label: string;
  children: ChooserSelectChild[];
  [key: string]: unknown;
}

/** Narrow-screen fallback: the same groups rendered as a grouped select. */
export function chooserSelectOptions(
  groups: readonly ChooserGroup[],
  userDefinedLabel: string,
): ChooserSelectGroup[] {
  return groups.map((group) => ({
    type: "group" as const,
    key: group.id,
    label: group.label,
    children: group.options.map((option) => {
      let label = option.label;
      if ("source" in option && option.source === "user-defined") {
        label = `${label} · ${userDefinedLabel}`;
      } else if ("family" in option && option.presets.length > 1) {
        label = `${label} · ${option.presets.length}`;
      }
      return { label, value: option.optionId };
    }),
  }));
}

/** Component-side icon map key; the component owns the actual components. */
export function chooserOptionIconKey(option: ChooserOption): string {
  if ("plan" in option) {
    return planBrandIconKey(option.plan.id) ?? option.plan.id;
  }
  if ("family" in option) return `family:${option.family.id}`;
  if ("preset" in option) return `family:${familyOf(option.preset).id}`;
  return "database";
}

export interface ChooserDetail {
  kind: "plan" | "family" | "preset" | "platform";
  iconKey: string;
  title: string;
  tag: { label: MessageKey; type: "warning" | "default" } | null;
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
      tag: { label: "供应商预设", type: "default" },
      links: { docsUrl: preset.docsUrl, websiteUrl: preset.websiteUrl },
    };
  }
  if ("preset" in option) {
    return {
      kind: "preset",
      iconKey: `family:${familyOf(option.preset).id}`,
      title: option.preset.name,
      tag: { label: "供应商预设", type: "default" },
      links: { docsUrl: option.preset.docsUrl, websiteUrl: option.preset.websiteUrl },
    };
  }
  if (!("plan" in option)) {
    return { kind: "platform", iconKey: "database", title: option.label, tag: null, links: null };
  }
  let tag: ChooserDetail["tag"] = null;
  if (option.source === "user-defined") tag = { label: "用户定义", type: "default" };
  else if (option.plan.kind === "custom") tag = { label: "自定义端点", type: "default" };
  return {
    kind: "plan",
    iconKey: planBrandIconKey(option.plan.id) ?? option.plan.id,
    title: option.label,
    tag,
    links: null,
  };
}
