import assert from "node:assert/strict";
import test from "node:test";
import { createPinia, defineStore, setActivePinia } from "pinia";
import { invalidateManagementPages } from "./managementPages.ts";

test("confirmed write invalidation retains other page content and does not instantiate an unused page", () => {
  const pinia = createPinia();
  setActivePinia(pinia);
  const useAccounts = defineStore("accountPage", {
    state: () => ({ content: "account", invalidations: 0 }),
    actions: { invalidate() { this.invalidations++; } },
  });
  const useAliases = defineStore("aliasPage", {
    state: () => ({ content: "alias", invalidations: 0 }),
    actions: { invalidate() { this.invalidations++; } },
  });
  const accounts = useAccounts(pinia);
  const aliases = useAliases(pinia);
  invalidateManagementPages("accountPage");
  assert.equal(accounts.invalidations, 0);
  assert.equal(aliases.invalidations, 1);
  assert.equal(aliases.content, "alias");
  assert.equal(pinia._s.has("providerPage"), false);
  invalidateManagementPages();
  assert.equal(accounts.invalidations, 1);
  assert.equal(aliases.invalidations, 2);
});

test("unused or disposed page stores need no invalidation work", () => {
  const pinia = createPinia();
  setActivePinia(pinia);
  invalidateManagementPages();
  assert.equal(pinia._s.size, 0);
});
