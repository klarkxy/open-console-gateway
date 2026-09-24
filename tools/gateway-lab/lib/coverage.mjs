export const EVIDENCE = Object.freeze({
  GATEWAY: "gateway_black_box",
  RUST: "rust_integration",
  LAB: "lab_fixture",
  LIVE: "live_remote",
});

const CLIENTS = ["chat", "responses", "messages"];
const SLOTS = ["chat", "responses", "messages"];
const MODES = ["json", "sse"];

export const MATRIX_SCENARIO_IDS = CLIENTS.flatMap((client) =>
  SLOTS.flatMap((slot) => MODES.map((mode) => `gw.matrix.${client}.${slot}.${mode}`)),
);
export const GEMINI_SCENARIO_IDS = SLOTS.flatMap((slot) => MODES.map((mode) => `gw.gemini.${slot}.${mode}`));

export const REQUIREMENTS = [
  { id: "protocol-matrix-4x3x2", title: "4 client formats x 3 upstream protocols x JSON/SSE", evidenceKinds: [EVIDENCE.GATEWAY], scenarioIds: [...MATRIX_SCENARIO_IDS, ...GEMINI_SCENARIO_IDS] },
  { id: "gemini-all-upstreams", title: "Gemini JSON and SSE through Chat, Responses, and Messages", evidenceKinds: [EVIDENCE.GATEWAY], scenarioIds: GEMINI_SCENARIO_IDS },
  { id: "model-catalog-per-endpoint", title: "Per-endpoint /v1/models catalogs", evidenceKinds: [EVIDENCE.GATEWAY, EVIDENCE.LAB], scenarioIds: ["lab.models.chat", "lab.models.responses", "lab.models.messages"], requireAll: true },
  { id: "model-catalog-ambiguity", title: "Model/catalog ambiguity", evidenceKinds: [EVIDENCE.GATEWAY], scenarioIds: ["gw.ambiguity"] },
  { id: "hidden-and-raw-ids", title: "Hidden and raw IDs", evidenceKinds: [EVIDENCE.GATEWAY], scenarioIds: ["gw.hidden-public", "gw.raw-id"] },
  { id: "routing-strict-priority", title: "Routing strict priority", evidenceKinds: [EVIDENCE.GATEWAY], scenarioIds: ["gw.routing.strict"] },
  { id: "routing-reorder", title: "Routing reorder", evidenceKinds: [EVIDENCE.GATEWAY], scenarioIds: ["gw.routing.reorder"] },
  { id: "routing-round-robin", title: "Routing round-robin", evidenceKinds: [EVIDENCE.GATEWAY], scenarioIds: ["gw.routing.rr"] },
  { id: "routing-sticky", title: "Routing sticky-global", evidenceKinds: [EVIDENCE.GATEWAY], scenarioIds: ["gw.routing.sticky"] },
  { id: "routing-disable", title: "Disabled account skip", evidenceKinds: [EVIDENCE.GATEWAY], scenarioIds: ["gw.routing.disable"] },
  { id: "routing-binding-scope", title: "Binding modelScope skip", evidenceKinds: [EVIDENCE.GATEWAY], scenarioIds: ["gw.routing.scope"] },
  { id: "routing-shared-quota", title: "Shared quota sibling skip", evidenceKinds: [EVIDENCE.GATEWAY], scenarioIds: ["gw.routing.shared-quota"] },
  { id: "failure-cooldown-429", title: "Failure cooldown / 429 fallthrough", evidenceKinds: [EVIDENCE.GATEWAY], scenarioIds: ["gw.fail.429"] },
  { id: "failure-non-replay-5xx", title: "Non-replay on 5xx", evidenceKinds: [EVIDENCE.GATEWAY], scenarioIds: ["gw.fail.503"] },
  { id: "failure-non-replay-drop", title: "Non-replay on post-connect drop", evidenceKinds: [EVIDENCE.GATEWAY], scenarioIds: ["gw.fail.drop"] },
  { id: "failure-recovery", title: "Failure recovery after cooldown expiry", evidenceKinds: [EVIDENCE.GATEWAY], scenarioIds: ["gw.fail.cooldown-recovery"] },
  { id: "stream-utf8", title: "Stream UTF-8", evidenceKinds: [EVIDENCE.GATEWAY], scenarioIds: ["gw.stream.utf8"] },
  { id: "stream-missing-end", title: "Missing stream end", evidenceKinds: [EVIDENCE.GATEWAY], scenarioIds: ["gw.stream.missing-end"] },
  { id: "stream-interrupt", title: "Stream interrupt", evidenceKinds: [EVIDENCE.GATEWAY], scenarioIds: ["gw.stream.interrupt"] },
  { id: "stream-cancel", title: "Client cancel", evidenceKinds: [EVIDENCE.GATEWAY], scenarioIds: ["gw.stream.cancel"] },
  { id: "stream-timeout", title: "Timeout", evidenceKinds: [EVIDENCE.GATEWAY], scenarioIds: ["gw.stream.timeout"] },
  { id: "key-rotation", title: "Key rotation", evidenceKinds: [EVIDENCE.GATEWAY], scenarioIds: ["gw.key.rotate"] },
  { id: "key-revoke", title: "Key / grant revoke", evidenceKinds: [EVIDENCE.GATEWAY], scenarioIds: ["gw.key.revoke.chat", "gw.key.revoke.responses", "gw.key.revoke.messages"] },
  { id: "cas-conflict", title: "CAS conflict", evidenceKinds: [EVIDENCE.GATEWAY], scenarioIds: ["gw.cas.conflict"] },
  { id: "cas-idempotency", title: "Onboarding idempotency", evidenceKinds: [EVIDENCE.GATEWAY], scenarioIds: ["gw.cas.idempotency"] },
  { id: "catalog-refresh-failure", title: "Catalog refresh failure", evidenceKinds: [EVIDENCE.GATEWAY, EVIDENCE.RUST], scenarioIds: ["gw.catalog.refresh-fail", "rust.catalog.refresh-fail"] },
  { id: "restart-persistence", title: "Restart persistence", evidenceKinds: [EVIDENCE.GATEWAY], scenarioIds: ["gw.restart"] },
  { id: "import-export", title: "Import/export", evidenceKinds: [EVIDENCE.GATEWAY], scenarioIds: ["gw.transfer"] },
  { id: "usage-exact", title: "Exact usage", evidenceKinds: [EVIDENCE.GATEWAY], scenarioIds: ["gw.usage.exact"] },
  { id: "usage-missing", title: "Missing usage", evidenceKinds: [EVIDENCE.GATEWAY], scenarioIds: ["gw.usage.missing"] },
  { id: "price-free-unpriced", title: "Price, free, and unpriced", evidenceKinds: [EVIDENCE.GATEWAY, EVIDENCE.RUST], scenarioIds: ["gw.unpriced", "rust.price-go", "rust.free-zen"], requireAll: true },
  { id: "quota-no-disable", title: "Quota does not disable", evidenceKinds: [EVIDENCE.GATEWAY, EVIDENCE.RUST], scenarioIds: ["gw.quota-no-disable", "rust.quota-no-disable"] },
  { id: "proxy-isolation", title: "Proxy isolation", evidenceKinds: [EVIDENCE.GATEWAY, EVIDENCE.RUST], scenarioIds: ["gw.proxy.list", "rust.proxy-list"] },
  { id: "auth-isolation", title: "Auth isolation", evidenceKinds: [EVIDENCE.GATEWAY, EVIDENCE.LAB], scenarioIds: ["lab.models.reject-wrong-key", "gw.auth.isolation"], requireAll: true },
  { id: "tool-history", title: "Tool declaration, call, fragment, result, history", evidenceKinds: [EVIDENCE.GATEWAY], scenarioIds: ["gw.tool.chat.json", "gw.tool.chat.sse", "gw.tool.responses.json", "gw.tool.messages.json", "gw.tool.chat.messages"] },
  { id: "explicit-unsupported", title: "Explicit unsupported features", evidenceKinds: [EVIDENCE.GATEWAY], scenarioIds: ["gw.unsupported.responses-store", "gw.unsupported.responses-previous", "gw.unsupported.responses-conversation", "gw.unsupported.responses-background", "gw.unsupported.gemini-op"] },
];

