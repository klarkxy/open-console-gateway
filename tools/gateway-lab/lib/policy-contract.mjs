import { LIVE_MODEL, MARKER } from "./common.mjs";

export const POLICY_HELP = `Temporary unavailability policy verify — Gateway Lab HTTP harness

Usage:
  node tools/gateway-lab/policy-verify.mjs --cli <ocg-manager-cli> --suite local|live --artifacts <dir>
  node tools/gateway-lab/policy-verify.mjs --cli <ocg-manager-cli> --suite live --lab-runtime <runtime.json> --artifacts <dir>
  node tools/gateway-lab/policy-verify.mjs --help

local  creates an in-process lab simulator. remoteCalls must stay 0.
live   attaches the coordinator lab at --lab-runtime (loopback URLs and
       synthetic lab Keys only). Success traffic is real minimax-m3 on that
       lab; faults are injected locally. A local fixture is not live evidence.
       This tool does not read mmx config, user directories, or environment Keys.

Reports: PASS / FAIL / UNSUPPORTED / NOT_RUN. HTTP/live required rows: FAIL exits 1;
unexecuted required HTTP/live exits 2. Companion Rust rows are documented separately
and do not drive this runner's exit code. Cleanup stops only processes this runner
started; an attached lab is preserved and its live-arm flag is restored.
`;

export const POLICY_PATHS = Object.freeze({
  config: "/dashboard/api/v4/routing/temporary-unavailability",
  restrictions: "/dashboard/api/v4/routing/temporary-unavailability/restrictions",
  clear: (id) =>
    `/dashboard/api/v4/routing/temporary-unavailability/restrictions/${encodeURIComponent(id)}/clear`,
});

export const BUILTIN_GOAT_ID = "builtin.goat.credits_rejection";
export const HTTP_BACKOFF = Object.freeze({ initialSeconds: 1, maxSeconds: 1 });
export const LIVE_BACKOFF = Object.freeze({ initialSeconds: 45, maxSeconds: 45 });
export const LIVE_EXPIRY_TIMEOUT_MS = 90000;
export const LIVE_MAX_REMOTE = 6;
export const HTTP_INFLIGHT_DELAY_MS = 800;
export const LIVE_OUTBOUND_MODEL = LIVE_MODEL;
export const POLICY_PROMPT = MARKER;
export const PHASE = Object.freeze({
  SIMULATE: "simulate",
  LIVE: "live",
});

export const RESPONSIBILITY = Object.freeze({
  HTTP: "requiredHttp",
  LIVE: "requiredLive",
  RUST: "companionRust",
  HARNESS: "harness",
});

export const EVIDENCE = Object.freeze({
  GATEWAY: "gateway_black_box",
  RUST: "rust_integration",
  LAB: "lab_fixture",
  LIVE: "live_remote",
});

export const SCENARIO = Object.freeze({
  CLI_HELP: "policy.cli.help",
  API_PROBE: "policy.api.probe",
  CAS_CONFLICT: "policy.cas.conflict",
  AUTH_DENIAL: "policy.auth.denial",
  VALIDATION: "policy.api.validation",
  CUSTOM_400: "policy.http.custom-400",
  CUSTOM_400_RESPONSES: "policy.http.custom-400.responses",
  CUSTOM_400_MESSAGES: "policy.http.custom-400.messages",
  OVERRIDE: "policy.http.override-disable",
  CLEAR: "policy.http.clear",
  ISOLATION_MODEL: "policy.http.isolation.model",
  ISOLATION_CREDENTIAL: "policy.http.isolation.credential",
  SINGLE_FLIGHT: "policy.http.single-flight",
  LATE_CHANGE: "policy.http.late-rule-change",
  ALL_WAITING: "policy.http.all-waiting",
  RESTART: "policy.http.restart",
  COMPLETE_JSON: "policy.http.complete-json",
  COMPLETE_SSE: "policy.http.complete-sse",
  CANCEL: "policy.http.cancel",
  LOCAL_REMOTE_ZERO: "policy.local.remoteCalls=0",
  LIVE_NOT_FAKE: "policy.live.not-local-fixture",
  LIVE_FAULT_ZERO_REMOTE: "policy.live.fault-local",
  LIVE_POLICY_ABBA: "policy.live.custom400-ABBA",
  LIVE_CHARLIE: "policy.live.charlie.minimax-m3",
  GOAT_ABB: "policy.goat.A-B-B",
  GOAT_CAPACITY: "policy.goat.capacity-32",
  GOAT_CLOCK: "policy.goat.clock",
  STALE_SUCCESS: "policy.rust.stale-probe-success-does-not-clear-new-restriction",
});

export const CUSTOM_400_ERROR = Object.freeze({
  message: "temporary custom unavailable",
  type: "api_error",
  code: "TEMP_UNAVAIL",
});

