import type {
  PlatformGroup,
  PlatformKind,
  PlatformLink,
  PlatformPrice,
  PlatformQuota,
  PlatformQuotaKind,
  PlatformSnapshot,
} from "../api/platform-accounts.ts";
import type { AccountProtocol } from "../api/dashboard.ts";

/**
 * Presentation logic for New API / Sub2API platform accounts. Pure helpers
 * only; i18n keys stay plain Chinese strings so this module never imports the
 * i18n runtime (same convention as custom-account.ts).
 */

export const PLATFORM_KIND_LABELS: Record<PlatformKind, string> = {
  new_api: "New API",
  sub2api: "Sub2API",
};

/**
 * Add-Account chooser entries for platform kinds. These are typed separately
 * from plan options on purpose: platform accounts are not plans, and no
 * backend PlanDefinition exists for them. Option ids carry a prefix so they
 * can never collide with plan option ids.
 */
export interface PlatformKindOption {
  optionId: string;
  kind: PlatformKind;
  label: string;
  disabled: boolean;
}

export const PLATFORM_KIND_OPTION_ID_PREFIX = "platform:";

/**
 * Parent-owned inference URL for a linked (or to-be-linked) Key, mirroring
 * the backend's `platform::inference_endpoint`: the site root with any `/v1`
 * suffix stripped, then `/v1/chat/completions`, `/v1/responses`, or
 * `/v1/messages` by protocol. Returns null for values that are not absolute
 * http(s) URLs or carry credentials; nothing is guessed from names.
 */
export function platformInferenceEndpoint(
  baseUrl: string,
  protocol: AccountProtocol,
): string | null {
  const trimmed = baseUrl.trim();
  if (!trimmed) return null;
  let parsed: URL;
  try {
    parsed = new URL(trimmed);
  } catch {
    return null;
  }
  if (parsed.protocol !== "http:" && parsed.protocol !== "https:") return null;
  if (parsed.username || parsed.password) return null;
  const suffix = protocol === "responses"
    ? "responses"
    : protocol === "messages"
      ? "messages"
      : "chat/completions";
  const root = trimmed.replace(/\/+$/u, "").replace(/\/v1$/iu, "");
  return `${root}/v1/${suffix}`;
}

export function buildPlatformKindOptions(): PlatformKindOption[] {
  return (Object.entries(PLATFORM_KIND_LABELS) as [PlatformKind, string][]).map(([kind, label]) => ({
    optionId: `${PLATFORM_KIND_OPTION_ID_PREFIX}${kind}`,
    kind,
    label,
    disabled: false,
  }));
}

export const PLATFORM_QUOTA_KIND_KEYS: Record<PlatformQuotaKind, string> = {
  wallet: "钱包",
  subscription: "订阅",
  key_limit: "Key 额度",
};

/** Keep wallet / subscription / Key limits as separate scopes (O03). */
export function quotasByKind(
  quotas: readonly PlatformQuota[],
): Record<PlatformQuotaKind, PlatformQuota[]> {
  return {
    wallet: quotas.filter((quota) => quota.kind === "wallet"),
    subscription: quotas.filter((quota) => quota.kind === "subscription"),
    key_limit: quotas.filter((quota) => quota.kind === "key_limit"),
  };
}

export function linkForAccount(
  links: readonly PlatformLink[],
  accountId: string,
): PlatformLink | null {
  return links.find((link) => link.accountId === accountId) ?? null;
}

export function linkedAccountIdSet(links: readonly PlatformLink[]): Set<string> {
  return new Set(links.map((link) => link.accountId));
}

/** "id · platform · subscriptionType"; empty when the link carries no explicit group. */
export function platformGroupLabel(
  group: Pick<PlatformGroup, "id" | "platform" | "subscriptionType">,
): string {
  return [group.id, group.platform, group.subscriptionType].filter((part) => part).join(" · ");
}

/**
 * Fixed unavailable-reason codes from the reader. Known codes get a localized
 * i18n key; anything else returns null so the UI shows the raw code.
 */
export const PLATFORM_UNAVAILABLE_REASON_KEYS: Record<string, string> = {
  user_identity_required: "需要登录身份才能查看价格",
  group_model_unavailable: "该分组不提供此模型",
  reasoning_multiplier: "按推理强度倍率计费",
};

export function platformUnavailableReasonKey(reason: string | null): string | null {
  return reason ? PLATFORM_UNAVAILABLE_REASON_KEYS[reason] ?? null : null;
}

export const MAX_PLATFORM_GROUP_ID_CHARS = 200;
export const MAX_PLATFORM_GROUP_PLATFORM_CHARS = 64;

