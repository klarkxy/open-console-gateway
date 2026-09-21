import type { Account } from "../api/dashboard.ts";
import type { Destination, DestinationCredential } from "../api/destinations.ts";

export interface DestinationGroup {
  destination: Destination;
  /** Destination credentials in routing_rank order. Membership is credential-only. */
  credentials: DestinationCredential[];
  /** Stable list key; equals the destination id. */
  id: string;
}

/** Drag / V3 reorder key. Prefer the legacy account id when the overlay exists. */
export function groupRowOrderId(credential: DestinationCredential): string {
  return credential.legacy_account_id || credential.id;
}

export function overlayAccountForCredential(
  credential: DestinationCredential,
  accountsById: ReadonlyMap<string, Account>,
): Account | undefined {
  return accountsById.get(credential.legacy_account_id);
}

function inferenceCredentials(destinations: readonly Destination[], credentials: readonly DestinationCredential[]): DestinationCredential[] {
  const observers = new Set(destinations.flatMap((destination) => destination.observer_credential_id ? [destination.observer_credential_id] : []));
  return credentials.filter((credential) => !observers.has(credential.id));
}

/**
 * Group credentials by destination. Membership comes from the destination
 * projection; a missing V3 Account overlay does not drop the row. Usage and
 * platform overlays attach later by credential or destination id.
 * Populated groups sort by the minimum `routing_rank`; empty destinations
 * append in list order.
 */
export function buildDestinationGroups(
  destinations: readonly Destination[],
  credentials: readonly DestinationCredential[],
): DestinationGroup[] {
  const credentialsByDestination = new Map<string, DestinationCredential[]>();
  for (const credential of inferenceCredentials(destinations, credentials)) {
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
    const seen = new Set<string>();
    const unique: DestinationCredential[] = [];
    let minRank = Number.POSITIVE_INFINITY;
    for (const credential of rows) {
      if (seen.has(credential.id)) continue;
      seen.add(credential.id);
      unique.push(credential);
      if (credential.routing_rank < minRank) minRank = credential.routing_rank;
    }
    const group: DestinationGroup = { destination, credentials: unique, id: destination.id };
    if (unique.length === 0) empty.push(group);
    else populated.push({ group, minRank, index });
  });

  populated.sort((left, right) => left.minRank - right.minRank || left.index - right.index);
  return [...populated.map((row) => row.group), ...empty];
}

export function expandGroupOrder(groups: readonly DestinationGroup[]): string[] {
  return groups.flatMap((group) => group.credentials.map(groupRowOrderId));
}

/** One row per credential in global routing order; supplier grouping is presentation only. */
export function buildCredentialOrder(
  destinations: readonly Destination[],
  credentials: readonly DestinationCredential[],
): DestinationGroup[] {
  const byId = new Map(destinations.map((destination) => [destination.id, destination]));
  return inferenceCredentials(destinations, credentials)
    .sort((left, right) => left.routing_rank - right.routing_rank || left.id.localeCompare(right.id))
    .flatMap((credential) => {
      const destination = byId.get(credential.destination_id);
      return destination ? [{ id: credential.id, destination, credentials: [credential] }] : [];
    });
}

/**
 * Keep destination groups that still have at least one visible credential row.
 * Input groups are not mutated; each kept group gets a new `credentials` array.
 * `visibleIds` may contain credential ids or legacy account ids.
 */
export function filterGroupRows(
  groups: readonly DestinationGroup[],
  visibleIds: ReadonlySet<string>,
): DestinationGroup[] {
  const result: DestinationGroup[] = [];
  for (const group of groups) {
    const credentials = group.credentials.filter((credential) => (
      visibleIds.has(credential.id) || visibleIds.has(credential.legacy_account_id)
    ));
    if (credentials.length === 0) continue;
    result.push({ ...group, credentials });
  }
  return result;
}

/**
 * A credential is listed unless its V3 overlay exists and was filtered out.
 * Missing overlays never hide the row.
 */
export function includeCredentialRow(
  credential: DestinationCredential,
  visibleAccountIds: ReadonlySet<string>,
  knownAccountIds: ReadonlySet<string>,
): boolean {
  if (!knownAccountIds.has(credential.legacy_account_id)) return true;
  return visibleAccountIds.has(credential.legacy_account_id);
}

/** Reorder credentials inside one destination group; other rows keep their places. */
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
 * destination that currently has exactly one credential. Platform parents stay
 * grouped even when they have a single Key.
 */
export function isSingleAccountGroup(group: DestinationGroup): boolean {
  if (group.credentials.length !== 1) return false;
  return group.destination.max_credentials === 1
    || group.destination.legacy.kind !== "platform_parent";
}

function rowAlignRank(
  credential: DestinationCredential,
  index: ReadonlyMap<string, number>,
): number {
  return index.get(credential.legacy_account_id)
    ?? index.get(credential.id)
    ?? Number.POSITIVE_INFINITY;
}

/**
 * Reorder groups and their credentials to the live V3 list so drag previews
 * follow `accounts` immediately while credential ranks catch up on reload.
 * Alignment keys are `legacy_account_id` or `credential.id`.
 */
export function alignDestinationGroupsToAccountOrder(
  groups: readonly DestinationGroup[],
  accountIds: readonly string[],
): DestinationGroup[] {
  const index = new Map(accountIds.map((id, position) => [id, position]));
  const populated: DestinationGroup[] = [];
  const empty: DestinationGroup[] = [];
  for (const group of groups) {
    if (group.credentials.length === 0) {
      empty.push(group);
      continue;
    }
    populated.push({
      ...group,
      credentials: [...group.credentials].sort((left, right) => (
        rowAlignRank(left, index) - rowAlignRank(right, index)
      )),
    });
  }
  populated.sort((left, right) => {
    const leftRank = Math.min(...left.credentials.map((credential) => rowAlignRank(credential, index)));
    const rightRank = Math.min(...right.credentials.map((credential) => rowAlignRank(credential, index)));
    return leftRank - rightRank;
  });
  return [...populated, ...empty];
}