export function custom400Body() {
  return { error: { ...CUSTOM_400_ERROR } };
}

export function nestedEcho400Body() {
  return {
    choices: [
      {
        message: {
          role: "assistant",
          content: `${CUSTOM_400_ERROR.code} ${CUSTOM_400_ERROR.message}`,
        },
      },
    ],
  };
}

export function statusOnlyRule({ id, destinationId = null, enabled = true, backoff = HTTP_BACKOFF }) {
  return {
    kind: "custom",
    id,
    destinationId,
    enabled,
    scope: "credential",
    match: { statusCodes: [400] },
    backoff: { ...backoff },
  };
}

export function custom400Rule({
  id,
  destinationId = null,
  enabled = true,
  scope = "credential",
  backoff = HTTP_BACKOFF,
}) {
  return {
    kind: "custom",
    id,
    destinationId,
    enabled,
    scope,
    match: {
      statusCodes: [400],
      errorCodes: [CUSTOM_400_ERROR.code],
      errorTypes: [CUSTOM_400_ERROR.type],
      messageContains: [CUSTOM_400_ERROR.message],
    },
    backoff: { ...backoff },
  };
}

export function credentialModel400Rule({ id, destinationId = null, backoff = HTTP_BACKOFF }) {
  return custom400Rule({ id, destinationId, scope: "credential_model", backoff });
}

export function builtinOverride({ id = BUILTIN_GOAT_ID, destinationId = null, enabled = false, backoff = HTTP_BACKOFF }) {
  return {
    kind: "builtin_override",
    id,
    destinationId,
    enabled,
    backoff: { ...backoff },
  };
}

export function httpFault(status, body, headers = {}, delayMs) {
  const item = { kind: "http", status, body, headers };
  if (delayMs != null) item.delayMs = delayMs;
  return item;
}

export function delayFault(ms) {
  return { kind: "delay", ms };
}

/** Expected upstream listeners for HTTP custom-400: first request stays on A. */
export const SEQ_FIRST_400_STAYS_A = Object.freeze(["alpha"]);
/** Next request skips waiting A and sends B. */
export const SEQ_NEXT_AVOIDS_A_USES_B = Object.freeze(["bravo"]);
/** GOAT credits: first request tries A then B. Not impersonated over generic HTTP. */
export const SEQ_GOAT_FIRST_AB = Object.freeze(["alpha", "bravo"]);
export const SEQ_GOAT_NEXT_B = Object.freeze(["bravo"]);
export const SEQ_ALL_WAITING_ZERO = Object.freeze([]);
export const SEQ_SINGLE_FLIGHT_A1_B1 = Object.freeze({ alpha: 1, bravo: 1 });
export const SEQ_LIVE_ABBA = Object.freeze(["alpha", "bravo", "bravo", "alpha"]);

export function routeSlotsOf(started) {
  return (started?.slots || []).filter((slot) => ["alpha", "bravo", "charlie"].includes(slot.slot));
}

export function protocolSlotsOf(started) {
  return (started?.slots || []).filter((slot) => ["chat", "responses", "messages"].includes(slot.slot));
}

export function slotById(started, id) {
  return (started?.slots || []).find((slot) => (slot.id || slot.slot) === id);
}

export function modelTriad(slot, { publicModel } = {}) {
  return {
    public: publicModel || slot?.publicModel || null,
    upstream: slot?.model || slot?.upstreamModel || null,
    live: LIVE_OUTBOUND_MODEL,
  };
}

export function listenersOfHits(hits) {
  return (hits || []).map((hit) => hit.listener);
}

export function assertListenerSequence(actual, expected, label) {
  const got = [...actual];
  const want = [...expected];
  if (got.length !== want.length || got.some((item, index) => item !== want[index])) {
    throw new Error(`${label}: expected listeners ${JSON.stringify(want)}, got ${JSON.stringify(got)}`);
  }
}

export function assertUpstreamCount(hits, expected, label) {
  if ((hits || []).length !== expected) {
    throw new Error(`${label}: expected ${expected} upstream sends, got ${(hits || []).length}`);
  }
}

export function listenerCounts(hits) {
  const counts = {};
  for (const hit of hits || []) {
    const id = hit.listener;
    counts[id] = (counts[id] || 0) + 1;
  }
  return counts;
}

export function assertListenerCounts(hits, expected, label) {
  const got = listenerCounts(hits);
  const wantKeys = Object.keys(expected);
  const extra = Object.keys(got).filter((key) => expected[key] == null);
  const mismatch = wantKeys.some((key) => got[key] !== expected[key]) || extra.length > 0;
  if (mismatch) {
    throw new Error(`${label}: expected listener counts ${JSON.stringify(expected)}, got ${JSON.stringify(got)}`);
  }
}

