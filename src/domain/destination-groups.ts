import type { Account } from "../api/dashboard.ts";
import type { Destination, DestinationCredential } from "../api/destinations.ts";

export interface DestinationGroup {
  destination: Destination;
  /** V3 accounts for this destination's credentials, in routing_rank order. */
  accounts: Account[];
  /** Stable list key; equals the destination id. */
  id: string;
}

/**
 * Group V3 accounts by their destination projection. Membership comes from
 * credentials; missing V3 rows are skipped so the account store stays the
 * source of truth for cards. Populated groups sort by the minimum
 * `routing_rank`; empty destinations append in list order.
 */
export function buildDestinationGroups(
  destinations: readonly Destination[],
  credentials: readonly DestinationCredential[],
  accountsById: ReadonlyMap<string, Account>,
): DestinationGroup[] {
  const credentialsByDestination = new Map<string, DestinationCredential[]>();
  for (const credential of credentials) {
    const rows = credentialsByDestination.get(credential.destination_id) ?? [];
    rows.push(credential);
    credentialsByDestination.set(credential.destination_id, rows);
  }

  const populated: { group: DestinationGroup; minRank: number; index: number }[] = [];
  const empty: DestinationGroup[] = [];

  destinations.forEach((destination, index) => {
    const rows = [...(credentialsByDestination.get(destination.id) ?? [])]
      .sort((left, right) => (
        left.routing_rank - right.routing_rank || left.id.localeCompare(right.id)
      ));
    const accounts: Account[] = [];
    const seen = new Set<string>();
    let minRank = Number.POSITIVE_INFINITY;
    for (const credential of rows) {
      const account = accountsById.get(credential.legacy_account_id);
      if (!account || seen.has(account.id)) continue;
      seen.add(account.id);
      accounts.push(account);
      if (credential.routing_rank < minRank) minRank = credential.routing_rank;
    }
    const group: DestinationGroup = { destination, accounts, id: destination.id };
    if (accounts.length === 0) empty.push(group);
    else populated.push({ group, minRank, index });
  });

  populated.sort((left, right) => left.minRank - right.minRank || left.index - right.index);
  return [...populated.map((row) => row.group), ...empty];
}

export function expandGroupOrder(groups: readonly DestinationGroup[]): string[] {
  return groups.flatMap((group) => group.accounts.map((account) => account.id));
}

/**
 * Keep destination groups that still have at least one visible credential row.
 * Input groups are not mutated; each kept group gets a new `accounts` array.
 */
export function filterGroupRows(
  groups: readonly DestinationGroup[],
  visibleIds: ReadonlySet<string>,
): DestinationGroup[] {
  const result: DestinationGroup[] = [];
  for (const group of groups) {
    const accounts = group.accounts.filter((account) => visibleIds.has(account.id));
    if (accounts.length === 0) continue;
    result.push({ ...group, accounts });
  }
  return result;
}

/** Reorder accounts inside one destination group; other accounts keep their places. */
export function moveWithinGroup(
  accountIds: readonly string[],
  groupAccountIds: readonly string[],
  accountId: string,
  delta: number,
): string[] | null {
  const from = groupAccountIds.indexOf(accountId);
  const to = from + delta;
  if (from < 0 || to < 0 || to >= groupAccountIds.length) return null;
  const nextKeys = [...groupAccountIds];
  const [moved] = nextKeys.splice(from, 1);
  nextKeys.splice(to, 0, moved);
  const keySet = new Set(groupAccountIds);
  const result = [...accountIds];
  let next = 0;
  for (let index = 0; index < result.length; index++) {
    if (!keySet.has(result[index])) continue;
    result[index] = nextKeys[next];
    next += 1;
  }
  if (next !== nextKeys.length) return null;
  return result;
}

/**
 * One credential destination (singleton / account-owned) or any non-platform
 * destination that currently has exactly one account. Platform parents stay
 * grouped even when they have a single Key.
 */
export function isSingleAccountGroup(group: DestinationGroup): boolean {
  if (group.accounts.length !== 1) return false;
  return group.destination.max_credentials === 1
    || group.destination.legacy.kind !== "platform_parent";
}

/**
 * Reorder groups and their accounts to the live V3 list so drag previews
 * follow `accounts` immediately while credential ranks catch up on reload.
 */
export function alignDestinationGroupsToAccountOrder(
  groups: readonly DestinationGroup[],
  accountIds: readonly string[],
): DestinationGroup[] {
  const index = new Map(accountIds.map((id, position) => [id, position]));
  const rank = (id: string) => index.get(id) ?? Number.POSITIVE_INFINITY;
  const populated: DestinationGroup[] = [];
  const empty: DestinationGroup[] = [];
  for (const group of groups) {
    if (group.accounts.length === 0) {
      empty.push(group);
      continue;
    }
    populated.push({
      ...group,
      accounts: [...group.accounts].sort((left, right) => rank(left.id) - rank(right.id)),
    });
  }
  populated.sort((left, right) => {
    const leftRank = Math.min(...left.accounts.map((account) => rank(account.id)));
    const rightRank = Math.min(...right.accounts.map((account) => rank(account.id)));
    return leftRank - rightRank;
  });
  return [...populated, ...empty];
}
