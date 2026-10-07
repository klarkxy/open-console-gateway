import assert from "node:assert/strict";
import test from "node:test";
import { aliasPageTarget, aliasPagePublicationKey, aliasPageRankText } from "./alias-page.ts";
test("alias page targets retain the exact account or destination and metadata intent", () => {
  const destination = aliasPageTarget({ accountId: null, destinationId: "dest", providerId: "provider", model: "alias", capabilities: true });
  assert.ok(destination && typeof destination !== "string");
  assert.deepEqual(destination.query, { destination: "dest", tab: "models", model: "alias", capabilities: "alias" });
  const account = aliasPageTarget({ accountId: "account", destinationId: null, providerId: "custom", model: "alias", capabilities: false });
  assert.ok(account && typeof account !== "string");
  assert.deepEqual(account.query, { account_id: "account" });
  assert.equal(aliasPageTarget(null), null);
});
test("publication keys and rank formatting do not infer routing availability", () => {
  assert.equal(aliasPagePublicationKey(" NAME "), "name");
  assert.equal(aliasPageRankText({ routingRanks: [2, 8] }), "2 · 8");
  assert.equal(aliasPageRankText({ routingRanks: [] }), "—");
});
