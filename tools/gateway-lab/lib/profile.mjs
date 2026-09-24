import { readFile } from "node:fs/promises";
import path from "node:path";
import { HOST, assertLoopbackHost, labDir, modelsPathFor } from "./common.mjs";

export const DEFAULT_PROFILE = Object.freeze({
  name: "default",
  host: HOST,
  live: Object.freeze({
    model: "minimax-m3",
    maxTokens: 512,
    timeoutMs: 60000,
    concurrency: 1,
    maxCallsPerVerify: 24,
  }),
  listeners: Object.freeze([{ id: "alpha" }, { id: "bravo" }, { id: "charlie" }]),
  endpoints: Object.freeze([
    {
      id: "chat",
      listener: "alpha",
      protocol: "chat_completions",
      path: "/chat/v1/chat/completions",
      auth: "bearer",
      secret: "sk-lab-chat",
      ok: "LAB_OK_chat",
      publicModel: "lab-chat",
      upstreamModel: "upstream-chat",
      catalog: ["upstream-chat"],
    },
    {
      id: "responses",
      listener: "bravo",
      protocol: "responses",
      path: "/responses/v1/responses",
      auth: "bearer",
      secret: "sk-lab-responses",
      ok: "LAB_OK_responses",
      publicModel: "lab-responses",
      upstreamModel: "upstream-responses",
      catalog: ["upstream-responses"],
    },
    {
      id: "messages",
      listener: "charlie",
      protocol: "messages",
      path: "/messages/v1/messages",
      auth: "x-api-key",
      secret: "sk-lab-messages",
      ok: "LAB_OK_messages",
      publicModel: "lab-messages",
      upstreamModel: "upstream-messages",
      catalog: ["upstream-messages"],
    },
    {
      id: "alpha",
      listener: "alpha",
      protocol: "chat_completions",
      path: "/chat/v1/chat/completions",
      auth: "bearer",
      secret: "sk-lab-alpha",
      ok: "LAB_OK_alpha",
      publicModel: "lab-route",
      upstreamModel: "upstream-alpha",
      catalog: ["upstream-alpha"],
    },
    {
      id: "bravo",
      listener: "bravo",
      protocol: "chat_completions",
      path: "/chat/v1/chat/completions",
      auth: "bearer",
      secret: "sk-lab-bravo",
      ok: "LAB_OK_bravo",
      publicModel: "lab-route",
      upstreamModel: "upstream-bravo",
      catalog: ["upstream-bravo"],
    },
    {
      id: "charlie",
      listener: "charlie",
      protocol: "chat_completions",
      path: "/chat/v1/chat/completions",
      auth: "bearer",
      secret: "sk-lab-charlie",
      ok: "LAB_OK_charlie",
      publicModel: "lab-route",
      upstreamModel: "upstream-charlie",
      catalog: ["upstream-charlie"],
    },
  ]),
});

export const LISTENER_IDS = Object.freeze(DEFAULT_PROFILE.listeners.map((item) => item.id));

export function normalizeEndpoint(raw) {
  if (!raw || typeof raw !== "object") throw new Error("endpoint must be an object");
  const protocol = raw.protocol;
  if (!["chat_completions", "responses", "messages"].includes(protocol)) {
    throw new Error(`unsupported protocol: ${protocol}`);
  }
  const pathName = raw.path;
  if (typeof pathName !== "string" || !pathName.startsWith("/")) throw new Error("endpoint path must start with /");
  const auth = raw.auth ?? (protocol === "messages" ? "x-api-key" : "bearer");
  if (!["bearer", "x-api-key"].includes(auth)) throw new Error(`unsupported auth: ${auth}`);
  const upstreamModel = raw.upstreamModel ?? raw.model;
  const publicModel = raw.publicModel;
  if (!upstreamModel || !publicModel) throw new Error(`endpoint ${raw.id} needs publicModel and upstreamModel`);
  return {
    id: raw.id,
    slot: raw.slot ?? raw.id,
    listener: raw.listener,
    protocol,
    path: pathName,
    modelsPath: raw.modelsPath ?? modelsPathFor(pathName),
    auth,
    secret: raw.secret,
    ok: raw.ok ?? `LAB_OK_${raw.id}`,
    publicModel,
    upstreamModel,
    model: upstreamModel,
    catalog: Array.isArray(raw.catalog) && raw.catalog.length ? [...raw.catalog] : [upstreamModel],
  };
}

export function normalizeProfile(raw) {
  const name = raw?.name || "custom";
  const host = raw?.host || HOST;
  assertLoopbackHost(host, "profile.host");
  if (raw?.live?.model && raw.live.model !== "minimax-m3") {
    throw new Error("profile.live.model is invalid; live outbound model is always minimax-m3");
  }
  const listeners = Array.isArray(raw?.listeners) && raw.listeners.length ? raw.listeners.map((item) => ({ id: item.id || item })) : DEFAULT_PROFILE.listeners.map((item) => ({ ...item }));
  const endpoints = (raw?.endpoints || []).map(normalizeEndpoint);
  if (endpoints.length < 3) throw new Error("profile must declare at least three endpoints");
  const protocols = new Set(endpoints.map((item) => item.protocol));
  if (!protocols.has("chat_completions") || !protocols.has("responses") || !protocols.has("messages")) {
    throw new Error("profile must include Chat Completions, Responses, and Messages endpoints");
  }
  for (const endpoint of endpoints) {
    if (!listeners.some((item) => item.id === endpoint.listener)) {
      throw new Error(`endpoint ${endpoint.id} references unknown listener ${endpoint.listener}`);
    }
  }
  const live = { ...DEFAULT_PROFILE.live, ...(raw?.live || {}), model: "minimax-m3" };
  return { name, host, live, listeners, endpoints };
}

export async function loadProfile(nameOrPath = "default") {
  if (!nameOrPath || nameOrPath === "default") {
    const file = path.join(labDir(), "profiles", "default.json");
    try {
      return normalizeProfile(JSON.parse(await readFile(file, "utf8")));
    } catch {
      return normalizeProfile(DEFAULT_PROFILE);
    }
  }
  const candidate = path.isAbsolute(nameOrPath)
    ? nameOrPath
    : path.join(labDir(), "profiles", nameOrPath.endsWith(".json") ? nameOrPath : `${nameOrPath}.json`);
  const text = await readFile(candidate, "utf8");
  return normalizeProfile(JSON.parse(text));
}

export function slotsOf(profile) {
  return profile.endpoints.map((endpoint) => ({
    slot: endpoint.slot,
    listener: endpoint.listener,
    protocol: endpoint.protocol,
    path: endpoint.path,
    modelsPath: endpoint.modelsPath,
    model: endpoint.upstreamModel,
    secret: endpoint.secret,
    auth: endpoint.auth,
    ok: endpoint.ok,
    publicModel: endpoint.publicModel,
    catalog: [...endpoint.catalog],
    id: endpoint.id,
  }));
}

export const SLOT_DEFS = Object.freeze(slotsOf(normalizeProfile(DEFAULT_PROFILE)));
