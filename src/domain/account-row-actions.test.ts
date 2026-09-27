import assert from "node:assert/strict";
import test from "node:test";
import { accountRowActions } from "./account-row-actions.ts";

const target = { id: "key-a", name: "Key A" };
const action = (key: string, disabled = false) => ({ key, disabled, accountId: target.id, accountName: target.name });
const caps = { platformLinked: true, refreshSupported: true, deleting: false };

test("linked Keys retain unlink and gain one local delete action without mutating inputs", () => {
  const source = [action("refresh-usage"), action("unlink")];
  const result = accountRowActions(source, target, caps);
  assert.deepEqual(result.map(row => row.key), ["refresh-usage", "unlink", "delete"]);
  assert.equal(result[2]?.accountId, "key-a");
  assert.equal(source.length, 2);
  assert.equal(accountRowActions(result, target, caps).filter(row => row.key === "delete").length, 1);
});

test("non-platform and missing-account rows never acquire a delete action", () => {
  assert.deepEqual(accountRowActions([], target, { ...caps, platformLinked: false }), []);
  assert.deepEqual(accountRowActions([], null, caps), []);
});

test("unsupported refresh is absent and deletion remains available", () => {
  const result = accountRowActions([action("refresh-usage"), action("unlink")], target, { ...caps, refreshSupported: false });
  assert.deepEqual(result.map(row => row.key), ["unlink", "delete"]);
});

test("deletion respects platform write blockers and disables repeated actions", () => {
  assert.equal(accountRowActions([action("unlink", true)], target, caps).at(-1)?.disabled, true);
  const result = accountRowActions([action("unlink"), action("refresh-usage")], target, { ...caps, deleting: true });
  assert.ok(result.every(row => row.disabled));
});

test("a billing load disables refresh without blocking deletion", () => {
  const result = accountRowActions([action("refresh-usage"), action("unlink")], target, { ...caps, refreshBusy: true });
  assert.equal(result.find(row => row.key === "refresh-usage")?.disabled, true);
  assert.equal(result.find(row => row.key === "delete")?.disabled, false);
});
