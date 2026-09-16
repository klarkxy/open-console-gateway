import assert from "node:assert/strict";
import test from "node:test";
import { ref, type VNode } from "vue";
import type { Account, ForwardLog, GatewayLog } from "../api/dashboard.ts";
import { t } from "../i18n/index.ts";
import { formatCost } from "../utils/format.ts";
import {
  renderDiagnostic,
  renderForwardDetail,
  renderRequestId,
  shortRequestId,
  type LogsColumnContext,
} from "./logs-columns.ts";

function forwardRow(overrides: Partial<ForwardLog> = {}): ForwardLog {
  return {
    id: 1,
    timestamp: "2026-07-19T12:00:00Z",
    model: "claude-sonnet",
    requested_model: null,
    resolved_alias: null,
    upstream_model: null,
    account_id: "acc-1",
    account_name: "Primary",
    client_key_id: null,
    client_key_name: null,
    route_account_id: null,
    provider_id: null,
    credential_account_id: null,
    raw_cost_usd: null,
    quota_debit: null,
    effective_paid_cost_usd: null,
    native_cost_value: null,
    native_cost_unit: null,
    native_cost_currency: null,
    status: "success",
    http_status: 200,
    route: "",
    prompt_tokens: 0,
    completion_tokens: 0,
    cached_tokens: 0,
    cache_creation_tokens: 0,
    cost: null,
    cost_state: "unpriced",
    pricing_revision_id: null,
    quota_multiplier: null,
    local_adjustment_multiplier: null,
    service_tier: null,
    error_message: null,
    request_id: null,
    attempt: null,
    error_source: null,
    error_stage: null,
    duration_ms: null,
    diagnostic: null,
    ...overrides,
  };
}

function gatewayRow(overrides: Partial<GatewayLog> = {}): GatewayLog {
  return {
    id: 1,
    level: "error",
    category: "upstream",
    message: "",
    created_at: "2026-07-19T12:00:00Z",
    request_id: null,
    attempt: null,
    error_source: null,
    error_stage: null,
    duration_ms: null,
    diagnostic: null,
    ...overrides,
  };
}

function makeContext(accounts: Account[] = []) {
  const calls = {
    copied: [] as Array<{ target: string; value: string; label: string }>,
    focused: [] as string[],
  };
  const ctx: LogsColumnContext = {
    copiedTarget: ref(null),
    copyText: (target, value, label) => { calls.copied.push({ target, value, label }); },
    focusRequestChain: (requestId) => { calls.focused.push(requestId); },
    accounts: ref(accounts),
  };
  return { ctx, calls };
}

function childrenOf(vnode: VNode): VNode[] {
  return Array.isArray(vnode.children) ? vnode.children as VNode[] : [];
}

function* walk(vnode: VNode): Generator<VNode> {
  if (!vnode) return;
  yield vnode;
  for (const child of childrenOf(vnode)) yield* walk(child);
}

function vnodeText(vnode: VNode): string {
  if (!vnode) return "";
  if (typeof vnode.children === "string") return vnode.children;
  return childrenOf(vnode).map(vnodeText).join("");
}

/** Value of the `<dd>` following the `<dt>` whose text matches `label`. */
function descriptionValue(root: VNode, label: string): string | null {
  for (const node of walk(root)) {
    if (node.type !== "dl") continue;
    const pairs = childrenOf(node);
    for (let i = 0; i + 1 < pairs.length; i += 2) {
      if (vnodeText(pairs[i]) === label) return vnodeText(pairs[i + 1]);
    }
  }
  return null;
}

function findByClass(root: VNode, className: string): VNode | null {
  for (const node of walk(root)) {
    if (node.props?.class === className) return node;
  }
  return null;
}

function slotVNode(vnode: VNode, name: string): VNode {
  const slots = (vnode.children ?? {}) as Record<string, () => VNode>;
  return slots[name]();
}

test("shortRequestId keeps short ids intact and truncates long ones", () => {
  assert.equal(shortRequestId("short-request"), "short-request");
  assert.equal(shortRequestId("x".repeat(18)), "x".repeat(18));
  const long = "abcdefghijklmnopqrstu";
  assert.equal(long.length, 21);
  assert.equal(shortRequestId(long), "abcdefghijklm…rstu");
  assert.equal(shortRequestId(long).length, 18);
});

test("renderRequestId renders a placeholder when the row has no request id", () => {
  const { ctx } = makeContext();
  assert.equal(renderRequestId(gatewayRow({ request_id: null }), ctx), "—");
  assert.equal(renderRequestId(forwardRow({ request_id: "" }), ctx), "—");
});

test("renderRequestId focuses the chain from the link and copies from the icon button", () => {
  const { ctx, calls } = makeContext();
  const requestId = "ocg-req-0001-abcdefghijklmnopqrstuvwxyz";
  const vnode = renderRequestId(gatewayRow({ id: 42, request_id: requestId }), ctx) as VNode;

  assert.equal(vnode.type, "div");
  assert.equal(vnode.props?.class, "request-id-cell");
  const [linkButton, copyButton] = childrenOf(vnode);

  const code = slotVNode(linkButton, "default");
  assert.equal(code.type, "code");
  assert.equal(code.children, shortRequestId(requestId));
  linkButton.props?.onClick();
  assert.deepEqual(calls.focused, [requestId]);

  copyButton.props?.onClick();
  assert.equal(calls.copied.length, 1);
  assert.equal(calls.copied[0]?.target, "request-id-42");
  assert.equal(calls.copied[0]?.value, requestId);
  assert.equal(typeof calls.copied[0]?.label, "string");

  const iconBefore = slotVNode(copyButton, "icon");
  ctx.copiedTarget.value = "request-id-42";
  const rerendered = renderRequestId(gatewayRow({ id: 42, request_id: requestId }), ctx) as VNode;
  const iconAfter = slotVNode(childrenOf(rerendered)[1] as VNode, "icon");
  assert.notEqual(iconAfter.props?.component, iconBefore.props?.component);
});

