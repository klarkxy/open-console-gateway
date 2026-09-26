import { readFile } from "node:fs/promises";
import { assertLoopbackHost, isLoopbackHost, rememberSecret, sha256 } from "./common.mjs";

const CONTROL_TIMEOUT_MS = 8000;

function loopbackUrl(url, label) {
  let parsed;
  try {
    parsed = new URL(url);
  } catch {
    throw new Error(`${label} is not a URL: ${url}`);
  }
  if (parsed.protocol !== "http:" && parsed.protocol !== "https:") {
    throw new Error(`${label} must be http(s), got ${parsed.protocol}`);
  }
  assertLoopbackHost(parsed.hostname, label);
  return parsed;
}

export function assertLoopbackRuntime(runtime) {
  if (!runtime || typeof runtime !== "object") throw new Error("runtime.json is not an object");
  const host = runtime.host || runtime.control?.host;
  if (host) assertLoopbackHost(host, "runtime host");
  const controlUrl = runtime.control?.url;
  if (!controlUrl) throw new Error("runtime.json is missing control.url");
  loopbackUrl(controlUrl, "control.url");
  for (const listener of runtime.listeners || []) {
    if (listener.host) assertLoopbackHost(listener.host, `listener ${listener.id} host`);
    if (listener.url) loopbackUrl(listener.url, `listener ${listener.id} url`);
  }
  for (const slot of runtime.slots || []) {
    if (slot.url) loopbackUrl(slot.url, `slot ${slot.slot || slot.id} url`);
    if (slot.listenerUrl) loopbackUrl(slot.listenerUrl, `slot ${slot.slot || slot.id} listenerUrl`);
    if (slot.secret) rememberSecret(slot.secret);
  }
  return runtime;
}

export async function loadLabRuntimeFile(runtimePath) {
  const text = await readFile(runtimePath, "utf8");
  return assertLoopbackRuntime(JSON.parse(text));
}

export async function call(lab, method, ...args) {
  const fn = lab?.[method];
  if (typeof fn !== "function") throw new Error(`lab is missing ${method}`);
  const result = fn.apply(lab, args);
  if (result && typeof result.then === "function") return await result;
  return result;
}

export async function labSnapshot(lab) {
  return (await call(lab, "snapshot")) || [];
}

export async function labStats(lab) {
  return (await call(lab, "stats")) || { remoteCalls: 0 };
}

export async function snapshotLiveArmed(lab) {
  const stats = await labStats(lab);
  if (typeof stats.liveEnabled === "boolean") return stats.liveEnabled;
  const runtime = typeof lab.runtime === "function" ? lab.runtime() : {};
  return Boolean(runtime.liveEnabled);
}

async function controlFetch(base, pathName, method = "GET", body) {
  const response = await fetch(`${base}${pathName}`, {
    method,
    headers: { "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
    signal: AbortSignal.timeout(CONTROL_TIMEOUT_MS),
  });
  const text = await response.text();
  let parsed = null;
  try {
    parsed = JSON.parse(text);
  } catch {
    parsed = null;
  }
  if (!response.ok) {
    throw new Error(`lab control ${method} ${pathName}: ${response.status} ${text.slice(0, 400)}`);
  }
  return parsed;
}

/**
 * HTTP adapter for a coordinator-started lab. close() is a no-op so cleanup
 * cannot stop the external process.
 */
export function attachLabFromRuntime(runtimeDoc) {
  const runtime = assertLoopbackRuntime(runtimeDoc);
  const control = { ...runtime.control };
  const controlBase = control.url.replace(/\/$/, "");
  const listeners = (runtime.listeners || []).map((item) => ({ ...item }));
  const slots = (runtime.slots || []).map((slot) => ({ ...slot }));
  let truncated = Boolean(runtime.truncated);

  function snapshotRuntime() {
    return {
      ...runtime,
      control: { ...control },
      listeners: listeners.map((item) => ({ ...item })),
      slots: slots.map((slot) => ({ ...slot })),
      truncated,
    };
  }

  async function snapshot() {
    const body = await controlFetch(controlBase, "/journal");
    truncated = Boolean(body.truncated);
    return body.requests || [];
  }

  async function stats() {
    const body = await controlFetch(controlBase, "/stats");
    truncated = Boolean(body.truncated);
    return body;
  }

  return {
    attached: true,
    preserveOnCleanup: true,
    listeners,
    control,
    slots,
    journal: [],
    get truncated() {
      return truncated;
    },
    runtime: snapshotRuntime,
    snapshot,
    stats,
    async script(listenerId, queue) {
      await controlFetch(controlBase, "/script", "POST", { listener: listenerId, queue });
    },
    async scriptIsolation(spec, queue) {
      await controlFetch(controlBase, "/script", "POST", { isolation: spec, queue });
    },
    async applyScenario(name, spec = {}) {
      return controlFetch(controlBase, "/scenario", "POST", { name, ...spec });
    },
    async reset() {
      const current = await stats();
      const armed = Boolean(current.liveEnabled);
      await controlFetch(controlBase, "/reset", "POST", {});
      await controlFetch(controlBase, "/live", "POST", { armed });
    },
    async armLive(armed = true) {
      return controlFetch(controlBase, "/live", "POST", { armed: Boolean(armed) });
    },
    async acceptSecret(endpointId, secret) {
      rememberSecret(secret);
      return controlFetch(controlBase, "/accept-key", "POST", { endpointId, secret });
    },
    async acceptModel(endpointId, model) {
      return controlFetch(controlBase, "/accept-model", "POST", { endpointId, model });
    },
    async health() {
      return controlFetch(controlBase, "/health");
    },
    async close() {
      return { verified: true, labPreserved: true, attached: true };
    },
  };
}

export async function attachLabFromPath(runtimePath) {
  const runtime = await loadLabRuntimeFile(runtimePath);
  const lab = attachLabFromRuntime(runtime);
  const health = await lab.health();
  if (!health?.ok) throw new Error(`attached lab health failed: ${JSON.stringify(health)}`);
  return { lab, started: lab.runtime(), runtimePath };
}

export function isolationSpec(slot, { scenario = "", model } = {}) {
  return {
    endpointId: slot.id || slot.slot,
    keyFingerprint: sha256(slot.secret || ""),
    model: model || slot.model || "",
    scenario,
  };
}

export function isLoopbackControl(url) {
  try {
    return isLoopbackHost(new URL(url).hostname);
  } catch {
    return false;
  }
}
