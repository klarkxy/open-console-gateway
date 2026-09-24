import assert from "node:assert/strict";
import test from "node:test";
import { filterNavigationItems, isNavigationShortcut, readSidebarCollapsed, SIDEBAR_STORAGE_KEY, stepSelection, writeSidebarCollapsed } from "./navigation-search.ts";
const items = [{ key: "dashboard", label: "仪表盘" }, { key: "keys", label: "接入 Key" }, { key: "providers", label: "供应商" }];
test("navigation search preserves order and matches localized labels and stable identifiers", () => {
  assert.deepEqual(filterNavigationItems(items, ""), items);
  assert.deepEqual(filterNavigationItems(items, "供应"), [items[2]]);
  assert.deepEqual(filterNavigationItems(items, " ＫＥＹ "), [items[1]]);
  assert.deepEqual(filterNavigationItems(items, "keys 接入"), [items[1]]);
  assert.deepEqual(filterNavigationItems(items, "unknown"), []);
  assert.notEqual(filterNavigationItems(items, ""), items);
});
test("keyboard selection wraps and handles empty search results", () => {
  assert.equal(stepSelection(0, -1, 3), 2);
  assert.equal(stepSelection(2, 1, 3), 0);
  assert.equal(stepSelection(0, 1, 0), -1);
  assert.equal(stepSelection(-1, 1, 3), 0);
  assert.equal(stepSelection(0, -1, 1), 0);
});
test("command shortcut supports Mac/Windows without swallowing IME or key repeat", () => {
  const base = { key: "k", metaKey: false, ctrlKey: false, altKey: false, isComposing: false, repeat: false };
  assert.equal(isNavigationShortcut(base), false);
  assert.equal(isNavigationShortcut({ ...base, ctrlKey: true }), true);
  assert.equal(isNavigationShortcut({ ...base, metaKey: true, key: "K" }), true);
  for (const overrides of [{ altKey: true }, { isComposing: true }, { repeat: true }, { key: "j" }]) {
    assert.equal(isNavigationShortcut({ ...base, ctrlKey: true, ...overrides }), false);
  }
});
test("sidebar preference fails safely and does not share the theme storage key", () => {
  assert.equal(readSidebarCollapsed(null), false);
  assert.equal(readSidebarCollapsed({ getItem: () => "true" }), true);
  assert.equal(readSidebarCollapsed({ getItem: () => "junk" }), false);
  assert.equal(readSidebarCollapsed({ getItem: () => { throw new Error("blocked"); } }), false);
  const saved = new Map<string, string>();
  writeSidebarCollapsed({ setItem: (key, value) => { saved.set(key, value); } }, true);
  assert.equal(saved.get(SIDEBAR_STORAGE_KEY), "true");
  assert.doesNotThrow(() => writeSidebarCollapsed({ setItem: () => { throw new Error("blocked"); } }, false));
});