export function liveProbeInput(model) {
  return {
    model,
    stream: false,
    max_tokens: 32,
    messages: [{ role: "user", content: MARKER }],
  };
}

export function assertChatCompletionProtocolValid(text, label) {
  let parsed;
  try {
    parsed = JSON.parse(text);
  } catch {
    throw new Error(`${label}: response is not JSON: ${String(text).slice(0, 200)}`);
  }
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    throw new Error(`${label}: response is not an object`);
  }
  if (parsed.error) {
    throw new Error(`${label}: error object ${parsed.error.message || parsed.error.type || "error"}`);
  }
  if (!Array.isArray(parsed.choices) || parsed.choices.length === 0) {
    throw new Error(`${label}: missing choices`);
  }
  const message = parsed.choices[0].message;
  if (!message || typeof message !== "object") {
    throw new Error(`${label}: missing choices[0].message`);
  }
  return parsed;
}

export function assertLiveSuccessReceipt(hit, label) {
  if (!hit) throw new Error(`${label}: missing receipt`);
  if (hit.live !== true) throw new Error(`${label}: receipt.live=${hit.live} (local fixture is not live)`);
  if (hit.liveStatus !== 200) throw new Error(`${label}: liveStatus=${hit.liveStatus}`);
  if (hit.liveModel !== LIVE_OUTBOUND_MODEL) {
    throw new Error(`${label}: liveModel=${hit.liveModel} (want ${LIVE_OUTBOUND_MODEL})`);
  }
}

export function assertRemoteBudget({ phase, delta, max = LIVE_MAX_REMOTE, beforeSend = false } = {}) {
  const n = Number(delta) || 0;
  if (phase !== PHASE.LIVE) {
    if (n > 0) {
      throw new Error(`simulate-phase remote delta ${n}; expected 0`);
    }
    return n;
  }
  if (beforeSend && n >= max) {
    throw new Error(`live remote budget ${max} exhausted before send (delta ${n})`);
  }
  if (!beforeSend && n > max) {
    throw new Error(`live remote budget ${max} exceeded (delta ${n})`);
  }
  return n;
}

export function policyExitCode(results) {
  const rows = (results || []).filter((row) => row.responsibility !== RESPONSIBILITY.RUST);
  const required = rows.filter(
    (row) => row.responsibility === RESPONSIBILITY.HTTP || row.responsibility === RESPONSIBILITY.LIVE,
  );
  const harness = rows.filter((row) => row.responsibility === RESPONSIBILITY.HARNESS);
  if (required.some((row) => row.status === "FAIL") || harness.some((row) => row.status === "FAIL")) return 1;
  if (required.some((row) => row.status === "NOT_RUN" || row.status === "UNSUPPORTED")) return 2;
  if (required.length === 0) return 2;
  if (!required.some((row) => row.status === "PASS")) return 2;
  return 0;
}

export const RUST_NOT_RUN_REASON =
  "true Command Code GOAT adapter (fixed URL, built-in matcher, controllable clock/capacity/32+) is kernel rust_integration evidence; this harness will not impersonate GOAT over generic HTTP";

export function companionRustRows() {
  return [
    {
      scenarioId: SCENARIO.GOAT_ABB,
      label: "GOAT A,B then B (true adapter, not generic HTTP)",
      status: "NOT_RUN",
      responsibility: RESPONSIBILITY.RUST,
      owner: "kernel rust_integration",
      reason: RUST_NOT_RUN_REASON,
    },
    {
      scenarioId: SCENARIO.GOAT_CAPACITY,
      label: "GOAT >32 waiting candidates before a healthy route",
      status: "NOT_RUN",
      responsibility: RESPONSIBILITY.RUST,
      owner: "kernel rust_integration",
      reason: RUST_NOT_RUN_REASON,
    },
    {
      scenarioId: SCENARIO.GOAT_CLOCK,
      label: "GOAT controllable clock / obsolete generation",
      status: "NOT_RUN",
      responsibility: RESPONSIBILITY.RUST,
      owner: "kernel rust_integration",
      reason: RUST_NOT_RUN_REASON,
    },
    {
      scenarioId: SCENARIO.AUTH_DENIAL,
      label: "dashboard session 401",
      status: "NOT_RUN",
      responsibility: RESPONSIBILITY.RUST,
      owner: "control-plane / Rust",
      reason: "loopback CLI local mode is not a session topology; HTTP harness does not impersonate 401",
    },
    {
      scenarioId: SCENARIO.STALE_SUCCESS,
      label: "in-flight successful probe must not clear a restriction added after it started",
      status: "NOT_RUN",
      responsibility: RESPONSIBILITY.RUST,
      owner: "kernel rust_integration",
      reason: "generation fencing of a late success against a newer restriction is kernel evidence; this harness does not mark it PASS",
    },
  ];
}
