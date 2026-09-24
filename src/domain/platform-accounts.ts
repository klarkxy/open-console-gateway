import type {
  PlatformAccount,
  PlatformGroup,
  PlatformKind,
  PlatformLink,
  PlatformPrice,
  PlatformQuota,
  PlatformQuotaKind,
  PlatformSnapshot,
} from "../api/platform-accounts.ts";
import type { Account, AccountModelCapabilityInput, AccountProtocol } from "../api/dashboard.ts";
import type { MessageKey } from "../i18n/index.ts";

/**
 * Presentation logic for New API / Sub2API platform accounts. Pure helpers
 * only; i18n keys stay plain Chinese strings. Type-only `MessageKey` is
 * allowed; this module never imports the i18n runtime.
 */

export const PLATFORM_KIND_LABELS: Record<PlatformKind, string> = {
  new_api: "New API",
  sub2api: "Sub2API",
};

/**
 * Snapshot error codes written by the platform reader. These are refresh-time
 * notices, never a resident card banner.
 */
export const PLATFORM_SNAPSHOT_ERROR_KEYS = {
  "auth.missing": "未保存管理凭证",
  "base_url.invalid": "平台地址无效",
  unauthorized: "管理凭证无效",
  user_id_required: "缺少用户 ID",
  user_id_mismatch: "用户 ID 不匹配",
  forbidden: "平台拒绝访问",
  network: "无法连接平台",
  timeout: "平台请求超时",
  redirect_rejected: "平台地址发生重定向",
  http_status: "平台返回异常状态",
  parse: "平台响应无法解析",
  oversize: "平台响应过大",
  endpoint_override: "平台改写了请求地址",
  secret_reflected: "平台响应含有凭证",
} as const satisfies Record<string, MessageKey>;

export function platformSnapshotErrorKey(code: string): MessageKey {
  return PLATFORM_SNAPSHOT_ERROR_KEYS[code as keyof typeof PLATFORM_SNAPSHOT_ERROR_KEYS]
    ?? "刷新未完成";
}

/**
 * New API management credential as typed in the form. Stored as the existing
 * opaque `userCredential` string (`userId:token`) so V3 stays unchanged.
 */
export type NewApiCredentialIssue =
  | "user_id_not_digits"
  | "user_id_without_token"
  | "token_without_user_id";

export const NEW_API_CREDENTIAL_ISSUE_KEYS = {
  user_id_not_digits: "用户 ID 须为数字",
  user_id_without_token: "填写用户 ID 时请同时填写系统访问令牌",
  token_without_user_id: "填写令牌时请同时填写用户 ID",
} as const satisfies Record<NewApiCredentialIssue, string>;

export function newApiCredentialIssue(
  userId: string,
  token: string,
): NewApiCredentialIssue | null {
  const id = userId.trim();
  const secret = token.trim();
  if (!id && !secret) return null;
  if (id && !/^\d+$/u.test(id)) return "user_id_not_digits";
  if (id && !secret) return "user_id_without_token";
  if (!id && secret) return "token_without_user_id";
  return null;
}

export function canImportPlatformKeys(
  parent: Pick<PlatformAccount, "kind" | "hasUserCredential">,
): boolean {
  return parent.kind === "new_api" && parent.hasUserCredential;
}

export type PlatformKeyImportFailureCode =
  | "full_key_unavailable"
  | "no_models"
  | "discover"
  | "create"
  | "link"
  | "existing";

export const PLATFORM_KEY_IMPORT_FAILURE_KEYS = {
  full_key_unavailable: "站点不允许读取完整 Key",
  no_models: "未返回可用模型",
  discover: "拉取模型失败",
  create: "创建失败",
  link: "关联失败",
  existing: "本地已有相同 Key",
} as const satisfies Record<PlatformKeyImportFailureCode, string>;

/** `undefined` means omit/preserve; never returns an empty string. */
export function composeNewApiUserCredential(
  userId: string,
  token: string,
): string | undefined {
  if (newApiCredentialIssue(userId, token) !== null) return undefined;
  const id = userId.trim();
  const secret = token.trim();
  if (!id || !secret) return undefined;
  return `${id}:${secret}`;
}

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
 * Parent-owned site root for a linked Key, mirroring the backend's
 * `platform::hosted_endpoint`. New API / Sub2API convert Chat, Messages, and
 * Responses themselves, so OCG stores this root and selects the protocol path
 * at request time. Returns null for values that are not absolute http(s) URLs
 * or carry credentials; nothing is guessed from names.
 */