/**
 * Explicit manual group entry. Trimmed; empty means unknown (null). Values are
 * never inferred from names, URLs, or masked Keys — only what the user typed.
 * Bounds mirror the backend write limits.
 */
export function platformManualGroup(
  id: string,
  platform: string,
): { id: string | null; platform: string | null } {
  const trimmedId = id.trim();
  const trimmedPlatform = platform.trim();
  if (Array.from(trimmedId).length > MAX_PLATFORM_GROUP_ID_CHARS) {
    throw new RangeError("platform group id exceeds 200 characters");
  }
  if (Array.from(trimmedPlatform).length > MAX_PLATFORM_GROUP_PLATFORM_CHARS) {
    throw new RangeError("platform group platform exceeds 64 characters");
  }
  return { id: trimmedId || null, platform: trimmedPlatform || null };
}

/** Quota amount with its unit; callers render 未知 when the value is null. */
export function formatQuotaAmount(value: number, unit: string, locale: string): string {
  const formatted = new Intl.NumberFormat(locale, { maximumFractionDigits: 2 }).format(value);
  return unit ? `${formatted} ${unit}` : formatted;
}

export function formatPlatformTime(epochSeconds: number, locale: string): string {
  if (!Number.isFinite(epochSeconds) || epochSeconds <= 0) return "";
  return new Intl.DateTimeFormat(locale, {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  }).format(new Date(epochSeconds * 1000));
}

function formatCurrency(value: number, currency: string, locale: string, digits: number): string {
  try {
    return new Intl.NumberFormat(locale, {
      style: "currency",
      currency,
      currencyDisplay: "narrowSymbol",
      maximumSignificantDigits: digits,
    }).format(value);
  } catch {
    // Non-ISO currency labels (points, credits, …) fall back to a plain suffix.
    return `${new Intl.NumberFormat(locale, { maximumSignificantDigits: digits }).format(value)} ${currency}`;
  }
}

export interface FormattedPlatformRate {
  /** Per-token rate; the wire unit is always currency per token. */
  label: string;
  /** Per-million-token reading for the tooltip, null for zero. */
  perMillion: string | null;
}

export function formatPlatformRate(
  value: number | null,
  currency: string,
  locale: string,
): FormattedPlatformRate | null {
  if (value === null || !Number.isFinite(value)) return null;
  return {
    label: `${formatCurrency(value, currency, locale, 4)}/token`,
    perMillion: value === 0 ? null : `${formatCurrency(value * 1e6, currency, locale, 6)} / 1M tokens`,
  };
}

export type PlatformPriceFlag = "official_reference" | "unavailable" | "expired" | "stale";

/**
 * Price trust flags: an official reference is not a quote, an unavailable
 * reason replaces numbers, expiry comes from `validUntil`, and a stale parent
 * snapshot marks every row in it.
 */
export function platformPriceFlags(
  price: PlatformPrice,
  snapshotStale: boolean,
  nowSeconds: number,
): PlatformPriceFlag[] {
  const flags: PlatformPriceFlag[] = [];
  if (price.officialReference) flags.push("official_reference");
  if (price.unavailableReason) flags.push("unavailable");
  if (price.validUntil > 0 && price.validUntil <= nowSeconds) flags.push("expired");
  if (snapshotStale) flags.push("stale");
  return flags;
}

/** Exact (model, group) price first, then the group-agnostic row; billed rows beat official references within each tier. */
export function platformPriceForModel(
  prices: readonly PlatformPrice[],
  modelId: string,
  groupId: string | null,
): PlatformPrice | null {
  const exact = prices.filter((price) => price.model === modelId && price.groupId === groupId);
  const fallback = prices.filter((price) => price.model === modelId && price.groupId === null);
  return exact.find((price) => !price.officialReference)
    ?? exact[0]
    ?? fallback.find((price) => !price.officialReference)
    ?? fallback[0]
    ?? null;
}

export type PlatformPriceDistinction = "billed" | "official";

export interface PlatformPriceRow {
  /** Unique per row; billed and official rows for the same model never share a key. */
  key: string;
  model: string;
  groupId: string | null;
  source: string;
  price: PlatformPrice | null;
  flags: PlatformPriceFlag[];
  /** Set when the same model+group has both a billed and an official-reference price. */
  distinction: PlatformPriceDistinction | null;
}

function priceGroupKey(model: string, groupId: string | null): string {
  return `${model}\n${groupId ?? ""}`;
}

