import assert from "node:assert/strict";
import test from "node:test";
import { hideRemovedAccountCredentials } from "./confirmed-account-removal.ts";

const credentials = [{ id: "ca", legacy_account_id: "a" }, { id: "cb", legacy_account_id: "b" }];
const cards = [{ id: "one", credential_ids: ["ca", "cb"] }, { id: "two", credential_ids: ["cb"] }];

test("confirmed deletion removes only its credential and card membership", () => {
  const next = hideRemovedAccountCredentials(credentials, cards, new Set(["a"]));
  assert.deepEqual(next.credentials, [credentials[1]]);
  assert.deepEqual(next.cards[0]?.credential_ids, ["cb"]);
  assert.equal(next.cards[1], cards[1]);
  assert.deepEqual(cards[0]?.credential_ids, ["ca", "cb"]);
  assert.equal(credentials.length, 2);
});

test("missing account overlays and unrelated deletion markers preserve the projection", () => {
  const empty = hideRemovedAccountCredentials(credentials, cards, new Set());
  assert.equal(empty.credentials, credentials);
  assert.equal(empty.cards, cards);
  const unrelated = hideRemovedAccountCredentials(credentials, cards, new Set(["other"]));
  assert.equal(unrelated.credentials, credentials);
  assert.equal(unrelated.cards, cards);
});

test("removing the last Key preserves the empty card and its stable identity", () => {
  const next = hideRemovedAccountCredentials(credentials, cards, new Set(["a", "b"]));
  assert.deepEqual(next.credentials, []);
  assert.deepEqual(next.cards.map(card => ({ id: card.id, keys: card.credential_ids })), [
    { id: "one", keys: [] }, { id: "two", keys: [] },
  ]);
});