export function platformHostedEndpoint(baseUrl: string): string | null {
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
  return trimmed.replace(/\/+$/u, "").replace(/\/v1$/iu, "");
}

/**
 * Complete inference URL for a protocol under a hosted root. Kept for
 * diagnostics and tests; linked Keys no longer persist this path.
 */
export function platformInferenceEndpoint(
  baseUrl: string,
  protocol: AccountProtocol,
): string | null {
  const root = platformHostedEndpoint(baseUrl);
  if (!root) return null;
  const suffix = protocol === "responses"
    ? "responses"
    : protocol === "messages"
      ? "messages"
      : "chat/completions";
  return `${root}/v1/${suffix}`;
}

export function discoveredModelCapabilities(
  modelIds: readonly string[],
  protocol: AccountProtocol = "chat_completions",
): AccountModelCapabilityInput[] {
  return modelIds.map((id) => ({
    public_model: id,
    upstream_model: id,
    protocol,
    source: "discovery",
  }));
}

export interface PlatformModelShare {
  id: string;
  accountIds: string[];
}

export interface PlatformKeyModelSummary {
  accountId: string;
  total: number;
  shared: number;
  exclusive: number;
}

export interface PlatformModelOverlay {
  uniqueIds: string[];
  sharedIds: string[];
  shares: PlatformModelShare[];
  keys: PlatformKeyModelSummary[];
}

function capabilityPublicIds(
  account: Pick<Account, "model_capabilities">,
): string[] {
  const ids: string[] = [];
  const seen = new Set<string>();
  for (const capability of account.model_capabilities) {
    const id = capability.public_model.trim();
    const key = id.toLocaleLowerCase();
    if (!id || seen.has(key)) continue;
    seen.add(key);
    ids.push(id);
  }
  return ids;
}

/** Distinct public names; protocol rows for the same name count once. */
export function uniquePublicModelCount(
  account: Pick<Account, "model_capabilities"> | null | undefined,
): number {
  return account ? capabilityPublicIds(account).length : 0;
}

export interface PlatformKeyModelRow {
  public_model: string;
  upstream_model: string;
  protocols: Account["model_capabilities"][number]["protocol"][];
}

/** Collapse protocol rows that share a public name; first upstream wins. */
export function platformKeyModelRows(
  account: Pick<Account, "model_capabilities"> | null | undefined,
): PlatformKeyModelRow[] {
  if (!account) return [];
  const rows: PlatformKeyModelRow[] = [];
  const index = new Map<string, PlatformKeyModelRow>();
  for (const capability of account.model_capabilities) {
    const publicModel = capability.public_model.trim();
    if (!publicModel) continue;
    const folded = publicModel.toLocaleLowerCase();
    const existing = index.get(folded);
    if (existing) {
      if (!existing.protocols.includes(capability.protocol)) {
        existing.protocols.push(capability.protocol);
      }
      continue;
    }
    const row: PlatformKeyModelRow = {
      public_model: publicModel,
      upstream_model: capability.upstream_model.trim() || publicModel,
      protocols: [capability.protocol],
    };
    index.set(folded, row);
    rows.push(row);
  }
  return rows;
}

export type PlatformCredentialTag =
  | { kind: "group"; name: string }
  | { kind: "token"; name: string }
  | { kind: "models"; count: number };

export const PLATFORM_CREDENTIAL_TAG_KEYS = {
  group: "分组 {name}",
  token: "令牌 {name}",
  models: "{count} 个模型",
} as const satisfies Record<PlatformCredentialTag["kind"], MessageKey>;

/**
 * Platform Key-row chips: site group, snapshot token name when it differs
 * from the local name, and unique public model count.
 */
export function platformCredentialTags(input: {
  group: string;
  tokenName: string;
  accountName: string;
  modelCount: number;
}): PlatformCredentialTag[] {
  const tags: PlatformCredentialTag[] = [];
  const group = input.group.trim();
  if (group) tags.push({ kind: "group", name: group });
  const token = input.tokenName.trim();
  const name = input.accountName.trim();
  if (token && token !== name) tags.push({ kind: "token", name: token });
  tags.push({ kind: "models", count: input.modelCount });
  return tags;
}

/**
 * Same public model on two Keys of one site is overlay, not a conflict:
 * `/v1/models` lists the name once, and routing tries those Keys in order.
 * Rates stay per Key and are never averaged.
 */
