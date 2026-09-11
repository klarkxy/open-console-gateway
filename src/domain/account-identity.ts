import type { Account } from "../api/dashboard.ts";
import type { Identity, IdentityCredential, IdentitySubscription } from "../api/identities.ts";
import { identityJoinKey } from "../api/identities.ts";
import type { AuthState } from "../api/identities.ts";
import type { ProviderCatalogEntry } from "../api/providers.ts";
import { t } from "../i18n/index.ts";
import {
  accountIsReady,
  accountRoutingDraftLabel,
  accountStatusLabel,
  accountStatusTagType,
  formatCooldownRemaining,
  type AccountStatusTagType,
} from "./account-display.ts";
import { isCooling } from "./accounts-usage.ts";
import { isCustomApiAccount } from "./custom-account.ts";
import { isZenFreeAccount } from "./account-providers.ts";
import { planForAccount } from "./plans.ts";

/**
 * Overlay the secret-free V4 identity projection onto a V3 Account card.
 * Join is `legacy.kind+id`. These helpers never invent health, expiry, or
 * a verified-wallet claim, and they never trigger probes.
 */

export function identityForAccount(
  identities: readonly Identity[],
  accountId: string,
): Identity | null {
  const key = identityJoinKey({ kind: "account", id: accountId });
  return identities.find((row) => identityJoinKey(row.legacy) === key) ?? null;
}

/** Inference Keys only. Platform observer credentials stay off the ordinary card. */
export function inferenceCredentials(identity: Identity | null): IdentityCredential[] {
  if (!identity) return [];
  return identity.credentials.filter((row) => row.credential.purpose === "inference");
}

export function accountShowsDeclaredRelation(identity: Identity | null): boolean {
  return !!identity && identity.declared_relations.length > 0;
}

export function accountCredentialCountLabel(identity: Identity | null): string | null {
  if (!identity || identity.credentials.length <= 1) return null;
  return t("{count} 个凭据", { count: identity.credentials.length });
}

export function inferenceAuthState(identity: Identity | null): AuthState | null {
  const rows = inferenceCredentials(identity);
  if (rows.length === 0) return null;
  if (rows.some((row) => row.credential.auth_state === "invalid")) return "invalid";
  if (rows.some((row) => row.credential.auth_state === "unknown")) return "unknown";
  if (rows.every((row) => row.credential.auth_state === "valid")) return "valid";
  return "unknown";
}

export function inferenceLastError(identity: Identity | null): string | null {
  for (const row of inferenceCredentials(identity)) {
    if (row.last_error) return row.last_error;
  }
  return null;
}

function inferenceSubscription(identity: Identity | null): IdentitySubscription | null {
  for (const row of inferenceCredentials(identity)) {
    if (row.subscription) return row.subscription;
  }
  return null;
}

function inventsLifecycleDates(
  account: Pick<Account, "provider_id">,
  catalog: readonly ProviderCatalogEntry[] | null,
): boolean {
  if (isCustomApiAccount(account)) return true;
  return planForAccount(account, catalog)?.id === "dynamic-http";
}

/** V3 card expiry: built-in billed families with stored dates. Custom/Zen hide. */
export function v3AccountShowsExpiry(
  account: Account,
  catalog: readonly ProviderCatalogEntry[] | null,
): boolean {
  const plan = planForAccount(account, catalog);
  return accountIsReady(account)
    && !!plan
    && plan.id !== "custom-endpoint"
    && !isZenFreeAccount(account)
    && !!account.purchase_date
    && !!account.expires_on;
}

export type AccountExpiryDisplayKind = "v3" | "hidden" | "unknown";

/**
 * D07: a null V4 subscription must not fabricate expiry or a zero price.
 * Dynamic/custom invented dates become “未提供” or hidden. Built-in Go
 * cards that already have a real V3 purchase_date keep that V3 UI.
 */
export function accountExpiryDisplay(
  account: Account,
  identity: Identity | null,
  catalog: readonly ProviderCatalogEntry[] | null,
): AccountExpiryDisplayKind {
  const v3Shows = v3AccountShowsExpiry(account, catalog);
  if (!identity) return v3Shows ? "v3" : "hidden";
  if (inferenceSubscription(identity) !== null) return v3Shows ? "v3" : "hidden";
  if (inventsLifecycleDates(account, catalog)) return v3Shows ? "unknown" : "hidden";
  return v3Shows ? "v3" : "hidden";
}

export function presentedAccountStatusLabel(
  account: Account,
  identity: Identity | null,
  now = Date.now(),
): string {
  if (isZenFreeAccount(account) || !accountIsReady(account) || accountRoutingDraftLabel(account)) {
    return accountStatusLabel(account, now);
  }

  const auth = inferenceAuthState(identity);
  if (auth === "invalid" || account.auth_error) {
    return account.enabled ? t("不可用") : `${t("已禁用")} · ${t("不可用")}`;
  }
  if (!account.enabled) return t("已禁用");
  if (isCooling(account, now)) {
    return t("冷却中·剩 {time}", { time: formatCooldownRemaining(account, now) });
  }
  if (auth === "unknown") return t("待验证");
  return t("已启用");
}

export function presentedAccountStatusTagType(
  account: Account,
  identity: Identity | null,
  now = Date.now(),
): AccountStatusTagType {
  if (isZenFreeAccount(account) || !accountIsReady(account) || accountRoutingDraftLabel(account)) {
    return accountStatusTagType(account, now);
  }
  const auth = inferenceAuthState(identity);
  if (auth === "invalid" || account.auth_error) return "error";
  if (!account.enabled) return "default";
  if (isCooling(account, now)) return "warning";
  if (auth === "unknown") return "warning";
  return "default";
}
