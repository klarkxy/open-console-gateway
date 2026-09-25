// The preset data is loaded through a top-level dynamic import so Rollup emits
// it as its own chunk: every consumer of this module sits behind a lazy
// view/modal boundary, and a static JSON import would inline the data into the
// first chunk that pulls the domain graph. The attribute is required so the
// import also works under Node's type-stripping test runner.
const presetsJson = (await import("../../resources/provider-presets.json", {
  with: { type: "json" },
})).default as unknown;
import { familyOf } from "./provider-families.ts";
import {
  emptyProviderDefinitionDraft,
  type DynamicAuthKind,
  type ProviderDefinitionDraft,
  type ProviderDefinitionMapping,
  type DynamicUpstreamProtocol,
} from "./dynamic-provider.ts";
import type { AuthSchemeDto } from "../api/generated/dashboard-v4.ts";

export type ProviderPresetCategory = "official" | "aggregator";
export type ProviderPresetOffering = "plan" | "api";
export type ProviderPresetAuthKind = Extract<DynamicAuthKind, "bearer" | "x-api-key" | "api-key">;

export interface ProviderPreset {
  id: string;
  name: string;
  category: ProviderPresetCategory;
  /**
   * User-visible offering group. Absent defaults to "api" so old callers and
   * rows keep their behavior; the UI never infers this from names.
   */
  offering?: ProviderPresetOffering;
  /**
   * Vendor family id used to group presets in the chooser. Absent or unknown
   * ids fall back to a synthesized single-preset family so legacy rows still
   * render. Multi-preset families carry a `variant`; single-preset families
   * omit it.
   */
  family?: string;
  /**
   * Short label within a vendor family (e.g. "Token Plan (CN)").
   * Present only when `family` is set; a variant without a family is a shape
   * issue. Variants are unique within a family.
   */
  variant?: string;
  /**
   * Vetted default model IDs seeded verbatim on create: exact upstream IDs,
   * never auto-discovered and never typed from a raw /models listing.
   */
  defaultModels?: string[];
  /** Full inference URL, or "" when the endpoint is customer-specific. */
  endpointUrl: string;
  protocol: DynamicUpstreamProtocol;
  authKind: ProviderPresetAuthKind;
  docsUrl: string;
  websiteUrl: string;
  note: { en: string; zh: string };
  endpointPlaceholder?: string;
  /** False means no model-discovery interface is configured for this preset. */
  modelDiscovery?: boolean;
  /**
   * Official protocol endpoints declared on the preset. Absent means the
   * editor keeps the single default route. Never inferred from display names.
   */
  protocolRoutes?: ProviderPresetProtocolRoute[];
}

export interface ProviderPresetProtocolRoute {
  protocol: DynamicUpstreamProtocol;
  endpointUrl: string;
  authScheme: AuthSchemeDto;
}

/** Resolve only the two customer-specific hosts whose route paths are documented. */
export function providerPresetRoutesForEndpoint(
  preset: Pick<ProviderPreset, "id" | "endpointUrl" | "protocolRoutes">,
  endpoint: string,
): ProviderPresetProtocolRoute[] | undefined {
  if (preset.endpointUrl && preset.endpointUrl === endpoint) {
    return preset.protocolRoutes?.map((route) => ({ ...route }));
  }
  if (preset.endpointUrl || !endpoint) return undefined;
  let url: URL;
  try {
    url = new URL(endpoint);
  } catch {
    return undefined;
  }
  if (url.protocol !== "https:" || url.port || url.username || url.password || url.search || url.hash) return undefined;
  const host = url.hostname.toLowerCase();
  const path = url.pathname.replace(/\/$/, "");
  if (preset.id === "azure-openai"
    && /^[a-z0-9][a-z0-9-]*\.openai\.azure\.com$/.test(host)
    && path === "/openai/v1/responses") {
    const origin = url.origin;
    return [
      { protocol: "responses", endpointUrl: `${origin}/openai/v1/responses`, authScheme: "api_key" },
      { protocol: "chat_completions", endpointUrl: `${origin}/openai/v1/chat/completions`, authScheme: "api_key" },
    ];
  }
  if (preset.id === "bedrock"
    && /^bedrock-runtime\.[a-z0-9-]+\.amazonaws\.com$/.test(host)
    && path === "/openai/v1/responses") {
    const origin = url.origin;
    return [
      { protocol: "responses", endpointUrl: `${origin}/openai/v1/responses`, authScheme: "bearer" },
      { protocol: "chat_completions", endpointUrl: `${origin}/openai/v1/chat/completions`, authScheme: "bearer" },
      { protocol: "messages", endpointUrl: `${origin}/anthropic/v1/messages`, authScheme: "x_api_key" },
    ];
  }
  return undefined;
}

