import assert from "node:assert/strict";
import test from "node:test";
import { sortProvidersByName } from "./provider-sort.ts";

test("provider name ordering is case-insensitive, numeric, reversible, and leaves source order intact", () => {
  const providers = [
    { id: "z", name: "Zebra" },
    { id: "10", name: "api 10" },
    { id: "2", name: "API 2" },
    { id: "a", name: "alpha" },
    { id: "duplicate", name: "Alpha" },
  ];
  const original = providers.map((item) => item.id);
  assert.deepEqual(sortProvidersByName(providers, (item) => item.name).map((item) => item.id), ["a", "duplicate", "2", "10", "z"]);
  assert.deepEqual(sortProvidersByName(providers, (item) => item.name, "name_desc").map((item) => item.id), ["z", "10", "2", "a", "duplicate"]);
  assert.deepEqual(providers.map((item) => item.id), original);
});