/**
 * Display rows for the model/price table. Each storefront model gets one row
 * carrying its billed price; an official reference for the same model+group
 * becomes its own row so actual and reference prices stay visually distinct.
 * Prices without a storefront model row are appended as price-only rows.
 */
export function platformPriceRows(
  snapshot: PlatformSnapshot,
  nowSeconds: number,
): PlatformPriceRow[] {
  const billedByGroup = new Map<string, PlatformPrice[]>();
  const officialByGroup = new Map<string, PlatformPrice[]>();
  for (const price of snapshot.prices) {
    const map = price.officialReference ? officialByGroup : billedByGroup;
    const key = priceGroupKey(price.model, price.groupId);
    map.set(key, [...(map.get(key) ?? []), price]);
  }
  const lookup = (map: Map<string, PlatformPrice[]>, modelId: string, groupId: string | null) => (
    map.get(priceGroupKey(modelId, groupId)) ?? map.get(priceGroupKey(modelId, null))
  );
  const consumed = new Set<PlatformPrice>();
  const rows: PlatformPriceRow[] = [];
  const pushPriceRow = (price: PlatformPrice, distinction: PlatformPriceDistinction | null, index: number): void => {
    consumed.add(price);
    rows.push({
      key: `price:${price.officialReference ? "official" : "billed"}:${price.model}:${price.groupId ?? ""}:${index}`,
      model: price.model,
      groupId: price.groupId,
      source: price.source,
      price,
      flags: platformPriceFlags(price, snapshot.stale, nowSeconds),
      distinction,
    });
  };
  for (const model of snapshot.models) {
    const billed = lookup(billedByGroup, model.id, model.groupId);
    const official = lookup(officialByGroup, model.id, model.groupId);
    const both = Boolean(billed?.length && official?.length);
    const price = billed?.[0] ?? official?.[0] ?? null;
    if (price) consumed.add(price);
    rows.push({
      key: `model:${model.id}:${model.groupId ?? ""}`,
      model: model.id,
      groupId: model.groupId,
      source: model.source,
      price,
      flags: price ? platformPriceFlags(price, snapshot.stale, nowSeconds) : [],
      distinction: both && price ? (price.officialReference ? "official" : "billed") : null,
    });
    if (both && price === billed?.[0]) {
      official?.forEach((officialPrice, index) => pushPriceRow(officialPrice, "official", index));
    }
  }
  for (const price of snapshot.prices) {
    if (consumed.has(price)) continue;
    const both = Boolean(
      lookup(billedByGroup, price.model, price.groupId)?.length
      && lookup(officialByGroup, price.model, price.groupId)?.length,
    );
    pushPriceRow(price, both ? (price.officialReference ? "official" : "billed") : null, rows.length);
  }
  return rows;
}

export interface PlatformModelCandidate {
  id: string;
  platform: string | null;
  groupId: string | null;
  source: string;
  price: PlatformPrice | null;
  alreadyMapped: boolean;
}

/**
 * Storefront rows the user may explicitly import into a linked Key. Deduped by
 * model id; rows already covered by an existing mapping (either name, case-
 * insensitive) are flagged instead of hidden so the user sees the full list.
 * Candidacy is never proof of inference permission.
 */
export function platformModelCandidates(
  snapshot: PlatformSnapshot | null,
  existing: readonly { public_model: string; upstream_model: string }[],
): PlatformModelCandidate[] {
  if (!snapshot) return [];
  const taken = new Set(
    existing.flatMap((capability) => [
      capability.public_model.trim().toLocaleLowerCase(),
      capability.upstream_model.trim().toLocaleLowerCase(),
    ]),
  );
  const seen = new Set<string>();
  const candidates: PlatformModelCandidate[] = [];
  for (const model of snapshot.models) {
    const identity = model.id.trim().toLocaleLowerCase();
    if (!identity || seen.has(identity)) continue;
    seen.add(identity);
    candidates.push({
      id: model.id,
      platform: model.platform,
      groupId: model.groupId,
      source: model.source,
      price: platformPriceForModel(snapshot.prices, model.id, model.groupId),
      alreadyMapped: taken.has(identity),
    });
  }
  return candidates;
}

/** Selected candidates become identity mappings on the Key's single upstream protocol. */
export function importCandidateCapabilities(
  selected: readonly PlatformModelCandidate[],
  protocol: AccountProtocol,
): { public_model: string; upstream_model: string; protocol: AccountProtocol; source: string }[] {
  return selected.map((candidate) => ({
    public_model: candidate.id,
    upstream_model: candidate.id,
    protocol,
    source: "platform",
  }));
}
