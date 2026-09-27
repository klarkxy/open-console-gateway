import type { PlatformAccountsView, PlatformSnapshot } from "../api/platform-accounts.ts";

/** A child refresh must never borrow a parent or sibling's observation. */
export function platformRefreshSnapshot(
  view: Pick<PlatformAccountsView, "accounts" | "links">,
  parentId: string,
  accountId?: string,
): PlatformSnapshot | null {
  if (accountId !== undefined) {
    return view.links.find(link => link.platformAccountId === parentId && link.accountId === accountId)?.snapshot ?? null;
  }
  return view.accounts.find(parent => parent.id === parentId)?.snapshot ?? null;
}
