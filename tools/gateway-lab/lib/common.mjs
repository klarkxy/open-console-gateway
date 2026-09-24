import { createHash, randomUUID } from "node:crypto";
import { fileURLToPath } from "node:url";
import path from "node:path";

export const HOST = "127.0.0.1";

export function isLoopbackHost(host) {
  const value = String(host || "").trim().toLowerCase();
  return value === "127.0.0.1" || value === "::1" || value === "localhost" || value === "[::1]";
}

export function assertLoopbackHost(host, label = "host") {
  if (!isLoopbackHost(host)) {
    throw new Error(`${label} must be a loopback address (127.0.0.1, ::1, localhost); got ${host}`);
  }
}
export const BODY_LIMIT = 1024 * 1024;
export const MAX_LOG = 2000;
export const MARKER = "LAB_PROBE";
export const TOOL_MARKER = "LAB_TOOL";
export const UTF8_MARKER = "LAB_UTF8";
export const TOOL_NAME = "lab_echo";
export const TOOL_SCHEMA = {
  type: "object",
  properties: { value: { type: "string" } },
  required: ["value"],
};
export const LIVE_MODEL = "minimax-m3";
export const LIVE_URL_ENV = "OCG_LAB_REMOTE_CHAT_URL";
export const LIVE_KEY_ENV = "OCG_LAB_REMOTE_KEY";
export const FORBIDDEN_PORTS = new Set([19143, 19144]);
export const REQUEST_TIMEOUT_MS = 20000;
export const LIVE_TIMEOUT_MS = 60000;
export const LIVE_MAX_TOKENS = 512;
export const LIVE_CONCURRENCY = 1;
export const LIVE_MAX_CALLS = 24;
export const CREATED_AT = 0;

export function labDir() {
  return path.dirname(path.dirname(fileURLToPath(import.meta.url)));
}

export function repoRoot() {
  return path.resolve(labDir(), "..", "..");
}

export function sha256(value) {
  return createHash("sha256").update(value).digest("hex");
}

export function sha256Buffer(buf) {
  return createHash("sha256").update(buf).digest("hex");
}

export function isObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

export function headerValue(value) {
  if (value == null) return "";
  if (Array.isArray(value)) return value.map(String).join("\n").trim();
  return String(value).trim();
}

export function pathnameOf(req) {
  try {
    return new URL(req.url || "/", "http://127.0.0.1").pathname;
  } catch {
    return "";
  }
}

export function isJsonContentType(value) {
  if (!value) return false;
  const media = String(Array.isArray(value) ? value[0] : value)
    .split(";")[0]
    .trim()
    .toLowerCase();
  return media === "application/json";
}

export function readBody(req, limit = BODY_LIMIT) {
  return new Promise((resolve, reject) => {
    const chunks = [];
    let size = 0;
    let oversized = false;
    req.on("data", (chunk) => {
      size += chunk.length;
      if (size > limit) {
        oversized = true;
        return;
      }
      chunks.push(chunk);
    });
    req.on("end", () => {
      resolve({ buf: oversized ? Buffer.alloc(0) : Buffer.concat(chunks), oversized });
    });
    req.on("error", reject);
  });
}

export function writeJson(res, status, body, headers = {}) {
  const payload = JSON.stringify(body);
  res.writeHead(status, {
    ...headers,
    "content-type": "application/json",
    "content-length": Buffer.byteLength(payload),
  });
  res.end(payload);
}

export function writeSseHeaders(res) {
  res.writeHead(200, {
    "content-type": "text/event-stream",
    "cache-control": "no-cache",
  });
}

export function writeSse(res, body) {
  writeSseHeaders(res);
  res.end(body);
}

export function sseEvent(event, data) {
  const prefix = event ? `event: ${event}\n` : "";
  return `${prefix}data: ${typeof data === "string" ? data : JSON.stringify(data)}\n\n`;
}

export function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

