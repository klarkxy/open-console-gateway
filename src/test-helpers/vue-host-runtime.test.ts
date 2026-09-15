import assert from "node:assert/strict";
import test from "node:test";
import {
  button,
  clearAllTimers,
  createHostNode,
  createTestWindow,
  fireTimers,
  insertHostNode,
  nextHostSibling,
  parentHostNode,
  removeHostNode,
  settle,
  text,
  walkHostNodes,
} from "./vue-host-runtime.ts";

test("insert moves a node under the same parent without duplicating it", () => {
  const parent = createHostNode("parent");
  const a = createHostNode("a");
  const b = createHostNode("b");
  const c = createHostNode("c");
  insertHostNode(a, parent);
  insertHostNode(b, parent);
  insertHostNode(c, parent);
  insertHostNode(a, parent, c);
  assert.deepEqual(parent.children.map((node) => node.type), ["b", "a", "c"]);
  assert.equal(a.parent, parent);
  assert.equal(parent.children.filter((node) => node === a).length, 1);
  assert.equal(nextHostSibling(a), c);
  assert.equal(nextHostSibling(c), null);
});

test("insert moves a node across parents and updates the parent pointer", () => {
  const left = createHostNode("left");
  const right = createHostNode("right");
  const child = createHostNode("child");
  const anchor = createHostNode("anchor");
  insertHostNode(child, left);
  insertHostNode(anchor, right);
  insertHostNode(child, right, anchor);
  assert.deepEqual(left.children, []);
  assert.deepEqual(right.children.map((node) => node.type), ["child", "anchor"]);
  assert.equal(parentHostNode(child), right);
  assert.equal(child.parent, right);
});

test("insert without an anchor appends, and a missing anchor still appends", () => {
  const parent = createHostNode("parent");
  const first = createHostNode("first");
  const second = createHostNode("second");
  const missing = createHostNode("missing");
  insertHostNode(first, parent);
  insertHostNode(second, parent, missing);
  assert.deepEqual(parent.children.map((node) => node.type), ["first", "second"]);
});

test("remove detaches a node and clears its parent pointer", () => {
  const parent = createHostNode("parent");
  const child = createHostNode("child");
  insertHostNode(child, parent);
  removeHostNode(child);
  assert.deepEqual(parent.children, []);
  assert.equal(child.parent, undefined);
  removeHostNode(child);
});

test("setTimeout fires once and setInterval re-arms until cancelled", async () => {
  const testWindow = createTestWindow();
  let timeouts = 0;
  let intervals = 0;
  testWindow.setTimeout(() => {
    timeouts += 1;
  });
  const intervalId = testWindow.setInterval(() => {
    intervals += 1;
  });
  await fireTimers(testWindow);
  assert.equal(timeouts, 1);
  assert.equal(intervals, 1);
  assert.equal(testWindow.__timers.size, 1);
  await fireTimers(testWindow);
  assert.equal(timeouts, 1);
  assert.equal(intervals, 2);
  testWindow.clearInterval(intervalId);
  await fireTimers(testWindow);
  assert.equal(intervals, 2);
  assert.equal(testWindow.__timers.size, 0);
});

test("an interval that clears itself does not re-arm", async () => {
  const testWindow = createTestWindow();
  let fires = 0;
  const id = testWindow.setInterval(() => {
    fires += 1;
    testWindow.clearInterval(id);
  });
  await fireTimers(testWindow);
  assert.equal(fires, 1);
  assert.equal(testWindow.__timers.size, 0);
  await fireTimers(testWindow);
  assert.equal(fires, 1);
});

test("a timeout that clears a later timeout prevents it from firing in the same batch", async () => {
  const testWindow = createTestWindow();
  const fired: string[] = [];
  let later = 0;
  testWindow.setTimeout(() => {
    fired.push("first");
    testWindow.clearTimeout(later);
  });
  later = testWindow.setTimeout(() => {
    fired.push("later");
  });
  await fireTimers(testWindow);
  assert.deepEqual(fired, ["first"]);
});

test("clearTimeout and clearAllTimers prevent later fires", async () => {
  const testWindow = createTestWindow();
  let fired = 0;
  const timeoutId = testWindow.setTimeout(() => {
    fired += 1;
  });
  testWindow.setInterval(() => {
    fired += 1;
  });
  testWindow.clearTimeout(timeoutId);
  clearAllTimers(testWindow);
  await fireTimers(testWindow);
  assert.equal(fired, 0);
  await settle();
});

test("text, walk, and button helpers read the host tree", () => {
  const root = createHostNode("root");
  const buttonNode = createHostNode("button");
  const label = createHostNode("text", { text: "确认安装" });
  insertHostNode(buttonNode, root);
  insertHostNode(label, buttonNode);
  assert.equal(text(root), "确认安装");
  assert.equal(walkHostNodes(root).length, 2);
  assert.equal(button(root, "确认安装"), buttonNode);
  assert.throws(() => button(root, "missing"));
});
