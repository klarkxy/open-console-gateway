import assert from "node:assert/strict";
import test from "node:test";
import type { Destination, DestinationCredential, RoutingCardView } from "../api/destinations.ts";
import { addEmptyCardAfter, buildRoutingCardGroups, moveCredentialToCard, moveCredentialWithinCard, removeEmptyCard } from "./routing-cards.ts";
import { moveItem } from "./account-lifecycle.ts";
const initial: RoutingCardView[] = [
  { id: "a", destination_id: "supplier-a", credential_ids: ["a1", "a2"] },
  { id: "b", destination_id: "supplier-b", credential_ids: ["b1"] },
];
test("a second supplier card supports A1 B1 A2 without duplicating credentials", () => {
  const split = addEmptyCardAfter(initial, "supplier-a", "a");
  assert.equal(split.length, 3);
  const moved = moveCredentialToCard(split, "a2", split[1].id)!;
  const ordered = moveItem(moved, 2, 1);
  assert.deepEqual(ordered.flatMap(c => c.credential_ids), ["a1", "b1", "a2"]);
  assert.deepEqual(initial[0].credential_ids, ["a1", "a2"]);
  assert.equal(ordered[0].destination_id, ordered[2].destination_id);
});
test("saved adjacent cards and empty cards keep their IDs and row order", () => {
  const cards = [initial[0], { id: "empty1", destination_id: "supplier-a", credential_ids: [] },
    { id: "empty2", destination_id: "supplier-a", credential_ids: [] }, initial[1]];
  const destinations = [{ id: "supplier-a", observer_credential_id: "observer" }, { id: "supplier-b", observer_credential_id: null }] as Destination[];
  const credentials = ["a2", "a1", "b1", "observer"].map(id => ({ id, enabled: id !== "a2" })) as DestinationCredential[];
  const groups = buildRoutingCardGroups(cards, destinations, credentials);
  assert.deepEqual(groups.map(g => g.id), ["a", "empty1", "empty2", "b"]);
  assert.deepEqual(groups[0].credentials.map(c => c.id), ["a1", "a2"]);
});
test("a credential cannot move to another destination or an unknown card", () => {
  assert.equal(moveCredentialToCard(initial, "a1", "b"), null);
  assert.equal(moveCredentialToCard(initial, "a1", "missing"), null);
  assert.equal(moveCredentialToCard(initial, "missing", "a"), null);
});
test("only an extra empty card can be removed", () => {
  assert.equal(removeEmptyCard(initial, "a"), null);
  const cards = addEmptyCardAfter(initial, "supplier-a", "a");
  assert.deepEqual(removeEmptyCard(cards, cards[1].id), initial);
  assert.equal(removeEmptyCard([cards[1]], cards[1].id), null);
});
test("row movement keeps card boundaries and leaves the input unchanged", () => {
  assert.deepEqual(moveCredentialWithinCard(initial, "a", "a2", -1)?.[0].credential_ids, ["a2", "a1"]);
  assert.equal(moveCredentialWithinCard(initial, "a", "a1", -1), null);
  assert.deepEqual(initial[0].credential_ids, ["a1", "a2"]);
});