export function newRunId() {
  const stamp = new Date().toISOString().replace(/[-:]/g, "").replace(/\.\d+Z$/, "Z");
  return `${stamp}-${randomUUID().slice(0, 8)}`;
}

export function parseArgv(argv) {
  const tokens = [...argv];
  const command = tokens[0] && !tokens[0].startsWith("-") ? tokens.shift() : "";
  const flags = {};
  const positional = [];
  for (let i = 0; i < tokens.length; i += 1) {
    const token = tokens[i];
    if (token === "--") {
      positional.push(...tokens.slice(i + 1));
      break;
    }
    if (token.startsWith("--")) {
      const eq = token.indexOf("=");
      if (eq !== -1) {
        flags[token.slice(2, eq)] = token.slice(eq + 1);
        continue;
      }
      const name = token.slice(2);
      const next = tokens[i + 1];
      if (next != null && !next.startsWith("-")) {
        flags[name] = next;
        i += 1;
      } else {
        flags[name] = true;
      }
      continue;
    }
    positional.push(token);
  }
  return { command, flags, positional };
}

export function isDirectRun(metaUrl) {
  const entry = process.argv[1];
  if (!entry) return false;
  try {
    return path.resolve(entry) === fileURLToPath(metaUrl);
  } catch {
    return false;
  }
}

export function createMutex() {
  let chain = Promise.resolve();
  return (fn) => {
    const run = chain.then(fn, fn);
    chain = run.then(
      () => undefined,
      () => undefined,
    );
    return run;
  };
}

const SECRET_KEYS = new Set(["key", "secret", "secretInput", "authorization", "x-api-key", "apiKey", "api_key"]);
const rememberedSecrets = new Set();

export function rememberSecret(value) {
  const text = typeof value === "string" ? value.trim() : "";
  if (text.length >= 8) rememberedSecrets.add(text);
  return text;
}

function redactRemembered(text) {
  let out = text;
  for (const secret of rememberedSecrets) {
    if (secret && out.includes(secret)) out = out.split(secret).join(redactValue(secret));
  }
  return out;
}

export function redactValue(value) {
  if (typeof value !== "string" || value.length === 0) return value;
  return `[redacted sha256:${sha256(value).slice(0, 12)}]`;
}

export function redactDeep(value, key = "") {
  if (typeof value === "string") {
    if (SECRET_KEYS.has(key) || /secretInput|authorization|bundlePassword|password|bundle/i.test(key)) return redactValue(value);
    let out = redactRemembered(value);
    if (/^sk-|Bearer\s+\S+/i.test(out)) return redactValue(out);
    if (/sk-[A-Za-z0-9_-]{8,}/.test(out) || /Bearer\s+\S+/i.test(out)) {
      out = out
        .replace(/sk-[A-Za-z0-9_-]{8,}/g, (match) => redactValue(match))
        .replace(/Bearer\s+\S+/gi, (match) => redactValue(match));
    }
    return out;
  }
  if (Array.isArray(value)) return value.map((item) => redactDeep(item));
  if (!isObject(value)) return value;
  const out = {};
  for (const [name, item] of Object.entries(value)) out[name] = redactDeep(item, name);
  return out;
}

export function promptDigest(text) {
  const value = typeof text === "string" ? text : JSON.stringify(text ?? "");
  return sha256(value).slice(0, 16);
}

export function familyOfProtocol(protocol) {
  if (protocol === "messages") return "messages";
  if (protocol === "responses") return "responses";
  return "chat";
}

export function modelsPathFor(inferencePath) {
  const pathName = inferencePath.replace(/\/$/, "");
  if (pathName.endsWith("/chat/completions")) return `${pathName.slice(0, -"/chat/completions".length)}/models`;
  if (pathName.endsWith("/responses")) return `${pathName.slice(0, -"/responses".length)}/models`;
  if (pathName.endsWith("/messages")) return `${pathName.slice(0, -"/messages".length)}/models`;
  return "/v1/models";
}

export function assert(condition, message) {
  if (!condition) throw new Error(message);
}
