import assert from "node:assert/strict";
import test from "node:test";
import type { PlatformAccountsView, PlatformSnapshot } from "../api/platform-accounts.ts";
import { platformRefreshSnapshot } from "./platform-refresh-snapshot.ts";

function snapshot(errors: string[]): PlatformSnapshot {
  return {
    observedAt: 100, stale: errors.length > 0, errors, quotas: [], models: [], prices: [], groups: [],
    billingPreference: null, walletOverflow: null,
  };
}
function view(): PlatformAccountsView {
  return {
    revision: 1, processGeneration: 1,
    accounts: [{ id: "parent", kind: "new_api", name: "Site", baseUrl: "https://example.test",
      hasUserCredential: true, version: 1, snapshot: snapshot([]) }],
    links: [{ accountId: "child", platformAccountId: "parent",
      group: { id: null, platform: null, subscriptionType: null, autoGroups: [], verified: false },
      snapshot: snapshot(["key_error"]) }],
  };
}

test("child feedback uses the child's failure, not the successful parent", () => {
  const data = view();
  assert.deepEqual(platformRefreshSnapshot(data, "parent")?.errors, []);
  assert.deepEqual(platformRefreshSnapshot(data, "parent", "child")?.errors, ["key_error"]);
});

test("missing, relinked and unobserved children cannot borrow another receipt", () => {
  const data = view();
  assert.equal(platformRefreshSnapshot(data, "missing"), null);
  assert.equal(platformRefreshSnapshot(data, "parent", "other"), null);
  assert.equal(platformRefreshSnapshot(data, "other", "child"), null);
  data.links[0]!.snapshot = null;
  assert.equal(platformRefreshSnapshot(data, "parent", "child"), null);
});