const PRESET_CATEGORIES: readonly ProviderPresetCategory[] = ["official", "aggregator"];
const PRESET_OFFERINGS: readonly ProviderPresetOffering[] = ["plan", "api"];
const PRESET_PROTOCOLS: readonly DynamicUpstreamProtocol[] = [
  "chat_completions",
  "responses",
  "messages",
];
const PRESET_AUTH_KINDS: readonly ProviderPresetAuthKind[] = ["bearer", "x-api-key", "api-key"];
const PRESET_ROUTE_AUTH: Record<string, AuthSchemeDto> = {
  bearer: "bearer",
  "x-api-key": "x_api_key",
  x_api_key: "x_api_key",
  "api-key": "api_key",
  api_key: "api_key",
  none: "none",
};

function isHttpUrl(value: unknown): value is string {
  if (typeof value !== "string" || !value) return false;
  try {
    const parsed = new URL(value);
    return parsed.protocol === "http:" || parsed.protocol === "https:";
  } catch {
    return false;
  }
}

/**
 * Lists every way a raw row violates the frozen preset contract; an empty
 * result means the row is valid. Used by tests and by the lenient parser.
 */
export function providerPresetShapeIssues(raw: unknown, index = 0): string[] {
  const issues: string[] = [];
  const where = `row ${index}`;
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) {
    return [`${where}: not an object`];
  }
  const row = raw as Record<string, unknown>;
  if (typeof row.id !== "string" || !row.id.trim()) issues.push(`${where}: id missing`);
  if (typeof row.name !== "string" || !row.name.trim()) issues.push(`${where}: name missing`);
  if (!PRESET_CATEGORIES.includes(row.category as ProviderPresetCategory)) {
    issues.push(`${where}: category must be official or aggregator`);
  }
  if (typeof row.endpointUrl !== "string") {
    issues.push(`${where}: endpointUrl must be a string (empty allowed)`);
  } else if (row.endpointUrl && !isHttpUrl(row.endpointUrl)) {
    issues.push(`${where}: endpointUrl is not an http(s) URL`);
  }
  if (!PRESET_PROTOCOLS.includes(row.protocol as DynamicUpstreamProtocol)) {
    issues.push(`${where}: protocol must be chat_completions, responses, or messages`);
  }
  if (!PRESET_AUTH_KINDS.includes(row.authKind as ProviderPresetAuthKind)) {
    issues.push(`${where}: authKind must be bearer, x-api-key, or api-key`);
  }
  if (!isHttpUrl(row.docsUrl)) issues.push(`${where}: docsUrl is not an http(s) URL`);
  if (!isHttpUrl(row.websiteUrl)) issues.push(`${where}: websiteUrl is not an http(s) URL`);
  const note = row.note as Record<string, unknown> | undefined;
  if (typeof note !== "object" || note === null
    || typeof note.en !== "string" || !note.en.trim()
    || typeof note.zh !== "string" || !note.zh.trim()) {
    issues.push(`${where}: note needs non-empty en and zh`);
  }
  if (row.offering !== undefined
    && !PRESET_OFFERINGS.includes(row.offering as ProviderPresetOffering)) {
    issues.push(`${where}: offering must be plan or api when present`);
  }
  if (row.family !== undefined
    && (typeof row.family !== "string" || !row.family.trim())) {
    issues.push(`${where}: family must be a non-empty string when present`);
  }
  if (row.variant !== undefined) {
    if (typeof row.variant !== "string" || !row.variant.trim()) {
      issues.push(`${where}: variant must be a non-empty string when present`);
    } else if (row.family === undefined) {
      issues.push(`${where}: variant requires family to be set`);
    }
  }
  if (row.endpointPlaceholder !== undefined
    && (typeof row.endpointPlaceholder !== "string" || !row.endpointPlaceholder.trim())) {
    issues.push(`${where}: endpointPlaceholder must be a non-empty string when present`);
  }
  if (row.modelDiscovery !== undefined && typeof row.modelDiscovery !== "boolean") {
    issues.push(`${where}: modelDiscovery must be a boolean when present`);
  }
  if (row.defaultModels !== undefined) {
    const models = row.defaultModels;
    const valid = Array.isArray(models)
      && models.length > 0
      && models.every((id) => typeof id === "string" && id.length > 0 && id.trim() === id)
      && new Set(models).size === models.length;
    if (!valid) {
      issues.push(`${where}: defaultModels must be a non-empty array of trimmed unique non-empty IDs`);
    }
  }
  if (row.protocolRoutes !== undefined) {
    issues.push(...providerPresetProtocolRouteIssues(row.protocolRoutes, where));
  }
  return issues;
}

