import assert from "node:assert/strict";
import test from "node:test";
import { platformRefreshErrors } from "./platform-refresh.ts";

const parents = [
  { id: "a", snapshot: { errors: ["new_api.user_self.unauthorized"] } },
  { id: "b", snapshot: { errors: [] } },
];
const links = [
  { accountId: "key-a", platformAccountId: "a", snapshot: { errors: [] } },
  { accountId: "key-b", platformAccountId: "b", snapshot: { errors: ["sub2api.usage.timeout"] } },
];

test("successful child refresh does not report an old parent failure", () => {
  assert.deepEqual(platformRefreshErrors(parents, links, "a", "key-a"), []);
});

test("failed child refresh is not hidden by a successful parent", () => {
  assert.deepEqual(platformRefreshErrors(parents, links, "b", "key-b"), ["sub2api.usage.timeout"]);
});

test("parent refresh reports only its own errors", () => {
  assert.deepEqual(platformRefreshErrors(parents, links, "a"), ["new_api.user_self.unauthorized"]);
  assert.deepEqual(platformRefreshErrors(parents, links, "b"), []);
});

test("a moved or missing child cannot use another parent's snapshot", () => {
  assert.deepEqual(platformRefreshErrors(parents, links, "a", "key-b"), []);
  assert.deepEqual(platformRefreshErrors(parents, links, "a", "missing"), []);
  assert.deepEqual(platformRefreshErrors([], [], "a"), []);
});

test("returned errors cannot mutate the stored snapshot", () => {
  const errors = platformRefreshErrors(parents, links, "a");
  errors.length = 0;
  assert.equal(parents[0].snapshot.errors.length, 1);
});