export function platformModelOverlay(
  keys: readonly Pick<Account, "id" | "model_capabilities">[],
): PlatformModelOverlay {
  const byKey = keys.map((key) => ({
    accountId: key.id,
    ids: capabilityPublicIds(key),
  }));
  const owners = new Map<string, string[]>();
  const display = new Map<string, string>();
  for (const key of byKey) {
    for (const id of key.ids) {
      const folded = id.toLocaleLowerCase();
      if (!display.has(folded)) display.set(folded, id);
      const list = owners.get(folded) ?? [];
      if (!list.includes(key.accountId)) list.push(key.accountId);
      owners.set(folded, list);
    }
  }
  const uniqueIds: string[] = [];
  const sharedIds: string[] = [];
  const shares: PlatformModelShare[] = [];
  for (const [folded, accountIds] of owners) {
    const id = display.get(folded) ?? folded;
    uniqueIds.push(id);
    shares.push({ id, accountIds });
    if (accountIds.length > 1) sharedIds.push(id);
  }
  return {
    uniqueIds,
    sharedIds,
    shares,
    keys: byKey.map((key) => {
      const shared = key.ids.filter((id) => (owners.get(id.toLocaleLowerCase())?.length ?? 0) > 1).length;
      return {
        accountId: key.accountId,
        total: key.ids.length,
        shared,
        exclusive: key.ids.length - shared,
      };
    }),
  };
}

export function platformKeyQuotaName(snapshot: PlatformSnapshot | null | undefined): string | null {
  const name = snapshot?.quotas.find((quota) => (
    quota.kind === "key_limit" && quota.scopeId.trim() && quota.scopeId.trim() !== "key"
  ))?.scopeId.trim();
  return name || null;
}

/** Link group first, then the first observed snapshot group. */
export function platformKeyGroupLabel(
  link: Pick<PlatformLink, "group"> | null | undefined,
  snapshot: PlatformSnapshot | null | undefined,
): string {
  const fromLink = link ? platformGroupLabel(link.group) : "";
  if (fromLink) return fromLink;
  for (const group of snapshot?.groups ?? []) {
    const label = platformGroupLabel(group);
    if (label) return label;
  }
  return "";
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

/** Overall remaining for a scope: wallet, or the Key quota that is not a time window. */
export function primaryQuota(
  quotas: readonly PlatformQuota[],
  kind: PlatformQuotaKind,
): PlatformQuota | null {
  const rows = quotasByKind(quotas)[kind];
  if (kind === "key_limit" || kind === "wallet") {
    return rows.find((row) => !row.period) ?? rows[0] ?? null;
  }
  return rows[0] ?? null;
}

/** Site-reported consume total for the current UTC month, when the optional catalog exists. */
export function walletMonthQuota(
  quotas: readonly PlatformQuota[],
): PlatformQuota | null {
  return quotas.find((quota) => quota.kind === "wallet" && quota.period === "month") ?? null;
}

/** Parent-card wallet figures: remaining, UTC-month used, lifetime used. */
export interface PlatformWalletMeter {
  unit: string;
  remaining: number | null;
  remainingUnlimited: boolean;
  historyUsed: number | null;
  monthUsed: number | null;
  observedAt: number | null;
}

export function platformWalletMeter(
  snapshot: PlatformSnapshot | null | undefined,
): PlatformWalletMeter | null {
  if (!snapshot) return null;
  const wallet = primaryQuota(snapshot.quotas, "wallet");
  const month = walletMonthQuota(snapshot.quotas);
  if (!wallet && !month) return null;
  const finite = (value: number | null | undefined): number | null => (
    value != null && Number.isFinite(value) ? value : null
  );
  return {
    unit: wallet?.unit || month?.unit || "",
    remaining: wallet?.unlimited ? null : finite(wallet?.remaining),
    remainingUnlimited: Boolean(wallet?.unlimited),
    historyUsed: wallet?.unlimited ? null : finite(wallet?.used),
    monthUsed: finite(month?.used),
    observedAt: snapshot.observedAt > 0 ? snapshot.observedAt : null,
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
  user_identity_required: "登录后才能查看价格",
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
  const code = unit.trim();
  if (/^[A-Za-z]{3}$/u.test(code)) {
    try {
      return new Intl.NumberFormat(locale, {
        style: "currency",
        currency: code.toUpperCase(),
        minimumFractionDigits: 2,
        maximumFractionDigits: 6,
      }).format(value);
    } catch {
      // Non-ISO labels fall through to a plain suffix.
    }
  }
  const formatted = new Intl.NumberFormat(locale, { maximumFractionDigits: 6 }).format(value);
  return code ? `${formatted} ${code}` : formatted;
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