function providerPresetProtocolRouteIssues(raw: unknown, where: string): string[] {
  if (!Array.isArray(raw) || raw.length === 0 || raw.length > 3) {
    return [`${where}: protocolRoutes must be 1–3 routes when present`];
  }
  const issues: string[] = [];
  const seen = new Set<string>();
  for (const [index, entry] of raw.entries()) {
    const routeWhere = `${where} protocolRoutes[${index}]`;
    if (typeof entry !== "object" || entry === null || Array.isArray(entry)) {
      issues.push(`${routeWhere}: not an object`);
      continue;
    }
    const route = entry as Record<string, unknown>;
    if (!PRESET_PROTOCOLS.includes(route.protocol as DynamicUpstreamProtocol)) {
      issues.push(`${routeWhere}: protocol must be chat_completions, responses, or messages`);
    } else if (seen.has(route.protocol as string)) {
      issues.push(`${routeWhere}: protocol is duplicated`);
    } else {
      seen.add(route.protocol as string);
    }
    if (typeof route.endpointUrl !== "string" || !isHttpUrl(route.endpointUrl)) {
      issues.push(`${routeWhere}: endpointUrl is not an http(s) URL`);
    }
    if (PRESET_ROUTE_AUTH[String(route.authScheme)] === undefined) {
      issues.push(`${routeWhere}: authScheme must be bearer, x-api-key, api-key, or none`);
    }
  }
  return issues;
}

function presentPresetProtocolRoutes(raw: unknown): ProviderPresetProtocolRoute[] | undefined {
  if (!Array.isArray(raw) || providerPresetProtocolRouteIssues(raw, "row").length > 0) return undefined;
  return raw.map((entry) => {
    const route = entry as Record<string, unknown>;
    return {
      protocol: route.protocol as DynamicUpstreamProtocol,
      endpointUrl: route.endpointUrl as string,
      authScheme: PRESET_ROUTE_AUTH[String(route.authScheme)]!,
    };
  });
}

/** Keeps only contract-valid rows so a bad entry cannot break the dashboard. */
export function parseProviderPresets(raw: unknown): ProviderPreset[] {
  if (!Array.isArray(raw)) return [];
  const seen = new Set<string>();
  const presets: ProviderPreset[] = [];
  for (const [index, row] of raw.entries()) {
    if (providerPresetShapeIssues(row, index).length > 0) continue;
    const record = row as Record<string, unknown>;
    const preset = {
      ...(row as ProviderPreset),
      protocolRoutes: presentPresetProtocolRoutes(record.protocolRoutes),
    };
    if (seen.has(preset.id)) continue;
    seen.add(preset.id);
    presets.push(preset);
  }
  return presets;
}

export const PROVIDER_PRESETS: readonly ProviderPreset[] = Object.freeze(
  parseProviderPresets(presetsJson),
);

/**
 * User-visible offering group from metadata only. Rows without an explicit
 * offering are general API offerings; nothing is inferred from names.
 */
export function providerPresetOffering(
  preset: Pick<ProviderPreset, "offering">,
): ProviderPresetOffering {
  return preset.offering === "plan" ? "plan" : "api";
}

export function groupProviderPresetsByOffering(
  presets: readonly ProviderPreset[],
): { plan: ProviderPreset[]; api: ProviderPreset[] } {
  return {
    plan: presets.filter((preset) => providerPresetOffering(preset) === "plan"),
    api: presets.filter((preset) => providerPresetOffering(preset) === "api"),
  };
}

/** Vetted seed IDs for a fixed-preset create; [] means the user edits models. */
export function providerPresetDefaultModels(
  preset: Pick<ProviderPreset, "defaultModels">,
): string[] {
  return preset.defaultModels ? [...preset.defaultModels] : [];
}

/** Preset lookup by persisted ID; unknown or absent IDs return null. */
export function providerPresetById(
  presetId: string | null | undefined,
  presets: readonly ProviderPreset[] = PROVIDER_PRESETS,
): ProviderPreset | null {
  return presetId ? presets.find((entry) => entry.id === presetId) ?? null : null;
}

/**
 * Offering of a saved provider's persisted preset ID. Unknown or absent IDs
 * are API; nothing is inferred from display names.
 */
