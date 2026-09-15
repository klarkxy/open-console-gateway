import { createRenderer } from "vue";

/**
 * Shared Vue custom-renderer + timer harness used by CPA and Applications
 * component tests. Insert detaches a node before attaching it so same-parent
 * and cross-parent moves keep a single parent pointer.
 */

export type HostNode = {
  children: HostNode[];
  parent?: HostNode;
  props: Record<string, unknown>;
  text?: string;
  type: string;
};

export type HostTimerKind = "timeout" | "interval";

export type HostTimer = {
  fn: () => void;
  kind: HostTimerKind;
};

export type TestWindow = {
  addEventListener(): void;
  removeEventListener(): void;
  open(url?: unknown): void;
  clearInterval(id?: number): void;
  clearTimeout(id?: number): void;
  setInterval(fn: () => void, delay?: number): number;
  setTimeout(fn: () => void, delay?: number): number;
  history: { replaceState(): void };
  location: { href: string; search: string; pathname: string };
  __timers: Map<number, HostTimer>;
  __opened: string[];
};

export function createHostNode(type: string, extra: Partial<HostNode> = {}): HostNode {
  return { children: [], props: {}, type, ...extra };
}

export function insertHostNode(
  child: HostNode,
  parent: HostNode,
  anchor?: HostNode | null,
): void {
  if (child.parent) {
    const siblings = child.parent.children;
    const current = siblings.indexOf(child);
    if (current >= 0) siblings.splice(current, 1);
  }
  child.parent = parent;
  if (anchor) {
    const index = parent.children.indexOf(anchor);
    if (index >= 0) {
      parent.children.splice(index, 0, child);
      return;
    }
  }
  parent.children.push(child);
}

export function removeHostNode(node: HostNode): void {
  if (!node.parent) return;
  const index = node.parent.children.indexOf(node);
  if (index >= 0) node.parent.children.splice(index, 1);
  node.parent = undefined;
}

export function nextHostSibling(node: HostNode): HostNode | null {
  if (!node.parent) return null;
  const index = node.parent.children.indexOf(node);
  return index >= 0 ? node.parent.children[index + 1] ?? null : null;
}

export function parentHostNode(node: HostNode): HostNode | null {
  return node.parent ?? null;
}

export function createVueHostRenderer() {
  return createRenderer<HostNode, HostNode>({
    createComment: (text) => createHostNode("comment", { text }),
    createElement: (type) => createHostNode(type),
    createText: (text) => createHostNode("text", { text }),
    insert: (child, parent, anchor) => insertHostNode(child, parent, anchor),
    nextSibling: nextHostSibling,
    parentNode: parentHostNode,
    patchProp: (node, key, _previous, next) => {
      node.props[key] = next;
    },
    remove: removeHostNode,
    setElementText: (node, text) => {
      for (const child of node.children) child.parent = undefined;
      node.children = [];
      node.text = text;
    },
    setText: (node, text) => {
      node.text = text;
    },
  });
}

export async function settle(ticks = 12): Promise<void> {
  for (let index = 0; index < ticks; index += 1) await Promise.resolve();
}

export function createTestWindow(options: {
  href?: string;
  search?: string;
  pathname?: string;
} = {}): TestWindow {
  const timers = new Map<number, HostTimer>();
  const opened: string[] = [];
  let next = 1;
  const href = options.href ?? "http://127.0.0.1/dashboard/";
  const search = options.search ?? "";
  const pathname = options.pathname ?? "/dashboard/";
  const clear = (id?: number) => {
    if (typeof id === "number") timers.delete(id);
  };
  return {
    addEventListener() {},
    removeEventListener() {},
    open(url?: unknown) {
      opened.push(String(url ?? ""));
    },
    clearInterval: clear,
    clearTimeout: clear,
    setTimeout(fn: () => void) {
      const id = next++;
      timers.set(id, { fn, kind: "timeout" });
      return id;
    },
    setInterval(fn: () => void) {
      const id = next++;
      timers.set(id, { fn, kind: "interval" });
      return id;
    },
    history: { replaceState() {} },
    location: { href, search, pathname },
    __timers: timers,
    __opened: opened,
  };
}

export function installTestWindow(
  testWindow: TestWindow | Parameters<typeof createTestWindow>[0] = createTestWindow(),
): TestWindow {
  const resolved = "__timers" in testWindow ? testWindow : createTestWindow(testWindow);
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    writable: true,
    value: resolved,
  });
  return resolved;
}

export function clearAllTimers(testWindow: Pick<TestWindow, "__timers">): void {
  testWindow.__timers.clear();
}

export async function fireTimers(testWindow: Pick<TestWindow, "__timers">): Promise<void> {
  const snapshot = [...testWindow.__timers.entries()];
  for (const [id, timer] of snapshot) {
    if (!testWindow.__timers.has(id)) continue;
    if (timer.kind === "timeout") testWindow.__timers.delete(id);
    timer.fn();
  }
  await settle();
}

export function deferred<T>(): {
  promise: Promise<T>;
  resolve: (value: T) => void;
  reject: (error: unknown) => void;
} {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((nextResolve, nextReject) => {
    resolve = nextResolve;
    reject = nextReject;
  });
  return { promise, resolve, reject };
}

export function text(node: HostNode): string {
  return `${node.text ?? ""}${node.children.map(text).join("")}`;
}

export function walkHostNodes(root: HostNode): HostNode[] {
  return root.children.flatMap(function walk(node: HostNode): HostNode[] {
    return [node, ...node.children.flatMap(walk)];
  });
}

export function button(root: HostNode, label: string): HostNode {
  const found = walkHostNodes(root).find(
    (node) => node.type === "button" && text(node).trim() === label,
  );
  if (!found) throw new Error(`button ${label} should render`);
  return found;
}