function evidenceOk(row, allowed) {
  if (row.status !== "PASS") return false;
  if (!allowed.includes(row.evidenceKind)) return false;
  if (row.evidenceKind === EVIDENCE.LAB && allowed.length === 1 && allowed[0] === EVIDENCE.GATEWAY) return false;
  return true;
}

export function applyCoverageChecklist(collector) {
  const checklist = [];
  for (const item of REQUIREMENTS) {
    const hits = collector.results.filter((row) => item.scenarioIds.includes(row.scenarioId) && evidenceOk(row, item.evidenceKinds));
    const missing = item.scenarioIds.filter((id) => !hits.some((row) => row.scenarioId === id));
    const complete = item.requireAll || item.evidenceKinds.length === 1
      ? missing.length === 0
      : hits.length > 0;
    if (!complete) {
      collector.notRun(`coverage:${item.id}`, `missing ${missing.join(",") || item.id}`, {
        scenarioId: `coverage.${item.id}`,
        evidenceKind: item.evidenceKinds[0],
        requirement: item.id,
      });
      checklist.push({ id: item.id, title: item.title, status: "NOT_RUN", missing, hits: hits.map((row) => row.scenarioId) });
      continue;
    }
    checklist.push({ id: item.id, title: item.title, status: "PASS", scenarioIds: hits.map((row) => row.scenarioId), evidenceKinds: [...new Set(hits.map((row) => row.evidenceKind))] });
  }
  return { checklist };
}