export function providerPresetOfferingForId(
  presetId: string | null | undefined,
  presets: readonly ProviderPreset[] = PROVIDER_PRESETS,
): ProviderPresetOffering {
  const preset = providerPresetById(presetId, presets);
  return preset ? providerPresetOffering(preset) : "api";
}

/**
 * Case-insensitive substring match over everything the chooser and the
 * Providers rail show for a preset: its name and id, the vendor family label,
 * the variant label, and the endpoint host. This is the single predicate —
 * callers must not chain a second filter on top, or family/host matches
 * would be filtered away.
 */
export function filterProviderPresets(
  presets: readonly ProviderPreset[],
  query: string,
): ProviderPreset[] {
  const needle = query.trim().toLocaleLowerCase();
  if (!needle) return [...presets];
  return presets.filter((preset) => {
    if (preset.name.toLocaleLowerCase().includes(needle)) return true;
    if (preset.id.toLocaleLowerCase().includes(needle)) return true;
    if (preset.variant?.toLocaleLowerCase().includes(needle)) return true;
    if (familyOf(preset).label.toLocaleLowerCase().includes(needle)) return true;
    const raw = preset.endpointUrl || preset.endpointPlaceholder || "";
    if (!raw) return false;
    let host = raw;
    try {
      host = new URL(raw).host;
    } catch {
      // Placeholder hosts that are not full URLs match as typed.
    }
    return host.toLocaleLowerCase().includes(needle);
  });
}

/**
 * Builds the draft for a preset selection. Preset-filled fields (name,
 * endpoint, protocol, auth) always reset; the Key and model mappings are
 * cleared on every switch so a secret can never cross providers. Vetted
 * defaultModels then seed exact upstream IDs with preset-prefixed public
 * names and no per-model override. Typed account names and notes survive a
 * switch; an auto-generated or empty account name follows the new preset so
 * the first account is never created nameless.
 */
export function applyProviderPresetToDraft(
  current: ProviderDefinitionDraft,
  preset: ProviderPreset | null,
): ProviderDefinitionDraft {
  const base = emptyProviderDefinitionDraft();
  const seeds = preset ? providerPresetDefaultModels(preset) : [];
  // An account name equal to the previous preset's name was auto-generated by
  // this helper, not typed; it follows the new preset instead of sticking.
  const previousPreset = current.preset_id
    ? PROVIDER_PRESETS.find((entry) => entry.id === current.preset_id) ?? null
    : null;
  const accountNameIsAuto = Boolean(previousPreset && current.account_name === previousPreset.name);
  const typedAccountName = current.account_name && !accountNameIsAuto ? current.account_name : "";
  return {
    ...base,
    name: preset ? preset.name : "",
    endpoint_url: preset ? preset.endpointUrl : "",
    upstream_protocol: preset ? preset.protocol : base.upstream_protocol,
    auth_kind: preset ? preset.authKind : base.auth_kind,
    // Persisted provenance follows the explicit picker choice: the exact
    // preset ID, or "" for a manual switch (onboarding commit maps "" to
    // templateId "custom-http"; update with empty presetId still clears).
    preset_id: preset ? preset.id : "",
    models: seeds.length > 0
      ? seeds.map((id) => ({
        public_model: providerPresetImportPublicName(preset!.id, id),
        upstream_model: id,
        upstream_override: null,
      }))
      : base.models,
    account_name: typedAccountName || (preset ? preset.name : ""),
    notes: current.notes,
  };
}

/** A placeholder is display-only; it is never a usable endpoint value. */
export function providerPresetEndpointPlaceholder(preset: ProviderPreset): string {
  return preset.endpointPlaceholder ?? "";
}

export function providerPresetModelDiscoveryEnabled(preset: ProviderPreset): boolean {
  return preset.modelDiscovery !== false;
}

/**
 * Preset imports get a predictable `<preset-id>/<exact-id>` public name so
 * rows stay distinguishable on the aggregated Aliases view; manual imports
 * keep the plain upstream ID. The upstream ID is always the exact model ID.
 */
export function providerPresetImportPublicName(
  presetId: string | null,
  upstreamModelId: string,
): string {
  return presetId ? `${presetId}/${upstreamModelId}` : upstreamModelId;
}

export function providerPresetNote(preset: ProviderPreset, locale: string): string {
  return locale.toLocaleLowerCase().startsWith("zh") ? preset.note.zh : preset.note.en;
}

