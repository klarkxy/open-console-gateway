import assert from "node:assert/strict";
import test from "node:test";
import { createPinia, setActivePinia } from "pinia";
import { dashboardApi, type Account } from "../api/dashboard.ts";
import { routingCardsApi, type DestinationCredential, type RoutingCardListSnapshot } from "../api/destinations.ts";
import { useAccountsStore } from "./accounts.ts";
import { useDestinationsStore } from "./destinations.ts";
import { useControlPlaneStore } from "./controlPlane.ts";

function fixture() {
  setActivePinia(createPinia());
  useControlPlaneStore().sync({ revision: 1, processGeneration: 1 });
  const accounts = useAccountsStore();
  const destinations = useDestinationsStore();
  const snapshot: RoutingCardListSnapshot = {
    destinations: [],
    credentials: [{ id: "ca", legacy_account_id: "a" }, { id: "cb", legacy_account_id: "b" }] as DestinationCredential[],
    cards: [{ id: "card", destination_id: "dest", credential_ids: ["ca", "cb"] }],
    expectation: { expectedRevision: 1, processGeneration: 1 },
  };
  destinations.commitSnapshot(snapshot);
  return { accounts, destinations, snapshot };
}

test("deletion removes projected Key rows even if the projection reload fails", async t => {
  const f = fixture();
  assert.equal(f.destinations.credentials.length, 2, "missing overlays alone do not hide rows");
  f.accounts.removeAccount("a");
  t.mock.method(routingCardsApi, "listSnapshot", async () => { throw new Error("offline"); });
  await assert.rejects(f.destinations.refreshAfterMutation(), /offline/);
  assert.deepEqual(f.destinations.credentials.map(row => row.id), ["cb"]);
  assert.deepEqual(f.destinations.cards[0]?.credential_ids, ["cb"]);
  assert.equal(f.destinations.credentialsByLegacyAccountId.has("a"), false);
  assert.deepEqual(f.destinations.expectation, f.snapshot.expectation, "a display projection never invents CAS tokens");
  f.destinations.commitSnapshot(f.snapshot);
  assert.deepEqual(f.destinations.credentials.map(row => row.id), ["cb"], "late receipts cannot restore the deleted row");
});

test("a new authoritative account list can restore a deliberately reimported row", async t => {
  const f = fixture();
  f.accounts.removeAccount("a");
  t.mock.method(dashboardApi, "getAccountsSnapshot", async () => ({ accounts: [{ id: "a" } as Account], expectation: { expectedRevision: 1, processGeneration: 1 } }));
  await f.accounts.loadPresented();
  assert.deepEqual(f.destinations.credentials.map(row => row.id), ["ca", "cb"]);
  assert.deepEqual(f.destinations.cards[0]?.credential_ids, ["ca", "cb"]);
});
