import type { Account } from "../api/dashboard.ts";
import type {
  Identity,
  IdentityBinding,
  IdentityCredential,
  IdentitySubscription,
} from "../api/identities.ts";
import { identityJoinKey } from "../api/identities.ts";
import type { AuthState } from "../api/identities.ts";
import type { ProviderCatalogEntry } from "../api/providers.ts";
import {
  accountIsReady,
  accountRoutingDraftState,
  accountStatus,
  accountStatusTagType,
  cooldownRemaining,
  type AccountStatus,
  type AccountStatusTagType,
} from "./account-display.ts";
import { isCooling } from "./accounts-usage.ts";
import { isCustomApiAccount } from "./custom-account.ts";
import { isZenFreeAccount } from "./account-providers.ts";
import { planForAccount } from "./plans.ts";

/**
 * Overlay the secret-free V4 identity projection onto a V3 Account card.
 * Join is `legacy.kind+id`. These helpers never invent health, expiry, or
 * a verified-wallet claim, and they never trigger probes. They return codes
 * and raw data only; the view layer localizes (src/views/account-status-text.ts).
 */

/**
 * The credential whose `legacy` account id matches this card.
 * An identity can hold several Keys; never fall back to a sibling.
 */
export function credentialForAccount(
  identity: Identity | null,
  accountId: string,
): IdentityCredential | null {
  if (!identity) return null;
  const key = identityJoinKey({ kind: "account", id: accountId });
  const matches = identity.credentials.filter((row) => identityJoinKey(row.legacy) === key);
  if (matches.length === 0) return null;
  return matches.find((row) => row.credential.purpose === "inference") ?? matches[0];
}

/** Inference Keys only. Platform observer credentials stay off the ordinary card. */
export function inferenceCredentials(identity: Identity | null): IdentityCredential[] {
  if (!identity) return [];
  return identity.credentials.filter((row) => row.credential.purpose === "inference");
}

function inferenceCredentialForAccount(
  identity: Identity | null,
  accountId: string,
): IdentityCredential | null {
  const row = credentialForAccount(identity, accountId);
  if (!row || row.credential.purpose !== "inference") return null;
  return row;
}

export function accountShowsDeclaredRelation(identity: Identity | null): boolean {
  return !!identity && identity.declared_relations.length > 0;
}

/** Extra-credential count badge data; null when a single Key adds no chrome. */
export function accountCredentialCount(identity: Identity | null): number | null {
  if (!identity || identity.credentials.length <= 1) return null;
  return identity.credentials.length;
}

export function inferenceAuthState(
  identity: Identity | null,
  accountId: string,
): AuthState | null {
  const row = inferenceCredentialForAccount(identity, accountId);
  return row?.credential.auth_state ?? null;
}

export function inferenceLastError(
  identity: Identity | null,
  accountId: string,
): string | null {
  return inferenceCredentialForAccount(identity, accountId)?.last_error ?? null;
}

function inferenceSubscription(
  identity: Identity | null,
  accountId: string,
): IdentitySubscription | null {
  return inferenceCredentialForAccount(identity, accountId)?.subscription ?? null;
}

/** The selected card's inference binding; never a sibling credential's binding. */
export function selectedInferenceBinding(
  identity: Identity | null,
  accountId: string,
): IdentityBinding | null {
  return inferenceCredentialForAccount(identity, accountId)?.bindings[0] ?? null;
}

export function selectedBindingDisabled(
  identity: Identity | null,
  accountId: string,
): boolean {
  const binding = selectedInferenceBinding(identity, accountId);
  return binding !== null && binding.enabled === false;
}

/**
 * Inference Keys that share this card's stored quota pool. A singleton or
 * missing pool is independent. Never inferred from identity membership or
 * from quota windows / cooldown.
 */
