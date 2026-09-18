import type { Account } from "../api/dashboard.ts";
import type { ProviderCatalogEntry } from "../api/providers.ts";
import { isCooling, isFreeCooling } from "../domain/accounts-usage.ts";
import { daysUntilDate } from "../domain/account-lifecycle.ts";
import { accountCapabilities } from "../domain/account-capabilities.ts";
import { planForAccount } from "../domain/plans.ts";

/**
 * The Dashboard "needs attention" area: a single honest list of accounts that
 * need the operator, derived from the same state the Accounts page shows.
 * Disabled accounts are a deliberate choice, not a problem, so they never
 * appear here.
 */

export type AttentionReason =
  | "auth-error"
  | "expired"
  | "cooling"
  | "setup-incomplete";

export interface AttentionItem {
  accountId: string;
  accountName: string;
  reason: AttentionReason;
}

const REASON_PRIORITY: Record<AttentionReason, number> = {
  "auth-error": 0,
  expired: 1,
  cooling: 2,
  "setup-incomplete": 3,
};

function accountCooling(
  account: Account,
  now: number,
  catalog?: readonly ProviderCatalogEntry[] | null,
): boolean {
  return accountCapabilities(account, catalog).freeCooldownOnly
    ? isFreeCooling(account, now)
    : isCooling(account, now);
}

export function buildNeedsAttention(
  accounts: readonly Account[],
  now: number = Date.now(),
  catalog?: readonly ProviderCatalogEntry[] | null,
): AttentionItem[] {
  const items: AttentionItem[] = [];
  for (const account of accounts) {
    const ready = account.setup_step === "ready";
    if (ready && account.auth_error) {
      items.push({ accountId: account.id, accountName: account.name, reason: "auth-error" });
      continue;
    }
    if (ready && account.enabled) {
      // Only built-in billed families model a purchase/expiry cadence, and the
      // catalog is the authority on which providers those are. Custom API,
      // Zen Free, CPA, and user-defined (dynamic) Providers carry no lifecycle
      // dates, so their accounts never raise expiry attention — a synthetic or
      // blanked date there is not a billing fact. A failed (null) catalog
      // keeps the narrow offline Go/Zen projection; a successful empty catalog
      // stays authoritative.
      const plan = planForAccount(account, catalog);
      const caps = accountCapabilities(account, catalog);
      const expiryDays = plan
        && caps.hasExpiry
        && !plan.dynamic
        && account.expires_on
        ? daysUntilDate(account.expires_on, now)
        : Number.POSITIVE_INFINITY;
      if (Number.isFinite(expiryDays) && expiryDays < 0) {
        items.push({ accountId: account.id, accountName: account.name, reason: "expired" });
        continue;
      }
      if (accountCooling(account, now, catalog)) {
        items.push({ accountId: account.id, accountName: account.name, reason: "cooling" });
        continue;
      }
    }
    if (!ready) {
      items.push({ accountId: account.id, accountName: account.name, reason: "setup-incomplete" });
    }
  }
  return items.sort(
    (left, right) => REASON_PRIORITY[left.reason] - REASON_PRIORITY[right.reason]
      || left.accountName.localeCompare(right.accountName),
  );
}