test("renderForwardDetail omits the request block without a request id and wires the filter button", () => {
  const { ctx, calls } = makeContext();
  const withoutId = renderForwardDetail(forwardRow(), ctx) as VNode;
  assert.equal(withoutId.type, "div");
  assert.equal(withoutId.props?.class, "diagnostic-detail");
  assert.equal(childrenOf(withoutId)[0], null);

  const withId = renderForwardDetail(forwardRow({ id: 7, request_id: "req-7" }), ctx) as VNode;
  const requestBlock = childrenOf(withId)[0] as VNode;
  const cell = findByClass(requestBlock, "request-id-cell");
  assert.ok(cell);
  const buttons = childrenOf(cell).filter((child) => child.type !== "code");
  buttons[1]?.props?.onClick();
  assert.deepEqual(calls.focused, ["req-7"]);
  buttons[0]?.props?.onClick();
  assert.equal(calls.copied[0]?.target, "request-id-7");
});

test("renderForwardDetail resolves provider accounts through the injected account list", () => {
  const accounts = [
    { id: "acc-1", name: "Primary" },
    { id: "acc-2", name: "Fallback" },
  ] as Account[];
  const { ctx } = makeContext(accounts);
  const row = forwardRow({
    route_account_id: "acc-2",
    credential_account_id: "acc-missing",
    raw_cost_usd: 0.5,
  });
  const vnode = renderForwardDetail(row, ctx) as VNode;
  assert.equal(descriptionValue(vnode, t("路由账号")), "Fallback");
  assert.equal(descriptionValue(vnode, t("凭证账号")), "acc-missing");
  assert.equal(descriptionValue(vnode, t("原始供应商成本")), formatCost(0.5, 5));
});

test("renderForwardDetail lists the frozen native estimate only for custom-provider rows", () => {
  const { ctx } = makeContext();
  const custom = forwardRow({
    provider_id: "custom",
    native_cost_value: 12,
    native_cost_currency: "USD",
    native_cost_unit: "token",
    pricing_revision_id: "rev-9",
  });
  const customTree = renderForwardDetail(custom, ctx) as VNode;
  assert.equal(descriptionValue(customTree, t("计价来源（冻结）")), "rev-9");
  assert.equal(descriptionValue(customTree, t("实际平台扣减")), t("未知"));

  const other = forwardRow({ provider_id: "opencode", native_cost_value: 12, pricing_revision_id: "rev-9" });
  const otherTree = renderForwardDetail(other, ctx) as VNode;
  assert.equal(descriptionValue(otherTree, t("计价来源（冻结）")), null);
});

test("renderDiagnostic stringifies upstream detail blocks and keeps row fields", () => {
  const headers = { "x-trace-id": "trace-123" };
  const row = forwardRow({
    route: "edge-cache",
    error_message: "boom",
    error_source: null,
    error_stage: null,
    diagnostic: {
      client_format: "chat.completions",
      upstream_format: "messages",
      attempt: 2,
      duration_ms: 300,
      upstream_wait_ms: 88,
      upstream_headers: headers,
      retry_action: "",
    },
  });
  const vnode = renderDiagnostic(row) as VNode;

  assert.equal(descriptionValue(vnode, t("协议路径")), "chat.completions → messages");
  assert.equal(descriptionValue(vnode, t("尝试次数")), "2");
  assert.equal(descriptionValue(vnode, t("耗时")), "300 ms");
  assert.equal(descriptionValue(vnode, t("上游响应头耗时")), "88 ms");
  assert.equal(descriptionValue(vnode, t("路由")), "edge-cache");
  // Null/empty diagnostics are dropped instead of rendering empty rows.
  assert.equal(descriptionValue(vnode, t("重试动作")), null);

  const errorPre = findByClass(vnode, "error-text");
  assert.ok(errorPre);
  assert.equal(vnodeText(errorPre), "boom");

  const jsonPre = findByClass(vnode, "diagnostic-json");
  assert.ok(jsonPre);
  assert.equal(vnodeText(jsonPre), JSON.stringify(headers, null, 2));
});

test("renderDiagnostic prefers the row duration and localizes known route legs", () => {
  const row = forwardRow({
    route: "auto",
    duration_ms: 250,
    diagnostic: { client_format: "messages", duration_ms: 999 },
  });
  const vnode = renderDiagnostic(row) as VNode;
  assert.equal(descriptionValue(vnode, t("耗时")), "250 ms");
  assert.equal(descriptionValue(vnode, t("路由")), t("自动"));

  const gateway = renderDiagnostic(gatewayRow()) as VNode;
  assert.equal(descriptionValue(gateway, t("路由")), null);
});
