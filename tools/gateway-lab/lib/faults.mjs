export const SCENARIOS = Object.freeze({
  success: [{ kind: "success" }],
  http_429: [{ kind: "http", status: 429, body: { error: { message: "Resets in 5 minutes", type: "rate_limit_error" } } }],
  http_503: [{ kind: "http", status: 503, body: { error: { message: "unavailable", type: "api_error" } } }],
  delay: [{ kind: "delay", ms: 250 }],
  malformed_json: [{ kind: "malformed_json" }],
  missing_usage: [{ kind: "missing_usage" }],
  sse_split: [{ kind: "sse_split" }],
  sse_fragment: [{ kind: "sse_fragment" }],
  missing_end: [{ kind: "missing_end" }],
  stream_interrupt: [{ kind: "stream_interrupt" }],
  drop: [{ kind: "drop" }],
  fail_then_success: [
    { kind: "http", status: 500, body: { error: { message: "first_fail", type: "api_error" } } },
    { kind: "success" },
  ],
});

export function isolationKey({ endpointId, keyFingerprint, model, scenario = "" }) {
  return [endpointId || "", keyFingerprint || "", model || "", scenario || ""].join("|");
}

export function normalizeScript(item) {
  if (!item || typeof item !== "object") return { kind: "success" };
  const kind = [
    "http",
    "drop",
    "success",
    "delay",
    "malformed_json",
    "missing_usage",
    "sse_split",
    "sse_fragment",
    "missing_end",
    "stream_interrupt",
  ].includes(item.kind)
    ? item.kind
    : "success";
  if (kind === "http") {
    const status = Number.parseInt(item.status, 10);
    return {
      kind: "http",
      status: Number.isFinite(status) ? status : 500,
      body: item.body ?? { error: { message: item.message || "scripted_http", type: "api_error" } },
    };
  }
  if (kind === "delay") {
    const ms = Number.parseInt(item.ms, 10);
    return { kind: "delay", ms: Number.isFinite(ms) ? ms : 250 };
  }
  if (kind === "success" && Array.isArray(item.catalog)) return { kind: "success", catalog: item.catalog };
  return { kind };
}

export function scenarioQueue(name) {
  const queue = SCENARIOS[name];
  if (!queue) throw new Error(`unknown scenario ${name}`);
  return queue.map(normalizeScript);
}