export function sharedQuotaSiblings(
  identity: Identity | null,
  accountId: string,
): IdentityCredential[] {
  const selected = inferenceCredentialForAccount(identity, accountId);
  const poolId = selected?.quota_pool_id?.trim();
  if (!selected || !poolId) return [];
  return inferenceCredentials(identity).filter((row) => (
    row.credential.id !== selected.credential.id
    && row.quota_pool_id === poolId
  ));
}

/** Quota-share badge data: a single named sibling or a count. */
export type QuotaShare = { kind: "named"; name: string } | { kind: "count"; count: number };

export function selectedQuotaShare(
  identity: Identity | null,
  accountId: string,
  nameForAccountId?: (legacyAccountId: string) => string | null,
): QuotaShare | null {
  const siblings = sharedQuotaSiblings(identity, accountId);
  if (siblings.length === 0) return null;
  const names = siblings.map((row) => {
    const named = nameForAccountId?.(row.legacy.id)?.trim();
    return named || row.legacy.id;
  });
  if (names.length === 1) return { kind: "named", name: names[0] };
  return { kind: "count", count: names.length };
}

/** Only-scope summary data for the selected card; null when unrestricted. */
export type ModelRestriction =
  | { kind: "restricted" }
  | { kind: "single"; model: string }
  | { kind: "count"; count: number };

export function selectedModelRestriction(
  identity: Identity | null,
  accountId: string,
): ModelRestriction | null {
  const binding = selectedInferenceBinding(identity, accountId);
  if (!binding || binding.model_scope.kind !== "only") return null;
  const names = binding.model_scope.models.map((name) => name.trim()).filter(Boolean);
  if (names.length === 0) return { kind: "restricted" };
  if (names.length === 1) return { kind: "single", model: names[0] };
  return { kind: "count", count: names.length };
}

function inventsLifecycleDates(
  account: Pick<Account, "provider_id">,
  catalog: readonly ProviderCatalogEntry[] | null,
): boolean {
  if (isCustomApiAccount(account)) return true;
  return planForAccount(account, catalog)?.dynamic === true;
}

/** V3 card expiry: built-in billed families with stored dates. Custom/Zen hide. */
export function v3AccountShowsExpiry(
  account: Account,
  catalog: readonly ProviderCatalogEntry[] | null,
): boolean {
  const plan = planForAccount(account, catalog);
  return accountIsReady(account)
    && !!plan
    && plan.kind !== "custom"
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
  if (inferenceSubscription(identity, account.id) !== null) return v3Shows ? "v3" : "hidden";
  if (inventsLifecycleDates(account, catalog)) return v3Shows ? "unknown" : "hidden";
  return v3Shows ? "v3" : "hidden";
}

export function presentedAccountStatus(
  account: Account,
  identity: Identity | null,
  now = Date.now(),
): AccountStatus {
  if (isZenFreeAccount(account) || !accountIsReady(account) || accountRoutingDraftState(account)) {
    return accountStatus(account, now);
  }

  const auth = inferenceAuthState(identity, account.id);
  if (auth === "invalid" || account.auth_error) {
    return account.enabled ? { kind: "unavailable" } : { kind: "disabled-unavailable" };
  }
  if (!account.enabled) return { kind: "disabled" };
  if (isCooling(account, now)) {
    return {
      kind: "cooling",
      remaining: cooldownRemaining(account, now) ?? { unit: "seconds", seconds: 0 },
    };
  }
  // Unknown auth is not a third card state. The enable switch is the draft/live gate.
  return { kind: "enabled" };
}

export function presentedAccountStatusTagType(
  account: Account,
  identity: Identity | null,
  now = Date.now(),
): AccountStatusTagType {
  if (isZenFreeAccount(account) || !accountIsReady(account) || accountRoutingDraftState(account)) {
    return accountStatusTagType(account, now);
  }
  const auth = inferenceAuthState(identity, account.id);
  if (auth === "invalid" || account.auth_error) return "error";
  if (!account.enabled) return "error";
  if (isCooling(account, now)) return "warning";
  return "success";
}
