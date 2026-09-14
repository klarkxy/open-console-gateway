import assert from "node:assert/strict";
import test from "node:test";
import { reconcileEditingAccount } from "./account-cas.ts";

test("reconcileEditingAccount retains only accounts present in the refreshed list", () => {
  const loaded = [{ id: "a1" }, { id: "a2" }];

  // Surviving account: fresh copy returned, caller keeps the modal open.
  assert.deepEqual(reconcileEditingAccount(loaded, "a2"), { id: "a2" });

  // Deleted account: null, caller must close the modal so it cannot morph
  // into create mode.
  assert.equal(reconcileEditingAccount(loaded, "gone"), null);
  assert.equal(reconcileEditingAccount(loaded, null), null);
  assert.equal(reconcileEditingAccount([], "a1"), null);
});