/**
 * Comparison identity for preset matching: origin plus the pathname without
 * trailing slashes. URLs carrying credentials, a query, or a fragment are
 * rejected outright instead of being silently normalized into an official
 * match; returns null for values that are not absolute http(s) URLs.
 */
export function normalizeProviderPresetEndpoint(value: string): string | null {
  const trimmed = value.trim();
  if (!trimmed) return null;
  let parsed: URL;
  try {
    parsed = new URL(trimmed);
  } catch {
    return null;
  }
  if (parsed.protocol !== "http:" && parsed.protocol !== "https:") return null;
  if (!parsed.hostname) return null;
  if (parsed.username || parsed.password || parsed.search || parsed.hash) return null;
  return `${parsed.origin}${parsed.pathname.replace(/\/+$/u, "")}`;
}

/**
 * Deterministic preset resolution for an existing draft: the normalized
 * endpoint must match exactly one preset's own endpoint with the same auth
 * kind. Sibling protocol paths are never derived or inferred, so a match
 * means the exact documented endpoint. Presets with a blank endpoint (Azure,
 * Bedrock) can never be inferred from a URL, and any ambiguity resolves to
 * null — the user picks manually instead of the UI guessing.
 */
export function resolveProviderPreset(
  endpointUrl: string,
  authKind: DynamicAuthKind | "",
  presets: readonly ProviderPreset[] = PROVIDER_PRESETS,
): ProviderPreset | null {
  const target = normalizeProviderPresetEndpoint(endpointUrl);
  if (!target) return null;
  if (authKind !== "bearer" && authKind !== "x-api-key" && authKind !== "api-key") return null;
  const matches = presets.filter((preset) => (
    Boolean(preset.endpointUrl)
    && preset.authKind === authKind
    && normalizeProviderPresetEndpoint(preset.endpointUrl) === target
  ));
  return matches.length === 1 ? matches[0] ?? null : null;
}

/**
 * Naming-style evidence from existing mappings: when every non-empty public
 * name shares the same `<preset-id>/` prefix and that prefix is a known
 * preset, imports keep that prefix. Mixed, unknown, or absent prefixes return
 * null; a preset is never inferred from the provider's display name.
 */
export function inferMappingPresetPrefix(
  models: readonly Pick<ProviderDefinitionMapping, "public_model">[],
  presets: readonly ProviderPreset[] = PROVIDER_PRESETS,
): string | null {
  const knownIds = new Set(presets.map((preset) => preset.id));
  let found: string | null = null;
  let seen = 0;
  for (const model of models) {
    const name = model.public_model.trim();
    if (!name) continue;
    seen += 1;
    const slash = name.indexOf("/");
    if (slash <= 0) return null;
    const prefix = name.slice(0, slash);
    if (!knownIds.has(prefix)) return null;
    if (found === null) found = prefix;
    else if (found !== prefix) return null;
  }
  return seen > 0 ? found : null;
}

export interface EditPresetResolution {
  /** Persisted source-template preset, when the stored ID is a known preset. */
  template: ProviderPreset | null;
  /** Unique live endpoint+auth match; verified endpoint evidence only. */
  endpointMatch: ProviderPreset | null;
  /**
   * Naming preset for imports: the persisted template first, then a shared
   * mapping prefix. An endpoint-only match never prefixes previously
   * unprefixed manual mappings.
   */
  importPresetId: string | null;
  /** Preset gating model discovery: template first, then legacy inference. */
  discoveryPreset: ProviderPreset | null;
}

/**
 * Edit-mode preset context. Persisted provenance wins; legacy rows without a
 * stored ID fall back to the safe endpoint/prefix inference. Template metadata
 * and the verified endpoint claim stay separate so a modified custom URL is
 * never presented as the official endpoint.
 */
export function resolveEditPreset(
  persistedPresetId: string | null | undefined,
  endpointUrl: string,
  authKind: DynamicAuthKind | "",
  models: readonly Pick<ProviderDefinitionMapping, "public_model">[],
  presets: readonly ProviderPreset[] = PROVIDER_PRESETS,
): EditPresetResolution {
  const template = persistedPresetId
    ? presets.find((preset) => preset.id === persistedPresetId) ?? null
    : null;
  const endpointMatch = resolveProviderPreset(endpointUrl, authKind, presets);
  const prefix = inferMappingPresetPrefix(models, presets);
  const prefixPreset = prefix
    ? presets.find((preset) => preset.id === prefix) ?? null
    : null;
  return {
    template,
    endpointMatch,
    importPresetId: template?.id ?? prefix,
    discoveryPreset: template ?? endpointMatch ?? prefixPreset,
  };
}
