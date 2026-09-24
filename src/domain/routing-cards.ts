import type { Destination, DestinationCredential, RoutingCardView } from "../api/destinations.ts";
import type { DestinationGroup } from "./destination-groups.ts";

/**
 * Pure layout helpers for routing cards. A RoutingCardList snapshot is the
 * single source of truth: the visible card order, then row order within each
 * card, is the persisted routing priority. These functions never mutate their
 * inputs and never touch the network; the destinations store commits results.
 */

/** Crypto-backed UUID when available, with a non-secure-context fallback. */
export function newRoutingCardId(): string {
  const cryptoApi = globalThis.crypto;
  if (cryptoApi && typeof cryptoApi.randomUUID === "function") {
    return cryptoApi.randomUUID();
  }
  return `card-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
}

function inferenceCredentialIds(
  destinations: readonly Destination[],
  credentials: readonly DestinationCredential[],
): Set<string> {
  const observers = new Set(
    destinations.flatMap((destination) => (
      destination.observer_credential_id ? [destination.observer_credential_id] : []
    )),
  );
  return new Set(
    credentials
      .filter((credential) => !observers.has(credential.id))
      .map((credential) => credential.id),
  );
}

/**
 * Build the visible card list from a saved snapshot. Each group id is the
 * routing card id (distinct from the destination id) so same-destination
 * cards stay separate, including every saved empty card. Rows
 * keep the card's saved credential order; usage/platform overlays attach
 * later by credential or legacy account id.
 */
export function buildRoutingCardGroups(
  cards: readonly RoutingCardView[],
  destinations: readonly Destination[],
  credentials: readonly DestinationCredential[],
): DestinationGroup[] {
  const destinationsById = new Map(destinations.map((destination) => [destination.id, destination]));
  const credentialsById = new Map(credentials.map((credential) => [credential.id, credential]));
  const inference = inferenceCredentialIds(destinations, credentials);
  const groups: DestinationGroup[] = [];

  for (const card of cards) {
    const destination = destinationsById.get(card.destination_id);
    if (!destination) continue;
    const rows: DestinationCredential[] = [];
    for (const id of card.credential_ids) {
      if (!inference.has(id)) continue;
      const credential = credentialsById.get(id);
      if (credential) rows.push(credential);
    }
    groups.push({ destination, credentials: rows, id: card.id });
  }

  return groups;
}

/** Insert an empty card for `destinationId` adjacent to (right after) `anchorCardId`. */
export function addEmptyCardAfter(
  cards: readonly RoutingCardView[],
  destinationId: string,
  anchorCardId: string,
): RoutingCardView[] {
  const next: RoutingCardView[] = [];
  const fresh: RoutingCardView = {
    id: newRoutingCardId(),
    destination_id: destinationId,
    credential_ids: [],
  };
  let inserted = false;
  for (const card of cards) {
    next.push(card);
    if (!inserted && card.id === anchorCardId) {
      next.push(fresh);
      inserted = true;
    }
  }
  if (!inserted) next.push(fresh);
  return next;
}

/**
 * Remove an empty card. Only a card with no rows may be removed; the
 * destination keeps at least one card (a populated sibling or a remaining
 * empty card). Returns null when removal is not allowed.
 */
export function removeEmptyCard(
  cards: readonly RoutingCardView[],
  cardId: string,
): RoutingCardView[] | null {
  const card = cards.find((row) => row.id === cardId);
  if (!card || card.credential_ids.length > 0) return null;
  const siblings = cards.filter((row) => row.destination_id === card.destination_id && row.id !== cardId);
  const destinationSurvives = siblings.length > 0;
  if (!destinationSurvives) return null;
  return cards.filter((row) => row.id !== cardId);
}

/**
 * Move one credential to a target card of the same destination, inserting at
 * `targetIndex` (default: append). Rejects a different-destination move by
 * returning null. Empty cards are valid targets.
 */
export function moveCredentialToCard(
  cards: readonly RoutingCardView[],
  credentialId: string,
  targetCardId: string,
  targetIndex?: number,
): RoutingCardView[] | null {
  const target = cards.find((card) => card.id === targetCardId);
  if (!target) return null;
  // Find the credential's current card and confirm same destination.
  let sourceDestination: string | null = null;
  for (const card of cards) {
    if (card.credential_ids.includes(credentialId)) {
      sourceDestination = card.destination_id;
      break;
    }
  }
  if (sourceDestination === null || sourceDestination !== target.destination_id) return null;

  const without = cards.map((card) => ({
    ...card,
    credential_ids: card.credential_ids.filter((id) => id !== credentialId),
  }));
  return without.map((card) => {
    if (card.id !== targetCardId) return card;
    const ids = [...card.credential_ids];
    const at = targetIndex === undefined
      ? ids.length
      : Math.max(0, Math.min(targetIndex, ids.length));
    ids.splice(at, 0, credentialId);
    return { ...card, credential_ids: ids };
  });
}

/** Reorder a credential within its own card; returns null when the move is invalid. */
export function moveCredentialWithinCard(
  cards: readonly RoutingCardView[],
  cardId: string,
  credentialId: string,
  delta: number,
): RoutingCardView[] | null {
  const card = cards.find((row) => row.id === cardId);
  if (!card) return null;
  const from = card.credential_ids.indexOf(credentialId);
  const to = from + delta;
  if (from < 0 || to < 0 || to >= card.credential_ids.length) return null;
  const ids = [...card.credential_ids];
  const [moved] = ids.splice(from, 1);
  ids.splice(to, 0, moved);
  return cards.map((row) => (row.id === cardId ? { ...row, credential_ids: ids } : row));
}
