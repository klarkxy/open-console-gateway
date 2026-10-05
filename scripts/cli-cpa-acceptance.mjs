// CPA sole-execution acceptance for the headless CLI.
// Drives ocg and the candidate ocg-cpa-host against one UUID
// profile and loopback synthetic providers. A stock health check, a mocked
// CLI, or supported=true is not a pass. Exit is nonzero until the live
// policy plane is proven.
//
// Trust is the primary-owned file runtime/cpa/artifact-lock.json. This
// harness never writes, infers, or accepts that file. A candidate manifest
// hash is not trust. The placeholder SHA is not an accepted record. A
// missing lock blocks before any product process starts.
//
// OCG_CPA_TEST_ENDPOINTS is the sanctioned test-feature seam for canonical
// opencode and command-code URLs. Every child environment strips it unless
// trusted-quota-persistence is entered with --test-feature-cli or
// --production-feature-off. Feature-off builds must keep official origins.
// The default stage stays pending and does not start that workflow.
// Native Codex, Claude, Kimi, XAI, and Antigravity scenarios are in this
// file. They use the stable dictionary in native-harness-work/rust-hook-api.md
// and the variant manifest in runtime-build/fixture-api.md. --native-fixtures
// checks that source in process and does not listen or start a product.
// The native stage stays blocked until the root lock accepts the variant
// selected by the named compile mode. --test-feature-cli and
// --production-feature-off only locate those predetermined binaries.
//
// Ordered stages share that profile. --scenario NAME runs from the start
// through NAME. --prerequisites checks the lock, the candidate manifest,
// and the CLI without serve. --lock-fixtures runs in-memory lock cases
// and does not start a product process. --list prints stage names and
// does not claim acceptance.
//
// This file does not certify Linux, macOS, real OAuth, or a real provider.

import { spawn, spawnSync } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { createReadStream } from 'node:fs';
import { mkdir, readdir, readFile, realpath, stat, unlink, writeFile } from 'node:fs/promises';
import { createServer as createNetServer, request as httpRequest } from 'node:http';
import { connect as netConnect } from 'node:net';
import { basename, dirname, join, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

const repo = dirname(dirname(fileURLToPath(import.meta.url)));
const workRoot = join(repo, 'tmp', 'ocg3-cli-delivery', 'orchestration-20261004', 'acceptance-work');
const prefix = '/dashboard/api/v4';
const sourceIdentity = {
  sourceCommit: '6fecc6e5567912661654a4eaf9b8f5436facd1c2',
  sourceVersion: 'v8.0.10',
  canonicalVersion: '8.0.10',
  protocolVersion: 1,
  capabilities: [
    'attempt-boundary',
    'selective-no-replay',
    'explicit-429-failover',
    'policy-ipc-v1',
    'readiness-v1',
    'compat-protocol-routes',
    'go-zen-identity',
    'ollama-reasoning',
    'oauth-redacted-references',
    'routing-preserve',
    'identity-fence-epoch-version-material',
  ],
};
const placeholderSHA256 = 'baff0e76f37b32f8e16618b6533f9a435dc36b4bc737302745b4b9353ed45542';
const artifactLockPath = join(repo, 'runtime', 'cpa', 'artifact-lock.json');
const artifactLockStatus = {
  path: artifactLockPath,
  present: false,
  accepted: false,
  trustedSHA256: null,
};
let candidateSha = '';
let trustedSha = '';
const operationPattern = /^child=(\d+);desired=(\d+);applied=(\d+);digest=([0-9a-f]{8}|none);status=(not_prepared|installed|apply_pending|applied|apply_failed|stopped)$/;
const forbiddenUpstreamHeaders = [
  'x-ocg-request-id',
  'x-ocg-process-generation',
  'x-ocg-projection-revision',
  'x-ocg-ready-token',
  'cookie',
];
const uuidPattern = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
const hostExecutable = process.platform === 'win32' ? 'ocg-cpa-host.exe' : 'ocg-cpa-host';
const cliExecutable = process.platform === 'win32' ? 'ocg.exe' : 'ocg';

const args = process.argv.slice(2);
const options = {
  binary: join(repo, 'target', 'debug', cliExecutable),
  hostDir: join(repo, 'tmp', 'ocg3-cli-delivery', 'orchestration-20261004', 'runtime-build'),
  scratch: '',
  scenario: '',
  prerequisites: false,
  list: false,
  lockFixtures: false,
  nativeFixtures: false,
  testFeatureCli: false,
  productionFeatureOff: false,
};
for (let index = 0; index < args.length; index += 1) {
  const arg = args[index];
  const next = args[index + 1];
  if (arg === '--binary') options.binary = resolve(next), index += 1;
  else if (arg === '--host-dir') options.hostDir = resolve(next), index += 1;
  else if (arg === '--scratch') options.scratch = resolve(next), index += 1;
  else if (arg === '--scenario') options.scenario = next, index += 1;
  else if (arg === '--prerequisites') options.prerequisites = true;
  else if (arg === '--list') options.list = true;
  else if (arg === '--lock-fixtures') options.lockFixtures = true;
  else if (arg === '--native-fixtures') options.nativeFixtures = true;
  else if (arg === '--test-feature-cli') options.testFeatureCli = true;
  else if (arg === '--production-feature-off') options.productionFeatureOff = true;
  else throw new Error(`unknown argument ${arg}`);
}
if (options.testFeatureCli && options.productionFeatureOff) {
  throw new Error('test-feature and production feature-off are mutually exclusive');
}
if (options.lockFixtures && options.nativeFixtures) {
  throw new Error('lock fixtures and native fixtures are separate commands');
}

const stages = [];
function stage(name, fn) { stages.push({ name, fn }); }

const secrets = new Set();
const results = [];
const ownedPids = new Set();
const world = {
  root: '',
  dataDir: '',
  endpoint: '',
  session: '',
  serve: undefined,
  upstream: undefined,
  proxy: undefined,
  arrivals: [],
  proxyHits: [],
  proxyRefusals: [],
  trusted: undefined,
  trustedHits: [],
  trustedMode: 'health',
  trustedGoRolling: '',
  trustedGoWeekly: '',
  trustedGoMonthly: '',
  trustedGoatFiveHourMs: 0,
  trustedGoatWeeklyMs: 0,
  trustedGoatStamp: '',
  trustedSeamJson: '',
  nativeRoots: undefined,
  nativeHits: [],
  nativeLoopback: undefined,
  openResponses: new Set(),
  keys: new Map(),
  clientKeys: new Set(),
  models: [
    'ocg-cpa-chat',
    'ocg-cpa-messages',
    'ocg-cpa-responses',
    'ocg-cpa-gemini',
    'ocg-cpa-plane-a',
    'ocg-cpa-plane-b',
    'ocg-cpa-count',
    'ocg-cpa-reject',
    'ocg-cpa-bodyloss',
    'ocg-cpa-truncated',
    'ocg-cpa-empty',
    'ocg-cpa-postoutput',
    'ocg-cpa-hang',
    'ocg-cpa-probe',
  ],
};
let serial = 0;
let halted = false;

function delay(ms) { return new Promise(done => setTimeout(done, ms)); }
function scrub(text) {
  let output = String(text ?? '');
  for (const secret of [...secrets].filter(Boolean).sort((left, right) => right.length - left.length)) {
    output = output.split(secret).join('[redacted]');
  }
  return output.replace(/(authorization|bearer|secret|token|password|api[-_]?key)\s*[:=]\s*\S+/gi, '$1=[redacted]');
}
function fail(message) { const error = new Error(message); error.kind = 'FAIL'; return error; }
function blocked(message) { const error = new Error(message); error.kind = 'BLOCKED'; return error; }
function assert(condition, message) { if (!condition) throw fail(message); }
const PLANE_A = 'ocg-cpa-plane-a';
const PLANE_B = 'ocg-cpa-plane-b';
function upstreamMarker(model) {
  if (model === PLANE_A) return 'loopback-ok:plane-a';
  if (model === PLANE_B) return 'loopback-ok:plane-b';
  return 'loopback-ok';
}
function secretSafeDigest(value) {
  return createHash('sha256').update(String(value ?? ''), 'utf8').digest('hex');
}
function loopback(address) {
  return address === '127.0.0.1' || address === '::1' || address === '::ffff:127.0.0.1';
}
function remember(secret) { if (secret) secrets.add(secret); return secret; }
function parseJson(text) {
  return JSON.parse(text, (_key, value, context) => (
    typeof value === 'number' && Number.isInteger(value) && !Number.isSafeInteger(value) && /^-?\d+$/.test(context?.source ?? '')
      ? BigInt(context.source) : value
  ));
}
function stringifyJson(value) {
  return JSON.stringify(value, (_key, item) => typeof item === 'bigint' ? JSON.rawJSON(item.toString()) : item);
}
async function sha256File(path) {
  const hash = createHash('sha256');
  await new Promise((resolveHash, rejectHash) => {
    const stream = createReadStream(path);
    stream.on('error', rejectHash);
    stream.on('data', chunk => hash.update(chunk));
    stream.on('end', resolveHash);
  });
  return hash.digest('hex');
}
function within(parent, child) {
  const from = resolve(parent).toLowerCase();
  const to = resolve(child).toLowerCase();
  return to === from || to.startsWith(from + sep);
}

function environment() {
  const home = join(world.root, 'home');
  const env = { ...process.env, HOME: home, USERPROFILE: home, OCG_MANAGER_ENCRYPTION_KEY: 'synthetic-cpa-acceptance-cipher' };
  for (const name of [
    'CODEX_HOME', 'CLAUDE_CONFIG_DIR', 'KIMI_CODE_HOME', 'GROK_HOME', 'MINIMAX_DATA_DIR', 'MAVIS_DATA_DIR',
    'ZCODE_PERSONAL_PROVIDER_CONFIG_FILE', 'ZCODE_DATA_BASE_DIR', 'DSH_HOME',
    'OCG_BROWSER_WORKER_URL', 'OCG_BROWSER_CONTROL_TOKEN_FILE', 'OCG_CPA_BASE_URL', 'OCG_CPA_HOST_DIR',
    'MANAGEMENT_PASSWORD', 'OCG_CPA_TEST_ENDPOINTS',
  ]) delete env[name];
  if (world.nativeRoots) {
    for (const [key, name] of Object.entries(NATIVE_ROOT_ENV)) {
      const value = world.nativeRoots[key];
      if (!value || !within(world.root, value)) throw fail(`native ${name} is outside the isolated profile`);
      env[name] = value;
    }
  }
  if (world.trustedSeamJson) env.OCG_CPA_TEST_ENDPOINTS = world.trustedSeamJson;
  return env;
}

function track(child) {
  if (child?.pid) ownedPids.add(child.pid);
  return child;
}
function killOwned(pid) {
  if (!Number.isInteger(pid) || pid <= 4 || !ownedPids.has(pid)) return;
  if (process.platform === 'win32') spawnSync('taskkill', ['/PID', String(pid), '/T', '/F'], { windowsHide: true });
  else { try { process.kill(pid, 'SIGKILL'); } catch {} }
}
function listenerPids(port) {
  const result = spawnSync('netstat', ['-ano', '-p', 'tcp'], { encoding: 'utf8', windowsHide: true });
  const found = new Set();
  for (const line of String(result.stdout ?? '').split(/\r?\n/)) {
    if (!line.includes('LISTENING')) continue;
    const parts = line.trim().split(/\s+/);
    const address = parts[1] ?? '';
    const match = address.match(/:(\d+)$/);
    if (!match || Number(match[1]) !== port) continue;
    const pid = Number(parts.at(-1));
    if (Number.isInteger(pid) && pid > 4) found.add(pid);
  }
  return [...found];
}
function executablePath(pid) {
  if (process.platform === 'win32') {
    const result = spawnSync('powershell.exe', [
      '-NoProfile', '-NonInteractive', '-Command',
      `(Get-CimInstance Win32_Process -Filter "ProcessId=${Number(pid)}").ExecutablePath`,
    ], { encoding: 'utf8', windowsHide: true });
    return String(result.stdout ?? '').trim();
  }
  const result = spawnSync('ps', ['-p', String(pid), '-o', 'comm='], { encoding: 'utf8', windowsHide: true });
  return String(result.stdout ?? '').trim();
}

function begin(argv, timeoutMs = 200000) {
  const child = track(spawn(options.binary, argv, { env: environment(), windowsHide: true, stdio: ['pipe', 'pipe', 'pipe'] }));
  let stdout = '';
  let stderr = '';
  let timedOut = false;
  const done = new Promise((resolveDone, rejectDone) => {
    child.on('error', rejectDone);
    const timer = setTimeout(() => { timedOut = true; killOwned(child.pid); }, timeoutMs);
    child.on('exit', code => {
      clearTimeout(timer);
      ownedPids.delete(child.pid);
      resolveDone({ code, stdout, stderr: scrub(stderr), timedOut, pid: child.pid });
    });
  });
  child.stdout.on('data', chunk => { stdout += chunk; });
  child.stderr.on('data', chunk => { stderr += chunk; });
  child.stdin.end();
  return { child, done };
}
function invoke(argv, { timeoutMs = 200000 } = {}) {
  if (options.lockFixtures) throw fail('lock fixture tried to start a product process');
  if (options.nativeFixtures) throw fail('native fixture tried to start a product process');
  return begin(argv, timeoutMs).done;
}

async function api(method, path, body, { cas = false, success = true, extra = [], timeoutMs } = {}) {
  const index = ++serial;
  const output = join(world.root, `response-${index}.json`);
  const argv = ['--data-dir', world.dataDir, '--endpoint', world.endpoint, 'api', method, path, '--output', output, ...extra];
  if (world.session && path.startsWith('/dashboard/') && !extra.includes('--session-file')) argv.push('--session-file', world.session);
  if (body !== undefined) {
    const file = join(world.root, `request-${index}.json`);
    await writeFile(file, stringifyJson(body));
    argv.push('--input', file);
  }
  if (cas) argv.push('--cas-current');
  const run = await invoke(argv, { timeoutMs });
  if (success) assert(run.code === 0, `${method} ${path} failed: ${run.stderr.slice(0, 700)}`);
  let text = '';
  try { text = await readFile(output, 'utf8'); } catch {}
  let value;
  try { value = text ? parseJson(text) : undefined; } catch { value = text; }
  return { ...run, value };
}

function runtimeFacts(value) {
  const matched = operationPattern.exec(String(value?.currentOperation ?? ''));
  const operation = matched ? {
    child: Number(matched[1]),
    desired: Number(matched[2]),
    applied: Number(matched[3]),
    digest8: matched[4],
    status: matched[5],
  } : null;
  const typedNames = ['childProcessGeneration', 'desiredRevision', 'appliedRevision', 'desiredDigest', 'appliedDigest', 'applyStatus', 'policyReady', 'executionUnavailable'];
  const typed = typedNames.every(name => Object.prototype.hasOwnProperty.call(value ?? {}, name));
  return { operation, typed, raw: value };
}
function summarizeRuntime(value) {
  const facts = runtimeFacts(value);
  return {
    supported: value?.supported ?? null,
    installed: value?.installed ?? null,
    running: value?.running ?? null,
    owned: value?.owned ?? null,
    phase: value?.phase ?? null,
    currentVersion: value?.currentVersion ?? null,
    assetSha256: value?.assetSha256 ?? null,
    port: value?.port ?? null,
    operation: facts.operation,
    typed: facts.typed,
    policyReady: value?.policyReady ?? null,
    executionUnavailable: value?.executionUnavailable ?? null,
    applyStatus: value?.applyStatus ?? facts.operation?.status ?? null,
    error: value?.error ?? value?.unavailableReason ?? null,
  };
}
function requireGeneration(value) {
  const facts = runtimeFacts(value);
  if (!facts.operation && !facts.typed) {
    throw blocked('old Rust generation: CpaRuntime has neither the frozen currentOperation encoding nor the typed execution fields');
  }
  return facts;
}
function requirePolicy(value) {
  const facts = requireGeneration(value);
  if (!facts.typed) throw blocked('CpaRuntime JSON does not expose policyReady and executionUnavailable');
  assert(value.policyReady === true, 'policyReady is false');
  assert(value.executionUnavailable === false, 'executionUnavailable is true');
  assert(value.running === true && value.owned === true, 'owned child is not running');
  assert(trustedSha !== '' && String(value.assetSha256 ?? '').toLowerCase() === trustedSha, 'running artifact SHA is not the trusted lock record');
  assert((value.applyStatus ?? facts.operation?.status) === 'applied', 'projection is not applied');
  return facts;
}

async function freePort() {
  const server = createNetServer();
  await new Promise((done, reject) => server.listen(0, '127.0.0.1', error => error ? reject(error) : done()));
  const { port } = server.address();
  await new Promise(done => server.close(done));
  return port;
}

function classifyBearer(header) {
  const token = String(header ?? '').replace(/^Bearer\s+/i, '');
  if (world.clientKeys.has(token)) return 'client';
  for (const [label, secret] of world.keys) if (secret === token) return label;
  return token ? 'other' : 'none';
}
function recordArrival(req, bodyText) {
  let body = {};
  try { body = JSON.parse(bodyText); } catch {}
  const names = Object.keys(req.headers).map(name => name.toLowerCase());
  const forbiddenHeader = forbiddenUpstreamHeaders.some(name => names.includes(name));
  const label = classifyBearer(req.headers.authorization ?? req.headers['x-api-key']);
  const model = body.model
    ?? String(req.url ?? '').match(/\/models\/([^:/?]+)/)?.[1]
    ?? '';
  const hit = {
    seq: world.arrivals.length + 1,
    method: req.method,
    path: String(req.url ?? '').split('?')[0],
    model,
    label,
    forbiddenHeader,
    proxyStamp: req.headers['x-ocg-acceptance-proxy'] === 'loopback',
    stream: body.stream === true,
  };
  world.arrivals.push(hit);
  return { body, hit };
}
function sendJson(res, status, value) {
  res.writeHead(status, { 'content-type': 'application/json' });
  res.end(JSON.stringify(value));
}
function successFor(hit, body) {
  const usage = { prompt_tokens: 11, completion_tokens: 7, total_tokens: 18 };
  const marker = upstreamMarker(body.model ?? hit.model);
  if (hit.path.includes('countTokens')) return { totalTokens: 11 };
  if (hit.path.endsWith('/messages')) {
    return { id: 'msg-loopback', type: 'message', role: 'assistant', model: body.model, content: [{ type: 'text', text: marker }], usage: { input_tokens: 11, output_tokens: 7 } };
  }
  if (hit.path.endsWith('/responses')) {
    return { id: 'resp-loopback', object: 'response', model: body.model, output: [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text: marker }] }], usage: { input_tokens: 11, output_tokens: 7 } };
  }
  if (hit.path.includes('generateContent')) {
    return { candidates: [{ content: { parts: [{ text: marker }], role: 'model' }, finishReason: 'STOP' }], usageMetadata: { promptTokenCount: 11, candidatesTokenCount: 7 } };
  }
  return { id: 'chatcmpl-loopback', object: 'chat.completion', created: 1, model: body.model, choices: [{ index: 0, message: { role: 'assistant', content: marker }, finish_reason: 'stop' }], usage };
}
function writeStream(res, body) {
  res.writeHead(200, { 'content-type': 'text/event-stream' });
  const marker = upstreamMarker(body.model);
  const chunk = { id: 'chatcmpl-loopback', object: 'chat.completion.chunk', model: body.model, choices: [{ index: 0, delta: { content: marker }, finish_reason: null }] };
  res.write(`data: ${JSON.stringify(chunk)}\n\n`);
  res.write(`data: ${JSON.stringify({ ...chunk, choices: [{ index: 0, delta: {}, finish_reason: 'stop' }], usage: { prompt_tokens: 11, completion_tokens: 7, total_tokens: 18 } })}\n\n`);
  res.end('data: [DONE]\n\n');
}
async function readRequest(req) {
  const chunks = [];
  for await (const chunk of req) chunks.push(chunk);
  return Buffer.concat(chunks).toString('utf8');
}
function startUpstream() {
  const server = createNetServer(async (req, res) => {
    if (!loopback(req.socket.remoteAddress)) { res.writeHead(403); res.end(); return; }
    const text = await readRequest(req);
    const { body, hit } = recordArrival(req, text);
    if (hit.label === 'none' || hit.label === 'other' || hit.label === 'client') {
      sendJson(res, 401, { error: { message: 'fixture credential rejected', type: 'auth' } });
      return;
    }
    if (hit.path.endsWith('/models')) { sendJson(res, 200, { object: 'list', data: world.models.map(id => ({ id, object: 'model' })) }); return; }
    const mode = world.models.find(model => hit.model === model || hit.path.includes(model)) ?? '';
    if (mode === 'ocg-cpa-reject' && hit.label === 'A') { sendJson(res, 429, { error: { message: 'synthetic explicit rejection', type: 'insufficient_quota' } }); return; }
    if (mode === 'ocg-cpa-bodyloss') { res.writeHead(200, { 'content-type': 'application/json' }); res.destroy(); return; }
    if (mode === 'ocg-cpa-truncated') {
      res.writeHead(200, { 'content-type': 'text/event-stream' });
      res.end('data: {"choices":[{"delta":{"content":"partial"}}]}\n\n');
      return;
    }
    if (mode === 'ocg-cpa-empty') { res.writeHead(200, { 'content-type': 'text/event-stream' }); res.end(); return; }
    if (mode === 'ocg-cpa-postoutput') {
      res.writeHead(200, { 'content-type': 'text/event-stream' });
      res.write('data: {"choices":[{"delta":{"content":"loopback-ok"}}]}\n\n');
      res.destroy();
      return;
    }
    if (mode === 'ocg-cpa-hang') { res.writeHead(200, { 'content-type': 'text/event-stream' }); world.openResponses.add(res); res.on('close', () => world.openResponses.delete(res)); return; }
    if (body.stream === true && !hit.path.includes('generateContent')) { writeStream(res, body); return; }
    if (hit.path.includes('streamGenerateContent')) {
      res.writeHead(200, { 'content-type': 'text/event-stream' });
      res.end(`data: ${JSON.stringify({ candidates: [{ content: { parts: [{ text: upstreamMarker(body.model ?? hit.model) }] } }] })}\n\n`);
      return;
    }
    sendJson(res, 200, successFor(hit, body));
  });
  return new Promise(done => server.listen(0, '127.0.0.1', () => done(server)));
}
function startProxy(upstreamPort) {
  const server = createNetServer(async (req, res) => {
    let target;
    try { target = new URL(req.url); } catch { res.writeHead(400); res.end(); return; }
    if (target.hostname !== '127.0.0.1' && target.hostname !== 'localhost') {
      world.proxyRefusals.push(target.hostname);
      res.writeHead(403); res.end(); return;
    }
    world.proxyHits.push({ path: target.pathname, port: target.port });
    const headers = { ...req.headers, host: target.host, 'x-ocg-acceptance-proxy': 'loopback' };
    delete headers['proxy-connection'];
    const proxyReq = httpRequest({ hostname: '127.0.0.1', port: target.port || upstreamPort, path: `${target.pathname}${target.search}`, method: req.method, headers }, proxyRes => {
      res.writeHead(proxyRes.statusCode ?? 502, proxyRes.headers);
      proxyRes.pipe(res);
    });
    proxyReq.on('error', () => { res.writeHead(502); res.end(); });
    req.pipe(proxyReq);
  });
  server.on('connect', (req, socket) => {
    const [host, portText] = String(req.url ?? '').split(':');
    if (host !== '127.0.0.1' && host !== 'localhost') {
      world.proxyRefusals.push(host);
      socket.write('HTTP/1.1 403 Forbidden\r\n\r\n');
      socket.end();
      return;
    }
    world.proxyHits.push({ path: 'CONNECT', port: portText });
    const upstream = netConnect({ host: '127.0.0.1', port: Number(portText) }, () => {
      socket.write('HTTP/1.1 200 Connection Established\r\n\r\n');
      upstream.pipe(socket);
      socket.pipe(upstream);
    });
    upstream.on('error', () => socket.end());
  });
  return new Promise(done => server.listen(0, '127.0.0.1', () => done(server)));
}
function markArrivals() { return world.arrivals.length; }
function since(mark) { return world.arrivals.slice(mark); }
function counts(items) {
  const tally = {};
  for (const item of items) tally[item.label] = (tally[item.label] ?? 0) + 1;
  return tally;
}
function sameCounts(actual, expected) {
  const names = new Set([...Object.keys(actual), ...Object.keys(expected)]);
  for (const name of names) if ((actual[name] ?? 0) !== (expected[name] ?? 0)) return false;
  return true;
}
function cliError(stderr) {
  try { return parseJson(String(stderr).trim()); } catch { return {}; }
}
async function childStatus(url, headers, body) {
  try {
    const response = await fetch(url, {
      method: 'POST',
      headers,
      body,
      signal: AbortSignal.timeout(5000),
    });
    return response.status;
  } catch {
    return 0;
  }
}
function assertSends(items, expected, label) {
  const tally = counts(items);
  assert(sameCounts(tally, expected), `${label} send counts ${JSON.stringify(tally)}`);
  assert(items.every(item => item.forbiddenHeader === false), `${label} forwarded a private correlation header`);
  assert(items.every(item => item.label !== 'client'), `${label} forwarded a client key`);
}
function revisionOf(facts, field) {
  if (facts.typed && facts.raw && facts.raw[field] !== undefined && facts.raw[field] !== null) return facts.raw[field];
  if (!facts.operation) return null;
  if (field === 'desiredRevision') return facts.operation.desired;
  if (field === 'appliedRevision') return facts.operation.applied;
  return null;
}
function statusOf(facts) {
  if (facts.typed && facts.raw?.applyStatus) return facts.raw.applyStatus;
  return facts.operation?.status ?? null;
}
function digestOf(facts) {
  if (facts.typed && facts.raw?.appliedDigest) return facts.raw.appliedDigest;
  return facts.operation?.digest8 ?? null;
}
async function readRuntime() {
  const value = (await api('GET', `${prefix}/external-integrations/cpa/runtime`)).value;
  return { value, facts: runtimeFacts(value) };
}
function policyReadyNow(snapshot) {
  return !snapshot.facts.typed || (snapshot.value.policyReady === true && snapshot.value.executionUnavailable === false);
}
async function waitAutomaticApply(previousApplied) {
  let last = await readRuntime();
  const deadline = Date.now() + 8000;
  for (;;) {
    const desired = revisionOf(last.facts, 'desiredRevision');
    const applied = revisionOf(last.facts, 'appliedRevision');
    const status = statusOf(last.facts);
    const caughtUp = desired !== null && applied !== null && String(desired) === String(applied);
    const moved = applied !== null && String(applied) !== String(previousApplied);
    if (status === 'applied' && caughtUp && moved && policyReadyNow(last) && Date.now() < deadline) {
      await delay(250);
      const confirm = await readRuntime();
      const desired2 = revisionOf(confirm.facts, 'desiredRevision');
      const applied2 = revisionOf(confirm.facts, 'appliedRevision');
      if (statusOf(confirm.facts) === 'applied' && String(desired2) === String(applied2) && String(applied2) === String(applied) && policyReadyNow(confirm)) {
        return { phase: 'applied', desired: desired2, applied: applied2, status: 'applied', ...confirm };
      }
      last = confirm;
      continue;
    }
    if (status === 'apply_failed') return { phase: 'failed', desired, applied, status, ...last };
    if (Date.now() >= deadline) {
      if (status === 'apply_pending' || (desired !== null && applied !== null && String(desired) !== String(applied))) {
        return { phase: 'pending', desired, applied, status, ...last };
      }
      if (status === 'applied' && moved && !policyReadyNow(last)) return { phase: 'unavailable', desired, applied, status, ...last };
      return { phase: 'unchanged', desired, applied, status, ...last };
    }
    await delay(250);
    last = await readRuntime();
  }
}
function assertAutomaticApply(outcome, label) {
  if (outcome.phase !== 'applied') {
    throw fail(`${label} did not apply automatically (${outcome.phase}); desired ${outcome.desired} applied ${outcome.applied}. The harness does not call runtime/start`);
  }
  requirePolicy(outcome.value);
  return outcome;
}
function numericCount(value) {
  const names = ['totalTokens', 'total_tokens', 'input_tokens', 'prompt_tokens', 'promptTokenCount'];
  for (const bag of [value, value?.usage, value?.usageMetadata]) {
    if (!bag || typeof bag !== 'object') continue;
    for (const name of names) {
      if (typeof bag[name] === 'number' && Number.isFinite(bag[name])) return true;
    }
  }
  return false;
}
function renderedRun(run) {
  const body = typeof run.value === 'string' ? run.value : stringifyJson(run.value ?? '');
  return `${body}\n${run.stderr}`;
}
async function countParticipation(model) {
  const logs = (await api('GET', `${prefix}/logs/forward?limit=80`)).value;
  const row = (logs.items ?? []).find(item => item.requestedModel === model || item.model === model || item.upstreamModel === model);
  if (row) return 'forward-log';
  const tail = await api('GET', `${prefix}/external-integrations/cpa/runtime/logs`, undefined, { success: false });
  if (tail.code === 0) {
    const text = `${tail.value?.stdout ?? ''}\n${tail.value?.stderr ?? ''}`;
    if (text.includes(model) || text.includes('countTokens') || text.includes('count_tokens')) return 'cpa-log';
  }
  return '';
}
async function stopOwnedChild() {
  const runtime = (await api('GET', `${prefix}/external-integrations/cpa/runtime`)).value;
  const port = runtime.port;
  await api('POST', `${prefix}/external-integrations/cpa/runtime/stop`, {}, { cas: true });
  for (let attempt = 0; attempt < 40; attempt += 1) {
    const now = (await api('GET', `${prefix}/external-integrations/cpa/runtime`)).value;
    if (now.running !== true && listenerPids(port).length === 0) return port;
    await delay(250);
  }
  throw fail('owned child stayed listening after stop');
}
async function startOwnedChild() {
  const started = (await api('POST', `${prefix}/external-integrations/cpa/runtime/start`, {}, { cas: true })).value;
  requirePolicy(started);
  const pids = listenerPids(started.port);
  assert(pids.length === 1, `owned child port ${started.port} has ${pids.length} listeners after the count check`);
  ownedPids.add(pids[0]);
  world.childPid = pids[0];
  return started;
}
function loopbackUrl(value) {
  return typeof value === 'string' && (value.includes('127.0.0.1') || value.includes('localhost'));
}
async function supportedLoopbackProbe() {
  const contracts = (await api('GET', `${prefix}/provider-contracts`)).value;
  const groups = (contracts.providers ?? []).filter(group => group?.card?.protocolProbe === true && group.providerId && group.providerId !== 'custom');
  const destinations = (await api('GET', `${prefix}/destinations`)).value.destinations ?? [];
  for (const group of groups) {
    const accountId = (group.accounts ?? []).find(account => account?.id)?.id;
    if (!accountId) continue;
    const destination = destinations.find(row => {
      if (row.providerId !== group.providerId || row.id === world.destination?.id) return false;
      const urls = [row.baseUrl, row.endpointUrl, ...(row.protocolRoutes ?? []).map(route => route.endpointUrl)];
      return urls.some(loopbackUrl);
    });
    if (!destination) continue;
    const modelId = (group.models ?? []).find(model => model.routable)?.modelId ?? group.catalog?.models?.[0];
    if (!modelId) continue;
    return { providerId: group.providerId, accountId, modelId };
  }
  return null;
}

async function startServe(dataDir) {
  world.dataDir = dataDir;
  if (!world.port) world.port = await freePort();
  const port = world.port;
  world.endpoint = `http://127.0.0.1:${port}`;
  const child = track(spawn(options.binary, [
    '--data-dir', dataDir, 'serve', '--host', '127.0.0.1', '--port', String(port), '--cpa-host-dir', options.hostDir,
  ], { env: environment(), windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] }));
  let stderr = '';
  child.stderr.on('data', chunk => { stderr = scrub(`${stderr}${chunk}`).slice(-4000); });
  child.stdout.resume();
  world.serve = child;
  for (let attempt = 0; attempt < 120; attempt += 1) {
    if (child.exitCode !== null) throw fail(`serve exited ${child.exitCode}: ${stderr.slice(0, 700)}`);
    try {
      const response = await fetch(`${world.endpoint}/dashboard/api/v4/auth/status`);
      if (response.ok) return port;
    } catch {}
    await delay(250);
  }
  throw fail(`serve did not answer auth status: ${stderr.slice(0, 700)}`);
}
async function stopServe() {
  const child = world.serve;
  world.serve = undefined;
  if (!child || child.exitCode !== null) return;
  const closed = new Promise(done => child.once('exit', done));
  child.kill('SIGINT');
  const exited = await Promise.race([closed.then(() => true), delay(8000).then(() => false)]);
  if (!exited) killOwned(child.pid);
  ownedPids.delete(child.pid);
}
async function shutdown() {
  for (const response of world.openResponses) { try { response.end(); } catch {} }
  await stopServe();
  for (const pid of [...ownedPids]) killOwned(pid);
  await new Promise(done => world.upstream ? world.upstream.close(done) : done());
  await new Promise(done => world.proxy ? world.proxy.close(done) : done());
  await new Promise(done => world.trusted ? world.trusted.close(done) : done());
  world.trusted = undefined;
  await new Promise(done => world.nativeLoopback ? world.nativeLoopback.close(done) : done());
  world.nativeLoopback = undefined;
}

async function prepareProfile() {
  const profiles = join(workRoot, 'profiles');
  await mkdir(profiles, { recursive: true });
  const requested = options.scratch || join(profiles, randomUUID());
  if (!within(profiles, requested)) throw fail('scratch is outside acceptance-work/profiles');
  const uuid = relative(profiles, requested).split(sep)[0];
  if (!uuidPattern.test(uuid) || relative(profiles, requested).includes(sep)) throw fail('scratch must be one UUID directory');
  await mkdir(requested, { recursive: true });
  const real = await realpath(requested);
  const realProfiles = await realpath(profiles);
  if (!within(realProfiles, real)) throw fail('scratch real path left acceptance-work/profiles');
  world.root = real;
  world.dataDir = join(real, 'data');
  world.session = join(real, 'session.json');
  await mkdir(join(real, 'home'), { recursive: true });
  await mkdir(world.dataDir, { recursive: true });
  remember('synthetic-cpa-acceptance-cipher');
}

function normalizePlatformToken(value, kind) {
  const token = String(value ?? '').trim().toLowerCase();
  if (kind === 'os') {
    if (token === 'win32' || token === 'windows') return 'windows';
    if (token === 'darwin' || token === 'macos') return 'macos';
    if (token === 'linux') return 'linux';
    return '';
  }
  if (token === 'amd64' || token === 'x64' || token === 'x86_64') return 'x86_64';
  if (token === 'arm64' || token === 'aarch64') return 'aarch64';
  return '';
}
function isLowerHex(value, length) {
  return typeof value === 'string' && new RegExp(`^[0-9a-f]{${length}}$`).test(value);
}
function safeBasename(value) {
  if (typeof value !== 'string' || value.length === 0) return '';
  if (value.includes('/') || value.includes('\\') || value === '.' || value === '..') return '';
  return value;
}
function nativeCompileMode() {
  return options.testFeatureCli ? 'native-loopback-fixture' : 'production';
}
function expectedBuildTags(compileMode) {
  return compileMode === 'native-loopback-fixture' ? ['ocg_native_loopback_fixture'] : [];
}
function sameStringList(left, right) {
  return Array.isArray(left) && Array.isArray(right) && left.length === right.length && left.every((item, index) => item === right[index]);
}
function evaluateArtifactLock({ lockText, manifest, fileSha, platform, compileMode = 'production' }) {
  const problems = [];
  const result = { problems, trustedSha: '', executableName: '', variant: compileMode };
  if (lockText == null) {
    problems.push('trusted artifact lock is absent');
    return result;
  }
  if (String(lockText).trim() === '') {
    problems.push('trusted artifact lock is empty');
    return result;
  }
  let lock;
  try { lock = JSON.parse(lockText); }
  catch {
    problems.push('trusted artifact lock is not JSON');
    return result;
  }
  if (!lock || typeof lock !== 'object' || Array.isArray(lock)) {
    problems.push('trusted artifact lock is not an object');
    return result;
  }
  if (lock.schemaVersion !== 1) problems.push('trusted artifact lock schemaVersion is not 1');
  if (!isLowerHex(lock.sourceCommit, 40)) problems.push('trusted artifact lock sourceCommit is not 40 lowercase hex');
  if (typeof lock.sourceVersion !== 'string' || lock.sourceVersion.length === 0) problems.push('trusted artifact lock sourceVersion is absent');
  if (lock.protocolVersion !== 1) problems.push('trusted artifact lock protocolVersion is not 1');
  for (const field of ['buildIdentity', 'hostSHA256', 'overlaySHA256']) {
    if (!isLowerHex(lock[field], 64)) problems.push(`trusted artifact lock ${field} is not 64 lowercase hex`);
  }
  const capabilitiesOk = Array.isArray(lock.requiredCapabilities)
    && lock.requiredCapabilities.length > 0
    && lock.requiredCapabilities.every(item => typeof item === 'string' && item.length > 0);
  if (!capabilitiesOk) problems.push('trusted artifact lock requiredCapabilities is empty');
  const currentOs = normalizePlatformToken(platform?.os, 'os');
  const currentArch = normalizePlatformToken(platform?.arch, 'arch');
  if (!currentOs || !currentArch) problems.push('current platform cannot be normalized');
  if (compileMode !== 'production' && compileMode !== 'native-loopback-fixture') {
    problems.push('selected compile mode is not production or native-loopback-fixture');
  }
  const artifacts = Array.isArray(lock.artifacts) ? lock.artifacts : null;
  if (!artifacts || artifacts.length === 0) problems.push('trusted artifact lock has no artifacts');
  const hostSuite = [];
  const seenVariants = new Set();
  const acceptedVariants = new Set(['production', 'native-loopback-fixture']);
  for (const record of artifacts ?? []) {
    if (!record || typeof record !== 'object' || Array.isArray(record)) {
      problems.push('trusted artifact lock artifact is not an object');
      continue;
    }
    const os = normalizePlatformToken(record.os, 'os');
    const arch = normalizePlatformToken(record.arch, 'arch');
    const variant = typeof record.variant === 'string' ? record.variant : '';
    if (!os || !arch) problems.push('trusted artifact lock artifact os/arch cannot be normalized');
    if (!acceptedVariants.has(variant)) problems.push('trusted artifact lock artifact variant is absent or unknown');
    if (os && arch && variant) {
      const key = `${os}/${arch}/${variant}`;
      if (seenVariants.has(key)) problems.push(`duplicate os/arch/variant ${key}`);
      seenVariants.add(key);
    }
    if (variant === 'native-loopback-fixture' && os && os !== 'windows') {
      problems.push('fixture record is not the Windows host');
    }
    if (record.verification !== 'host-suite' && record.verification !== 'compiled-only') {
      problems.push('trusted artifact lock artifact verification is not host-suite or compiled-only');
    }
    if (os === currentOs && arch === currentArch && variant === compileMode && record.verification === 'host-suite') hostSuite.push(record);
  }
  const windowsHost = (variant) => (artifacts ?? []).filter(item => (
    item && typeof item === 'object'
    && normalizePlatformToken(item.os, 'os') === 'windows'
    && normalizePlatformToken(item.arch, 'arch') === 'x86_64'
    && item.variant === variant
    && item.verification === 'host-suite'
    && isLowerHex(item.sha256, 64)
  ));
  const productionHosts = windowsHost('production');
  const fixtureHosts = windowsHost('native-loopback-fixture');
  if (productionHosts.length === 1 && fixtureHosts.length === 1 && productionHosts[0].sha256 === fixtureHosts[0].sha256) {
    problems.push('production and fixture executable hashes are not distinct');
  }
  let record = null;
  if (hostSuite.length === 0) problems.push(`trusted artifact lock has no host-suite record for the current platform and ${compileMode} variant`);
  else if (hostSuite.length > 1) problems.push('trusted artifact lock has more than one host-suite record for the current platform');
  else record = hostSuite[0];
  if (record) {
    const base = safeBasename(record.executable);
    if (!base) problems.push('trusted artifact lock executable is not a basename');
    else result.executableName = base;
    if (!isLowerHex(record.sha256, 64)) problems.push('trusted artifact lock record sha256 is not 64 lowercase hex');
    else if (record.sha256 === placeholderSHA256) problems.push('placeholder SHA baff0e76 is not an accepted lock record');
  }
  const manifestObject = manifest && typeof manifest === 'object' && !Array.isArray(manifest) ? manifest : null;
  if (!manifestObject) problems.push('candidate manifest is absent for the trusted lock');
  else {
    if (isLowerHex(lock.sourceCommit, 40) && manifestObject.sourceCommit !== lock.sourceCommit) {
      problems.push('manifest sourceCommit does not match the trusted artifact lock');
    }
    if (typeof lock.sourceVersion === 'string' && lock.sourceVersion.length > 0 && manifestObject.sourceVersion !== lock.sourceVersion) {
      problems.push('manifest sourceVersion does not match the trusted artifact lock');
    }
    if (lock.protocolVersion === 1 && manifestObject.protocolVersion !== lock.protocolVersion) {
      problems.push('manifest protocolVersion does not match the trusted artifact lock');
    }
    if (isLowerHex(lock.buildIdentity, 64) && manifestObject.buildIdentity !== lock.buildIdentity) {
      problems.push('manifest buildIdentity does not match the trusted artifact lock');
    }
    if (isLowerHex(lock.hostSHA256, 64) && manifestObject.hostSHA256 !== lock.hostSHA256) {
      problems.push('manifest hostSHA256 does not match the trusted artifact lock');
    }
    if (isLowerHex(lock.overlaySHA256, 64) && manifestObject.overlaySHA256 !== lock.overlaySHA256) {
      problems.push('manifest overlaySHA256 does not match the trusted artifact lock');
    }
    const capabilities = Array.isArray(manifestObject.capabilities) ? manifestObject.capabilities : [];
    if (capabilitiesOk) {
      for (const capability of lock.requiredCapabilities) {
        if (!capabilities.includes(capability)) problems.push(`manifest is missing required capability ${capability}`);
      }
    }
    if (manifestObject.variant !== compileMode) problems.push('manifest variant does not match the selected compile mode');
    if (!sameStringList(manifestObject.buildTags, expectedBuildTags(compileMode))) {
      problems.push('manifest buildTags do not match the selected compile mode');
    }
    const goos = normalizePlatformToken(manifestObject.build?.goos, 'os');
    const goarch = normalizePlatformToken(manifestObject.build?.goarch, 'arch');
    if (!goos || !goarch || goos !== currentOs || goarch !== currentArch) {
      const rawOs = manifestObject.build?.goos ?? 'absent';
      const rawArch = manifestObject.build?.goarch ?? 'absent';
      problems.push(`manifest build.goos/build.goarch is ${rawOs}/${rawArch}; ${currentOs || 'unknown'}/${currentArch || 'unknown'} is required`);
    }
  }
  const recordSha = record && isLowerHex(record.sha256, 64) && record.sha256 !== placeholderSHA256 ? record.sha256 : '';
  if (recordSha) {
    if (manifestObject && manifestObject.executableSHA256 !== recordSha) {
      problems.push('manifest executableSHA256 is not the trusted lock record');
    }
    if (fileSha === undefined) problems.push('executable bytes were not compared to the trusted lock record');
    else if (fileSha !== recordSha) problems.push('executable bytes are not the trusted lock record');
  }
  if (problems.length === 0 && recordSha && fileSha === recordSha) result.trustedSha = recordSha;
  return result;
}
async function readArtifactLockText() {
  try {
    const text = await readFile(artifactLockPath, 'utf8');
    artifactLockStatus.present = true;
    return text;
  } catch (error) {
    artifactLockStatus.present = error?.code !== 'ENOENT';
    if (error?.code === 'ENOENT') return null;
    return { unreadable: true };
  }
}
async function prerequisiteReport() {
  const problems = [];
  if (typeof JSON.rawJSON !== 'function') problems.push('Node JSON.rawJSON is required so revision integers survive the CLI');
  if (process.platform !== 'win32' || process.arch !== 'x64') problems.push(`pinned Windows x64 host cannot be claimed on ${process.platform}/${process.arch}`);
  const binaryName = options.binary.split(sep).at(-1)?.toLowerCase() ?? '';
  if (binaryName !== cliExecutable.toLowerCase()) problems.push('binary name is not ocg');
  let binaryExists = false;
  try { await stat(options.binary); binaryExists = true; } catch { problems.push('ocg binary is absent'); }
  const manifestPath = join(options.hostDir, 'manifest.json');
  let manifest;
  try { manifest = JSON.parse(await readFile(manifestPath, 'utf8')); }
  catch { problems.push('pinned manifest.json is unreadable'); }
  if (manifest) {
    const capabilities = Array.isArray(manifest.capabilities) ? manifest.capabilities : [];
    if (manifest.sourceCommit !== sourceIdentity.sourceCommit) problems.push('manifest sourceCommit is not 6fecc6e5567912661654a4eaf9b8f5436facd1c2');
    if (manifest.sourceVersion !== sourceIdentity.sourceVersion) problems.push('manifest sourceVersion is not v8.0.10');
    if (manifest.protocolVersion !== sourceIdentity.protocolVersion) problems.push('manifest protocolVersion is not 1');
    for (const capability of sourceIdentity.capabilities) {
      if (!capabilities.includes(capability)) problems.push(`manifest is missing capability ${capability}`);
    }
    const declared = String(manifest.executableSHA256 ?? '').toLowerCase();
    if (/^[0-9a-f]{64}$/.test(declared)) candidateSha = declared;
    else problems.push('manifest executableSHA256 is absent');
  }
  const platform = { os: process.platform, arch: process.arch };
  const loaded = await readArtifactLockText();
  let decision = { problems: [], trustedSha: '', executableName: '' };
  if (loaded && typeof loaded === 'object' && loaded.unreadable) {
    problems.push('trusted artifact lock is unreadable');
  } else {
    const preview = evaluateArtifactLock({ lockText: loaded, manifest: manifest ?? null, platform, compileMode: nativeCompileMode() });
    let fileSha;
    if (preview.executableName) {
      try { fileSha = await sha256File(join(options.hostDir, preview.executableName)); }
      catch {
        fileSha = '';
        problems.push('pinned ocg-cpa-host executable is unreadable');
      }
    }
    decision = evaluateArtifactLock({ lockText: loaded, manifest: manifest ?? null, fileSha, platform, compileMode: nativeCompileMode() });
  }
  trustedSha = decision.trustedSha;
  artifactLockStatus.accepted = trustedSha.length === 64;
  artifactLockStatus.trustedSHA256 = trustedSha || null;
  for (const problem of decision.problems) {
    if (!problems.includes(problem)) problems.push(problem);
  }
  if (binaryExists && problems.length === 0 && trustedSha) {
    const schema = await invoke(['schema', 'v4'], { timeoutMs: 30000 });
    if (schema.code !== 0 || !String(schema.stdout).includes('OnboardingCommitRequest')) problems.push(`CLI schema v4 did not return the V4 catalog: ${schema.stderr.slice(0, 400)}`);
  } else if (problems.length === 0 && !trustedSha) {
    problems.push('current platform has no accepted trusted lock record');
  }
  return problems;
}

stage('prerequisites', async () => {
  const problems = await prerequisiteReport();
  if (problems.length) throw blocked(problems.join('; '));
});

stage('operator-management-override', async () => {
  const secret = `synthetic-cpa-mgmt-${randomUUID()}`;
  const dataDir = join(world.root, 'operator-override');
  await mkdir(dataDir, { recursive: true });
  const port = await freePort();
  const env = environment();
  assert(env.MANAGEMENT_PASSWORD === undefined, 'operator scenario inherited MANAGEMENT_PASSWORD');
  env.MANAGEMENT_PASSWORD = secret;
  const child = track(spawn(options.binary, [
    '--data-dir', dataDir, 'serve', '--host', '127.0.0.1', '--port', String(port), '--cpa-host-dir', options.hostDir,
  ], { env, windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] }));
  let stdout = '';
  let stderr = '';
  child.stdout.on('data', chunk => { stdout += chunk; });
  child.stderr.on('data', chunk => { stderr += chunk; });
  const exited = new Promise(done => child.once('exit', () => { ownedPids.delete(child.pid); done(true); }));
  let ready = false;
  try {
    for (let attempt = 0; attempt < 120 && child.exitCode === null; attempt += 1) {
      try {
        const response = await fetch(`http://127.0.0.1:${port}/dashboard/api/v4/auth/status`);
        if (response.ok) { ready = true; break; }
      } catch {}
      await delay(250);
    }
  } finally {
    if (child.exitCode === null) {
      child.kill('SIGINT');
      const done = await Promise.race([exited, delay(8000).then(() => false)]);
      if (!done) killOwned(child.pid);
    }
  }
  const raw = `${stdout}\n${stderr}`;
  assert(!raw.includes(secret), 'operator management secret was printed');
  remember(secret);
  assert(ready, `operator override serve did not answer auth status: ${scrub(stderr).slice(0, 400)}`);
  assert(listenerPids(port).length === 0, 'operator override serve stayed listening');
  assert(world.serve === undefined && world.port === undefined, 'operator override replaced the acceptance serve');
  assert(!Object.prototype.hasOwnProperty.call(environment(), 'MANAGEMENT_PASSWORD'), 'normal environment exports MANAGEMENT_PASSWORD');
});

stage('empty-init-register-login', async () => {
  assert(!Object.prototype.hasOwnProperty.call(environment(), 'MANAGEMENT_PASSWORD'), 'normal init exported MANAGEMENT_PASSWORD');
  await startServe(world.dataDir);
  const status = (await api('GET', `${prefix}/auth/status`)).value;
  assert(status.local === true, 'loopback listener was not local');
  assert(status.initialized === false, 'empty profile was already initialized');
  assert(status.authenticated === false, 'empty profile was already authenticated');
  const password = remember('synthetic-cpa-admin-password');
  const registered = await api('POST', `${prefix}/auth/register`, { username: 'cpa-acceptance', password }, { cas: true, extra: ['--session-file', world.session] });
  assert(registered.value.initialized === true && registered.value.authenticated === true, 'register did not establish a session');
  const authed = (await api('GET', `${prefix}/auth/status`)).value;
  assert(authed.authenticated === true, 'session file did not authenticate the next call');
  await api('POST', `${prefix}/auth/logout`, {}, { cas: true });
  const loggedOut = await api('GET', `${prefix}/settings`, undefined, { success: false });
  assert(loggedOut.code !== 0, 'logout left the dashboard writable');
  await api('POST', `${prefix}/auth/login`, { username: 'cpa-acceptance', password }, { cas: true });
  assert((await api('GET', `${prefix}/auth/status`)).value.authenticated === true, 'login did not restore the session');
});

stage('second-serve-refused', async () => {
  const second = await invoke([
    '--data-dir', world.dataDir, 'serve', '--host', '127.0.0.1', '--port', String(await freePort()), '--cpa-host-dir', options.hostDir,
  ], { timeoutMs: 20000 });
  assert(second.timedOut !== true, 'second serve was still running when the harness killed it');
  assert(second.code !== 0, 'a second serve opened the same profile');
  assert((await api('GET', `${prefix}/auth/status`)).value.authenticated === true, 'the first serve stopped answering');
});

stage('install-pinned-child', async () => {
  const before = (await api('GET', `${prefix}/external-integrations/cpa/runtime`)).value;
  requireGeneration(before);
  const installed = (await api('POST', `${prefix}/external-integrations/cpa/runtime/install`, { expectedVersion: 'v8.0.10' }, { cas: true })).value;
  const facts = requireGeneration(installed);
  assert(installed.installed === true, 'install did not mark the candidate child installed');
  assert(trustedSha !== '' && String(installed.assetSha256 ?? '').toLowerCase() === trustedSha, 'install reported an artifact SHA other than the trusted lock record');
  assert(installed.currentVersion === sourceIdentity.canonicalVersion, 'install reported a version other than 8.0.10');
  assert(installed.running !== true, 'install started the child before start');
  assert(facts.operation?.status === 'installed' || installed.applyStatus === 'installed', 'install status was not installed');
  const check = (await api('POST', `${prefix}/external-integrations/cpa/runtime/check-update`, {}, { cas: true })).value;
  assert(check.latestVersion === 'v8.0.10' && check.updateAvailable === false, 'check-update did not report the pinned artifact');
  assert(check.releaseUrl === 'pinned://ocg-cpa-host/v8.0.10', 'check-update used a stock release URL');
  const unready = await api('POST', '/v1/chat/completions', { model: 'ocg-cpa-chat', messages: [{ role: 'user', content: 'not-yet' }] }, { success: false, extra: ['--key-file', await gatewayKeyFile()] });
  assert(unready.code !== 0, 'inference succeeded before the child was started');
  assert(since(0).length === 0, 'a provider was contacted before the child was started');
});

stage('saved-product-config', async () => {
  world.upstream = await startUpstream();
  world.proxy = await startProxy(world.upstream.address().port);
  await new Promise((resolveProxy, rejectProxy) => {
    const probe = httpRequest({
      hostname: '127.0.0.1',
      port: world.proxy.address().port,
      path: 'http://example.com/v1/chat/completions',
      method: 'POST',
    }, response => {
      const status = response.statusCode;
      response.resume();
      response.on('end', () => status === 403 ? resolveProxy() : rejectProxy(fail(`mock proxy returned ${status} for an external host`)));
    });
    probe.on('error', rejectProxy);
    probe.end();
  });
  assert(world.proxyRefusals.includes('example.com'), 'mock proxy dialed or ignored an external host');
  const origin = `http://127.0.0.1:${world.upstream.address().port}`;
  const proxyUrl = `http://127.0.0.1:${world.proxy.address().port}`;
  const secretA = remember(`synthetic-cpa-a-${randomUUID()}`);
  const secretB = remember(`synthetic-cpa-b-${randomUUID()}`);
  world.keys.set('A', secretA);
  world.keys.set('B', secretB);
  const routes = [
    ['chat_completions', '/v1/chat/completions'],
    ['messages', '/v1/messages'],
    ['responses', '/v1/responses'],
  ].map(([protocol, path]) => ({ protocol, endpointUrl: `${origin}${path}`, authScheme: 'bearer' }));
  const committed = (await api('POST', `${prefix}/onboarding/commit`, {
    operationId: randomUUID(),
    mode: 'complete',
    authorizeCurrentEndpoint: true,
    connection: { kind: 'new', templateId: 'custom', name: 'CPA acceptance upstream', endpointUrl: routes[0].endpointUrl, upstreamProtocol: 'chat_completions', authKind: 'bearer', protocolRoutes: routes },
    authorization: { kind: 'api_key', secretInput: secretA, accountLabel: 'credential A' },
    targets: world.models.map(model => ({ publicModel: model, upstreamModel: model })),
  }, { cas: true })).value;
  world.accountA = committed.accountId;
  world.credentialA = committed.credentialId;
  world.connectionId = committed.connectionId;
  assert(world.accountA && world.credentialA && world.connectionId, 'onboarding did not return account, credential, and connection ids');
  const destinations = (await api('GET', `${prefix}/destinations`)).value.destinations;
  world.destination = destinations.find(row => row.id === committed.connectionId || row.name === 'CPA acceptance upstream');
  assert(world.destination, 'custom destination was not listed');
  assert(Object.prototype.hasOwnProperty.call(world.destination, 'plan'), 'destination plan field is absent');
  const contracts = (await api('GET', `${prefix}/provider-contracts`)).value;
  world.providerContracts = contracts;
  const identities = (await api('GET', `${prefix}/accounts`)).value.identities;
  const identity = identities.find(row => row.credentials?.some(item => item.credential.id === world.credentialA));
  assert(identity?.identity?.id, 'credential A has no identity');
  world.identityId = identity.identity.id;
  world.bindingA = identity.credentials.find(item => item.credential.id === world.credentialA)?.bindings?.[0]?.id;
  assert(world.bindingA, 'credential A has no binding');
  const createdB = (await api('POST', `${prefix}/identities/${world.identityId}/credentials`, {
    connectionId: world.connectionId,
    secretInput: secretB,
    accountLabel: 'credential B',
  }, { cas: true })).value;
  world.credentialB = createdB.credentialId;
  world.accountB = createdB.accountId;
  world.bindingB = createdB.bindingId;
  assert(world.credentialB && world.accountB && world.bindingB, 'second credential was not created');
  const account = (await api('GET', `${prefix}/accounts/${world.accountA}`)).value;
  world.providerId = account.providerId;
  const listedAccounts = (await api('GET', `${prefix}/account-records`)).value.accounts ?? [];
  const accountIds = listedAccounts.map(row => row.id).filter(Boolean);
  const orderedIds = [world.accountA, world.accountB, ...accountIds.filter(id => id !== world.accountA && id !== world.accountB)];
  await api('PUT', `${prefix}/accounts/order`, { accountIds: orderedIds }, { cas: true });
  await api('PUT', `${prefix}/settings`, { routingMode: 'strict-priority', proxyMode: 'manual', proxyUrl, conversationSticky: false }, { cas: true });
  const settings = (await api('GET', `${prefix}/settings`)).value;
  assert(settings.routingMode === 'strict-priority', 'strict priority was not saved');
  assert(settings.proxyMode === 'manual' && settings.proxyUrl === proxyUrl, 'manual loopback proxy was not saved');
  await api('PATCH', `${prefix}/alias-publication`, { publicModel: 'ocg-cpa-chat', published: false }, { cas: true });
  const hidden = (await api('GET', `${prefix}/alias-publication`)).value;
  assert((hidden.unpublished ?? []).includes('ocg-cpa-chat'), 'alias was not saved as unpublished');
  const cards = (await api('GET', `${prefix}/routing/cards`)).value;
  assert(Array.isArray(cards.cards), 'routing cards were not readable');
  world.routingCards = cards.cards;
  await api('PUT', `${prefix}/routing/cards`, { cards: cards.cards }, { cas: true });
  const keyFile = await gatewayKeyFile();
  const publicRun = await invoke(['--data-dir', world.dataDir, '--endpoint', world.endpoint, 'api', 'GET', `${prefix}/connection`, '--session-file', world.session]);
  assert(publicRun.code === 0, publicRun.stderr);
  assert(!publicRun.stdout.includes(world.primaryKey), 'stdout printed the client key');
  const created = (await api('POST', `${prefix}/keys`, { name: 'cpa acceptance subkey' }, { cas: true })).value;
  assert(created.revision !== undefined, 'client key create did not return a revision');
  const listed = (await api('GET', `${prefix}/connection`)).value;
  const sub = listed.subKeys.find(row => row.name === 'cpa acceptance subkey');
  assert(sub?.id && sub.value, 'created client key was not listed');
  remember(sub.value);
  world.clientKeys.add(sub.value);
  const rotated = (await api('POST', `${prefix}/keys/${sub.id}/regenerate`, {}, { cas: true })).value;
  assert(rotated, 'client key regenerate returned no body');
  const refreshed = (await api('GET', `${prefix}/connection`)).value.subKeys.find(row => row.id === sub.id);
  if (!refreshed || refreshed.value === sub.value) throw fail('client key rotation did not change the stored key');
  remember(refreshed.value);
  world.clientKeys.add(refreshed.value);
  await api('PATCH', `${prefix}/keys/${sub.id}`, { enabled: false }, { cas: true });
  assert((await api('GET', `${prefix}/connection`)).value.subKeys.find(row => row.id === sub.id).enabled === false, 'client key was not disabled');
  world.disabledKeyId = sub.id;
  world.disabledKeyFile = join(world.root, 'disabled-key.txt');
  await writeFile(world.disabledKeyFile, refreshed.value);
  assert((await api('GET', `${prefix}/connection`)).value.primaryKey === world.primaryKey, 'primary client key changed');
  if (world.destination.plan !== null) {
    assert(world.destination.plan.usageSource && Array.isArray(world.destination.plan.windows), 'destination plan is missing usage source or windows');
  }
  void keyFile;
});

stage('typed-runtime-fields', async () => {
  const runtime = (await api('GET', `${prefix}/external-integrations/cpa/runtime`)).value;
  const facts = requireGeneration(runtime);
  if (!facts.typed) {
    throw blocked('typed CpaRuntime fields are absent: childProcessGeneration, desiredRevision, appliedRevision, desiredDigest, appliedDigest, applyStatus, policyReady, executionUnavailable. currentOperation is only the interim encoding');
  }
  assert(runtime.policyReady === false || runtime.policyReady === true, 'policyReady is not a boolean');
  assert(runtime.executionUnavailable === false || runtime.executionUnavailable === true, 'executionUnavailable is not a boolean');
  if (facts.operation) {
    assert(Number(runtime.childProcessGeneration) === facts.operation.child, 'typed child generation disagrees with currentOperation');
    assert(Number(runtime.desiredRevision) === facts.operation.desired, 'typed desired revision disagrees with currentOperation');
    assert(Number(runtime.appliedRevision) === facts.operation.applied, 'typed applied revision disagrees with currentOperation');
    assert(runtime.applyStatus === facts.operation.status, 'typed apply status disagrees with currentOperation');
  }
});

stage('start-applied-child', async () => {
  const started = (await api('POST', `${prefix}/external-integrations/cpa/runtime/start`, {}, { cas: true })).value;
  const facts = requireGeneration(started);
  assert(started.running === true && started.owned === true, 'start did not run the owned child');
  assert(trustedSha !== '' && String(started.assetSha256 ?? '').toLowerCase() === trustedSha, 'started artifact SHA is not the trusted lock record');
  assert(started.currentVersion === sourceIdentity.canonicalVersion, 'started version is not 8.0.10');
  assert((started.applyStatus ?? facts.operation?.status) === 'applied', 'start did not apply the projection');
  const digest = facts.typed ? started.appliedDigest : facts.operation?.digest8;
  assert(digest && digest !== 'none', 'applied digest is empty');
  const pids = listenerPids(started.port);
  assert(pids.length === 1, `owned child port ${started.port} has ${pids.length} listeners`);
  ownedPids.add(pids[0]);
  world.childPid = pids[0];
  const path = executablePath(pids[0]);
  assert(path.toLowerCase().endsWith(hostExecutable.toLowerCase()), 'child listener is not ocg-cpa-host');
  assert(trustedSha !== '' && await sha256File(path) === trustedSha, 'child executable SHA is not the trusted lock record');
  if (!facts.typed) {
    throw blocked('owned child matched the trusted lock record, but CpaRuntime JSON omits childProcessGeneration, desiredRevision, appliedRevision, desiredDigest, appliedDigest, applyStatus, policyReady, and executionUnavailable');
  }
  requirePolicy(started);
  const models = (await api('GET', '/v1/models', undefined, { extra: ['--key-file', await gatewayKeyFile()] })).value;
  assert(!(models.data ?? []).some(row => row.id === 'ocg-cpa-chat'), 'unpublished alias was listed');
  await api('PATCH', `${prefix}/alias-publication`, { publicModel: 'ocg-cpa-chat', published: true }, { cas: true });
  const visible = (await api('GET', '/v1/models', undefined, { extra: ['--key-file', world.gatewayKeyFile] })).value;
  assert((visible.data ?? []).some(row => row.id === 'ocg-cpa-chat'), 'republished alias stayed hidden');
});

stage('saved-versus-applied', async () => {
  requirePolicy((await api('GET', `${prefix}/external-integrations/cpa/runtime`)).value);
  const before = await readRuntime();
  const beforeApplied = revisionOf(before.facts, 'appliedRevision');
  await api('PUT', `${prefix}/settings`, { routingMode: 'round-robin' }, { cas: true });
  const round = assertAutomaticApply(await waitAutomaticApply(beforeApplied), 'round-robin save');
  const mark = markArrivals();
  await api('POST', '/v1/chat/completions', { model: 'ocg-cpa-chat', messages: [{ role: 'user', content: 'round-robin' }] }, { extra: ['--key-file', await gatewayKeyFile()] });
  const roundHits = since(mark);
  assert(roundHits.length === 1 && (roundHits[0].label === 'A' || roundHits[0].label === 'B'), `round-robin send counts ${JSON.stringify(counts(roundHits))}`);
  assert(roundHits.every(item => item.forbiddenHeader === false && item.label !== 'client'), 'round-robin forwarded a client key or correlation header');
  await api('PUT', `${prefix}/settings`, { routingMode: 'strict-priority' }, { cas: true });
  const strict = assertAutomaticApply(await waitAutomaticApply(round.applied), 'strict-priority save');
  const strictMark = markArrivals();
  await api('POST', '/v1/chat/completions', { model: 'ocg-cpa-chat', messages: [{ role: 'user', content: 'strict-again' }] }, { extra: ['--key-file', world.gatewayKeyFile] });
  assertSends(since(strictMark), { A: 1 }, 'strict-priority');
  const previous = world.keys.get('A');
  world.keys.set('A-old', previous);
  world.keys.delete('A');
  const rotated = remember(`synthetic-cpa-a2-${randomUUID()}`);
  world.keys.set('A', rotated);
  await api('POST', `${prefix}/credentials/${world.credentialA}/rotate`, { secretInput: rotated }, { cas: true });
  const immediateMark = markArrivals();
  const immediate = await api('POST', '/v1/chat/completions', { model: 'ocg-cpa-chat', messages: [{ role: 'user', content: 'rotated-material' }] }, { success: false, extra: ['--key-file', world.gatewayKeyFile] });
  const immediateHits = since(immediateMark);
  assert((counts(immediateHits)['A-old'] ?? 0) === 0, 'rotated material was sent');
  if (immediateHits.length === 0) assert(immediate.code !== 0, 'rotation was unavailable but the CLI returned success');
  else assertSends(immediateHits, { A: 1 }, 'rotation already applied');
  assertAutomaticApply(await waitAutomaticApply(strict.applied), 'credential rotation');
  const afterApply = markArrivals();
  await api('POST', '/v1/chat/completions', { model: 'ocg-cpa-chat', messages: [{ role: 'user', content: 'new-material' }] }, { extra: ['--key-file', world.gatewayKeyFile] });
  assertSends(since(afterApply), { A: 1 }, 'rotated-after-apply');
});

stage('inference-protocols', async () => {
  requirePolicy((await api('GET', `${prefix}/external-integrations/cpa/runtime`)).value);
  const key = await gatewayKeyFile();
  const disabledMark = markArrivals();
  const disabled = await api('POST', '/v1/chat/completions', { model: 'ocg-cpa-chat', messages: [{ role: 'user', content: 'disabled' }] }, { success: false, extra: ['--key-file', world.disabledKeyFile] });
  assert(disabled.code !== 0, 'a disabled client key was accepted');
  assert(since(disabledMark).length === 0, 'a disabled client key reached the provider');
  await api('DELETE', `${prefix}/keys/${world.disabledKeyId}`, {}, { cas: true });
  assert(!(await api('GET', `${prefix}/connection`)).value.subKeys.some(row => row.id === world.disabledKeyId), 'deleted client key remained');
  const calls = [
    ['chat', '/v1/chat/completions', { model: 'ocg-cpa-chat', messages: [{ role: 'user', content: 'ping' }] }],
    ['messages', '/v1/messages', { model: 'ocg-cpa-messages', max_tokens: 16, messages: [{ role: 'user', content: 'ping' }] }],
    ['responses', '/v1/responses', { model: 'ocg-cpa-responses', input: 'ping' }],
    ['gemini', '/v1beta/models/ocg-cpa-gemini:generateContent', { contents: [{ role: 'user', parts: [{ text: 'ping' }] }] }],
    ['gemini-stream', '/v1beta/models/ocg-cpa-gemini:streamGenerateContent', { contents: [{ role: 'user', parts: [{ text: 'ping' }] }] }],
    ['chat-stream', '/v1/chat/completions', { model: 'ocg-cpa-chat', stream: true, messages: [{ role: 'user', content: 'ping' }] }],
  ];
  for (const [label, path, body] of calls) {
    const mark = markArrivals();
    const proxyBefore = world.proxyHits.length;
    const run = await api('POST', path, body, { extra: ['--key-file', key] });
    const hits = since(mark);
    assertSends(hits, { A: 1 }, label);
    assert(world.proxyHits.length > proxyBefore, `${label} did not use the manual loopback proxy`);
    const rendered = typeof run.value === 'string' ? run.value : stringifyJson(run.value);
    assert(rendered.includes('loopback-ok'), `${label} response did not contain the upstream marker`);
  }
  const countCalls = [
    ['count', '/v1beta/models/ocg-cpa-count:countTokens', { contents: [{ role: 'user', parts: [{ text: 'ping' }] }] }],
    ['count-v1', '/v1/models/ocg-cpa-count:countTokens', { contents: [{ role: 'user', parts: [{ text: 'ping' }] }] }],
  ];
  for (const [label, path, body] of countCalls) {
    const mark = markArrivals();
    const run = await api('POST', path, body, { success: false, extra: ['--key-file', key] });
    const hits = since(mark);
    if (/local estimation|not available/i.test(renderedRun(run))) throw fail(`${label} used the OCG local countTokens fallback`);
    assert(run.code === 0, `${label} did not return a CPA count: ${run.stderr.slice(0, 300)}`);
    assert(numericCount(run.value), `${label} did not return a numeric CPA count`);
    assert(hits.length === 0, `${label} made ${hits.length} upstream generation sends`);
  }
  if (!await countParticipation('ocg-cpa-count')) {
    await stopOwnedChild();
    try {
      for (const [label, path, body] of countCalls) {
        const mark = markArrivals();
        const run = await api('POST', path, body, { success: false, extra: ['--key-file', key] });
        assert(!(run.code === 0 && numericCount(run.value)), `${label} returned a numeric count while the child was stopped`);
        assert(since(mark).length === 0, `${label} sent upstream while the child was stopped`);
      }
    } finally {
      await startOwnedChild();
    }
  }
  const mark = markArrivals();
  const rejected = await api('POST', '/v1/responses', { model: 'ocg-cpa-responses', input: 'ping', store: true }, { success: false, extra: ['--key-file', key] });
  assert(rejected.code !== 0, 'stateful responses was accepted');
  assert(since(mark).length === 0, 'stateful responses contacted the provider');
  const runtime = (await api('GET', `${prefix}/external-integrations/cpa/runtime`)).value;
  const direct = await childStatus(
    `${runtime.baseUrl}/v1/chat/completions`,
    { authorization: `Bearer ${world.primaryKey}`, 'content-type': 'application/json' },
    JSON.stringify({ model: 'ocg-cpa-chat', messages: [{ role: 'user', content: 'direct' }] }),
  );
  assert(direct === 401 || direct === 403, `client key on the child returned HTTP ${direct}`);
});

stage('private-hop-and-headers', async () => {
  const runtime = (await api('GET', `${prefix}/external-integrations/cpa/runtime`)).value;
  requirePolicy(runtime);
  const mark = markArrivals();
  const bare = await childStatus(
    `${runtime.baseUrl}/v1/chat/completions`,
    { 'content-type': 'application/json' },
    JSON.stringify({ model: 'ocg-cpa-chat', messages: [{ role: 'user', content: 'no-hop' }] }),
  );
  assert(bare === 401 || bare === 403, `child without the hop secret returned HTTP ${bare}`);
  assert(since(mark).length === 0, 'an unauthenticated child request reached the provider');
  const hop = await api('POST', `${prefix}/external-integrations/cpa/client-keys`, {}, { cas: true, success: false });
  const hopError = cliError(hop.stderr);
  assert(hop.code !== 0 && hopError.code === 'invalidRequest' && hopError.message === 'cpa_hop_secret_only', 'CPA client-key create did not fail closed as cpa_hop_secret_only');
  assert(since(mark).every(item => item.forbiddenHeader === false), 'an upstream request kept an OCG correlation header');
});

stage('explicit-rejection', async () => {
  requirePolicy((await api('GET', `${prefix}/external-integrations/cpa/runtime`)).value);
  const mark = markArrivals();
  await api('POST', '/v1/chat/completions', { model: 'ocg-cpa-reject', messages: [{ role: 'user', content: 'reject' }] }, { extra: ['--key-file', await gatewayKeyFile()] });
  assertSends(since(mark), { A: 1, B: 1 }, 'explicit rejection');
});

stage('no-replay', async () => {
  requirePolicy((await api('GET', `${prefix}/external-integrations/cpa/runtime`)).value);
  const key = await gatewayKeyFile();
  for (const model of ['ocg-cpa-bodyloss', 'ocg-cpa-truncated', 'ocg-cpa-empty', 'ocg-cpa-postoutput']) {
    const mark = markArrivals();
    await api('POST', '/v1/chat/completions', { model, stream: model !== 'ocg-cpa-bodyloss', messages: [{ role: 'user', content: model }] }, { success: false, extra: ['--key-file', key] });
    assertSends(since(mark), { A: 1 }, model);
  }
  const cancelMark = markArrivals();
  const cancel = begin([
    '--data-dir', world.dataDir, '--endpoint', world.endpoint, 'api', 'POST', '/v1/chat/completions',
    '--key-file', key, '--input', await writeJson('cancel', { model: 'ocg-cpa-hang', messages: [{ role: 'user', content: 'cancel' }] }),
    '--output', join(world.root, 'cancel-output.json'),
  ], 30000);
  for (let attempt = 0; attempt < 50 && since(cancelMark).length === 0; attempt += 1) await delay(100);
  assert(since(cancelMark).length === 1, 'cancel fixture did not reach exactly one upstream send before abort');
  killOwned(cancel.child.pid);
  const cancelled = await cancel.done;
  assert(cancelled.code !== 0, 'caller cancel was reported as success');
  await delay(500);
  assertSends(since(cancelMark), { A: 1 }, 'cancel');
  await api('PUT', `${prefix}/settings`, { nonStreamTimeoutSecs: 1 }, { cas: true });
  const deadlineMark = markArrivals();
  const deadline = await api('POST', '/v1/chat/completions', { model: 'ocg-cpa-hang', messages: [{ role: 'user', content: 'deadline' }] }, { success: false, timeoutMs: 20000, extra: ['--key-file', key] });
  assert(deadline.timedOut !== true, 'the gateway did not finish the hung request before the harness killed it');
  assert(deadline.code !== 0, 'deadline hang was treated as success');
  assertSends(since(deadlineMark), { A: 1 }, 'deadline');
  await api('PUT', `${prefix}/settings`, { nonStreamTimeoutSecs: 60 }, { cas: true });
});

stage('quota-does-not-stick', async () => {
  requirePolicy((await api('GET', `${prefix}/external-integrations/cpa/runtime`)).value);
  const before = await restrictionSnapshot();
  const mark = markArrivals();
  await api('POST', '/v1/chat/completions', { model: 'ocg-cpa-chat', messages: [{ role: 'user', content: 'after-429' }] }, { extra: ['--key-file', await gatewayKeyFile()] });
  assertSends(since(mark), { A: 1 }, 'post-rejection chat');
  const after = await restrictionSnapshot();
  assert(after === before, 'ordinary explicit 429 changed quota recovery or temporary restrictions');
  const childPort = (await api('GET', `${prefix}/external-integrations/cpa/runtime`)).value.port;
  const previousPid = world.childPid;
  await stopServe();
  assert(listenerPids(childPort).length === 0, 'stop left the child listening');
  await startServe(world.dataDir);
  await api('POST', `${prefix}/auth/login`, { username: 'cpa-acceptance', password: 'synthetic-cpa-admin-password' }, { cas: true });
  const restored = (await api('GET', `${prefix}/external-integrations/cpa/runtime`)).value;
  requirePolicy(restored);
  const restarted = listenerPids(restored.port);
  assert(restarted.length === 1 && restarted[0] !== previousPid, 'restart did not replace the owned child');
  ownedPids.add(restarted[0]);
  world.childPid = restarted[0];
  const retry = markArrivals();
  await api('POST', '/v1/chat/completions', { model: 'ocg-cpa-chat', messages: [{ role: 'user', content: 'restarted' }] }, { extra: ['--key-file', await gatewayKeyFile()] });
  assertSends(since(retry), { A: 1 }, 'restart');
  if (await restrictionSnapshot() !== before) throw fail('restart created a quota restriction from ordinary 429 history');
});

function isoIn(ms) {
  return new Date(Date.now() + ms).toISOString();
}
function sameInstant(left, right) {
  const a = Date.parse(left);
  const b = Date.parse(right);
  return Number.isFinite(a) && Number.isFinite(b) && a === b;
}
function nearInstant(iso, expectedMs, skewMs) {
  const at = Date.parse(iso);
  return Number.isFinite(at) && Math.abs(at - expectedMs) <= skewMs;
}
function trustedProvider(path) {
  if (path.startsWith('/zen/go/')) return 'opencode';
  if (path.startsWith('/provider/v1/') || path === '/alpha/billing/credits') return 'command-code';
  return '';
}
function recordTrusted(req, path) {
  const token = String(req.headers.authorization ?? req.headers['x-api-key'] ?? '').replace(/^(Bearer|x-api-key)\s+/i, '');
  const hit = {
    method: req.method,
    path,
    provider: trustedProvider(path),
    mode: world.trustedMode,
    leakedClient: Boolean(token) && (world.clientKeys.has(token) || token === world.primaryKey),
  };
  world.trustedHits.push(hit);
  return hit;
}
function trustedCatalog(provider) {
  if (provider === 'command-code') {
    return { object: 'list', data: [{ id: 'ocg-goat-chat', supported_endpoints: ['/chat/completions'] }] };
  }
  return { object: 'list', data: [{ id: 'ocg-go-chat' }] };
}
function trustedUsage(provider) {
  if (provider === 'command-code') {
    return {
      credits: { monthlyCredits: 70 },
      windowLimits: {
        limited: true,
        fiveHour: { used: 0, cap: 14, resetAt: world.trustedGoatFiveHourMs },
        weekly: { used: 0, cap: 35, resetAt: world.trustedGoatWeeklyMs },
      },
    };
  }
  const window = resetsAt => ({ status: 'ok', percent: 0, resetsAt });
  return {
    usage: {
      rolling: window(world.trustedGoRolling),
      weekly: window(world.trustedGoWeekly),
      monthly: window(world.trustedGoMonthly),
    },
  };
}
function trustedGeneration(path, body) {
  const usage = { prompt_tokens: 11, completion_tokens: 7, total_tokens: 18 };
  if (path.endsWith('/messages')) {
    return { id: 'msg-trusted', type: 'message', role: 'assistant', model: body.model, content: [{ type: 'text', text: 'loopback-ok' }], usage: { input_tokens: 11, output_tokens: 7 } };
  }
  if (path.endsWith('/responses')) {
    return { id: 'resp-trusted', object: 'response', model: body.model, output: [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text: 'loopback-ok' }] }], usage: { input_tokens: 11, output_tokens: 7 } };
  }
  return { id: 'chatcmpl-trusted', object: 'chat.completion', created: 1, model: body.model, choices: [{ index: 0, message: { role: 'assistant', content: 'loopback-ok' }, finish_reason: 'stop' }], usage };
}
function startTrustedProviders() {
  world.trustedHits = [];
  world.trustedMode = 'health';
  world.trustedGoRolling = isoIn(30 * 60 * 1000);
  world.trustedGoWeekly = isoIn(2 * 24 * 60 * 60 * 1000);
  world.trustedGoMonthly = isoIn(20 * 24 * 60 * 60 * 1000);
  world.trustedGoatFiveHourMs = Date.now() + 30 * 60 * 1000;
  world.trustedGoatWeeklyMs = Date.now() + 2 * 24 * 60 * 60 * 1000;
  world.trustedGoatStamp = isoIn(14 * 24 * 60 * 60 * 1000);
  const server = createNetServer(async (req, res) => {
    if (!loopback(req.socket.remoteAddress)) { res.writeHead(403); res.end(); return; }
    const path = String(req.url ?? '').split('?')[0];
    const provider = trustedProvider(path);
    recordTrusted(req, path);
    if (!provider) { res.writeHead(404); res.end(); return; }
    const text = await readRequest(req);
    let body = {};
    try { body = JSON.parse(text); } catch {}
    if (req.method === 'GET' && path.endsWith('/models')) { sendJson(res, 200, trustedCatalog(provider)); return; }
    if (req.method === 'GET' && (path.endsWith('/usage') || path === '/alpha/billing/credits')) { sendJson(res, 200, trustedUsage(provider)); return; }
    if (req.method !== 'POST') { res.writeHead(404); res.end(); return; }
    if (world.trustedMode === 'go-limit' && provider === 'opencode') {
      res.writeHead(429, { 'content-type': 'text/plain; charset=utf-8' });
      res.end('Weekly usage limit reached. Resets in 4 days.');
      return;
    }
    if (world.trustedMode === 'goat-limit' && provider === 'command-code') {
      sendJson(res, 429, {
        error: {
          code: 'RATE_LIMITED',
          type: 'rate_limit_error',
          message: `You've reached your 5-hour usage limit for your plan. Your limit resets at ${world.trustedGoatStamp}`,
        },
      });
      return;
    }
    if (world.trustedMode === 'ordinary-429') {
      sendJson(res, 429, { error: { message: 'synthetic explicit rejection', type: 'insufficient_quota' } });
      return;
    }
    sendJson(res, 200, trustedGeneration(path, body));
  });
  return new Promise(done => server.listen(0, '127.0.0.1', () => done(server)));
}
async function closeTrusted() {
  const server = world.trusted;
  world.trusted = undefined;
  if (!server) return;
  await new Promise(done => server.close(done));
}
async function restartAcceptanceServe() {
  await stopServe();
  await startServe(world.dataDir);
  await api('POST', `${prefix}/auth/login`, { username: 'cpa-acceptance', password: 'synthetic-cpa-admin-password' }, { cas: true });
  const runtime = (await api('GET', `${prefix}/external-integrations/cpa/runtime`)).value;
  requirePolicy(runtime);
  const pids = listenerPids(runtime.port);
  assert(pids.length === 1, `owned child port ${runtime.port} has ${pids.length} listeners`);
  ownedPids.add(pids[0]);
  world.childPid = pids[0];
  return runtime;
}
function recoveryFor(accountId, payload) {
  const rows = payload?.credentials ?? [];
  const row = rows.find(item => item.legacyAccountId === accountId || item.accountId === accountId);
  return row?.quotaRecovery ?? null;
}
async function readRecovery(accountId) {
  const payload = (await api('GET', `${prefix}/credentials`)).value;
  return recoveryFor(accountId, payload);
}
async function orderTrustedAccountsLast(goId, goatId) {
  const listed = (await api('GET', `${prefix}/account-records`)).value.accounts ?? [];
  const ids = listed.map(row => row.id).filter(Boolean);
  const tail = [goId, goatId];
  const ordered = [...ids.filter(id => !tail.includes(id)), ...tail.filter(id => ids.includes(id))];
  await api('PUT', `${prefix}/accounts/order`, { accountIds: ordered }, { cas: true });
}
function officialCatalogSource(providerId) {
  if (providerId === 'opencode') return 'https://opencode.ai/zen/go/v1/models';
  if (providerId === 'command-code') return 'https://api.commandcode.ai/provider/v1/models';
  return '';
}
async function refreshTrustedCatalog(providerId, accountId, port, pathPrefix) {
  const before = world.trustedHits.length;
  const refreshed = await api('POST', `${prefix}/providers/${providerId}/models/refresh`, { accountId }, { cas: true, success: false });
  const hits = world.trustedHits.slice(before).filter(hit => hit.path.endsWith('/models'));
  if (hits.length === 0) {
    throw blocked(`pending: ${providerId} model refresh did not use the test-feature loopback. The catalog hook is not integrated or this binary is feature-off. An official origin was not accepted as success.`);
  }
  assert(refreshed.code === 0, `${providerId} model refresh failed after a loopback hit: ${refreshed.stderr.slice(0, 300)}`);
  const catalogPath = providerId === 'opencode' ? '/zen/go/v1/models' : '/provider/v1/models';
  assert(hits.every(hit => hit.path === catalogPath), `${providerId} catalog hit path is ${hits.map(hit => hit.path).join(', ')}`);
  const sourceUrl = String(refreshed.value?.sourceUrl ?? '');
  const loopbackSource = sourceUrl.startsWith(`http://127.0.0.1:${port}${pathPrefix}`);
  const officialSource = sourceUrl === officialCatalogSource(providerId);
  assert(loopbackSource || officialSource, `${providerId} sourceUrl is ${sourceUrl}`);
  const modelId = providerId === 'opencode' ? 'ocg-go-chat' : 'ocg-goat-chat';
  assert((refreshed.value?.models ?? []).includes(modelId), `${providerId} catalog did not include ${modelId}`);
  assert(hits.every(hit => hit.leakedClient === false), `${providerId} catalog refresh forwarded a client key`);
  return refreshed.value;
}
async function publishTrustedAlias(modelId) {
  await api('PATCH', `${prefix}/alias-publication`, { publicModel: modelId, published: true }, { cas: true });
  const listed = (await api('GET', `${prefix}/alias-publication`)).value;
  assert(!(listed.unpublished ?? []).includes(modelId), `${modelId} stayed unpublished`);
}
async function sendTrusted(model, content) {
  const before = world.trustedHits.length;
  const run = await api('POST', '/v1/chat/completions', { model, messages: [{ role: 'user', content }] }, { success: false, extra: ['--key-file', await gatewayKeyFile()] });
  return { run, hits: world.trustedHits.slice(before) };
}
async function assertProtocolUsage(model) {
  for (let attempt = 0; attempt < 20; attempt += 1) {
    const logs = (await api('GET', `${prefix}/logs/forward?limit=80`)).value;
    const row = (logs.items ?? []).find(item => item.requestedModel === model || item.model === model || item.upstreamModel === model);
    if (row && row.promptTokens === 11 && row.completionTokens === 7) return;
    await delay(250);
  }
  throw fail(`${model} did not record protocol usage 11/7`);
}
async function assertRestriction(accountId, windowName, predicate, label) {
  const recovery = await readRecovery(accountId);
  assert(recovery?.status === 'waiting' && recovery.window === windowName, `${label} recovery is ${JSON.stringify(recovery)}`);
  assert(predicate(recovery.resetsAt), `${label} reset ${recovery?.resetsAt} is not the served deadline`);
  return recovery;
}
async function runTrustedQuotaWorkflow() {
  assert(options.testFeatureCli, 'test-feature seam was entered without --test-feature-cli');
  const originalDir = world.dataDir;
  const server = await startTrustedProviders();
  world.trusted = server;
  const port = server.address().port;
  world.trustedSeamJson = JSON.stringify({ opencode: `http://127.0.0.1:${port}`, 'command-code': `http://127.0.0.1:${port}` });
  let workflowError;
  try {
    await restartAcceptanceServe();
    const applied = revisionOf((await readRuntime()).facts, 'appliedRevision');
    const goKey = remember(`synthetic-go-${randomUUID()}`);
    const goatKey = remember(`synthetic-goat-${randomUUID()}`);
    world.keys.set('go', goKey);
    world.keys.set('goat', goatKey);
    const go = (await api('POST', `${prefix}/accounts`, { providerId: 'opencode', name: 'trusted opencode', key: goKey }, { cas: true })).value;
    const goat = (await api('POST', `${prefix}/accounts`, { providerId: 'command-code', name: 'trusted command code', key: goatKey }, { cas: true })).value;
    const goId = go.account?.id;
    const goatId = goat.account?.id;
    assert(goId && goatId, 'trusted account create did not return account ids');
    await orderTrustedAccountsLast(goId, goatId);
    assertAutomaticApply(await waitAutomaticApply(applied), 'trusted accounts');
    await refreshTrustedCatalog('opencode', goId, port, '/zen/go/');
    await refreshTrustedCatalog('command-code', goatId, port, '/provider/v1/');
    await publishTrustedAlias('ocg-go-chat');
    await publishTrustedAlias('ocg-goat-chat');
    const beforeHealth = await restrictionSnapshot();
    world.trustedMode = 'health';
    for (const [model, provider] of [['ocg-go-chat', 'opencode'], ['ocg-goat-chat', 'command-code']]) {
      const sent = await sendTrusted(model, 'trusted-generation');
      if (sent.hits.length === 0) {
        throw blocked(`pending: ${model} did not reach the test-feature loopback. The projection hook is not integrated. Official origin was not accepted as success.`);
      }
      assert(sent.run.code === 0, `${model} generation failed after a loopback hit: ${sent.run.stderr.slice(0, 300)}`);
      assert(sent.hits.length === 1 && sent.hits[0].provider === provider && sent.hits[0].path.endsWith('/chat/completions'), `${model} generation hits ${JSON.stringify(sent.hits)}`);
      assert(sent.hits.every(hit => hit.leakedClient === false), `${model} forwarded a client key`);
      await assertProtocolUsage(model);
    }
    const goUsageMark = world.trustedHits.length;
    const goUsage = await api('POST', `${prefix}/accounts/${goId}/usage/refresh`, {}, { cas: true, success: false });
    const goUsageHits = world.trustedHits.slice(goUsageMark).filter(hit => hit.path === '/zen/go/v1/usage');
    if (goUsageHits.length === 0) throw blocked('pending: OpenCode usage refresh did not use the test-feature loopback. The Go usage hook is not integrated.');
    assert(goUsage.code === 0 && goUsage.value?.source === 'official_go_usage', `Go usage refresh did not accept the official health body: ${goUsage.stderr.slice(0, 300)}`);
    const goatUsageMark = world.trustedHits.length;
    const goatUsage = await api('POST', `${prefix}/accounts/${goatId}/provider-usage`, {}, { cas: true, success: false });
    const goatUsageHits = world.trustedHits.slice(goatUsageMark).filter(hit => hit.path === '/alpha/billing/credits');
    if (goatUsageHits.length === 0) throw blocked('pending: Command Code usage refresh did not use the test-feature loopback. The GOAT usage hook is not integrated.');
    assert(goatUsage.code === 0, `GOAT usage refresh failed after a loopback hit: ${goatUsage.stderr.slice(0, 300)}`);
    assert(await restrictionSnapshot() === beforeHealth, 'health or calibration JSON created a restriction');
    world.trustedMode = 'ordinary-429';
    for (const model of ['ocg-go-chat', 'ocg-goat-chat']) {
      const sent = await sendTrusted(model, 'ordinary-429');
      assert(sent.hits.length === 1, `${model} ordinary 429 hits ${sent.hits.length}`);
      assert(sent.run.code !== 0, `${model} ordinary 429 returned success`);
    }
    assert(await restrictionSnapshot() === beforeHealth, 'ordinary explicit 429 changed quota recovery or temporary restrictions');
    world.trustedMode = 'go-limit';
    const goSentAt = Date.now();
    const goLimited = await sendTrusted('ocg-go-chat', 'go-weekly-limit');
    assert(goLimited.hits.length === 1 && goLimited.hits[0].provider === 'opencode', `Go trusted 429 hits ${JSON.stringify(goLimited.hits)}`);
    assert(goLimited.run.code !== 0, 'Go trusted 429 returned success');
    await assertRestriction(goId, 'week', iso => nearInstant(iso, goSentAt + 4 * 24 * 60 * 60 * 1000, 3 * 60 * 1000), 'Go weekly');
    const goAgain = await sendTrusted('ocg-go-chat', 'go-weekly-again');
    assert(goAgain.hits.length === 0, 'Go restriction still sent upstream');
    assert(goAgain.run.code !== 0, 'Go restriction returned success with zero sends');
    world.trustedMode = 'goat-limit';
    const goatLimited = await sendTrusted('ocg-goat-chat', 'goat-five-hour-limit');
    assert(goatLimited.hits.length === 1 && goatLimited.hits[0].provider === 'command-code', `GOAT trusted 429 hits ${JSON.stringify(goatLimited.hits)}`);
    assert(goatLimited.run.code !== 0, 'GOAT trusted 429 returned success');
    await assertRestriction(goatId, 'five_hours', iso => sameInstant(iso, world.trustedGoatStamp), 'GOAT 5-hour');
    const goatAgain = await sendTrusted('ocg-goat-chat', 'goat-five-hour-again');
    assert(goatAgain.hits.length === 0, 'GOAT restriction still sent upstream');
    assert(goatAgain.run.code !== 0, 'GOAT restriction returned success with zero sends');
    const restricted = await restrictionSnapshot();
    const quiet = world.trustedHits.length;
    await restartAcceptanceServe();
    assert(world.trustedHits.length === quiet, 'restart sent while the trusted restriction was held');
    assert(await restrictionSnapshot() === restricted, 'restart dropped the trusted restriction');
    const snapshot = join(world.root, 'trusted-quota.snapshot');
    const sourceDir = world.dataDir;
    await stopServe();
    const created = await invoke(['--data-dir', sourceDir, 'backup', 'create', '--output', snapshot], { timeoutMs: 60000 });
    assert(created.code === 0, created.stderr);
    const sideDir = join(world.root, 'trusted-restored');
    await mkdir(sideDir, { recursive: true });
    const restored = await invoke(['--data-dir', sideDir, 'backup', 'restore', '--input', snapshot], { timeoutMs: 60000 });
    assert(restored.code === 0, restored.stderr);
    await startServe(sideDir);
    await api('POST', `${prefix}/auth/login`, { username: 'cpa-acceptance', password: 'synthetic-cpa-admin-password' }, { cas: true });
    const sideQuiet = world.trustedHits.length;
    assert((await readRecovery(goId))?.window === 'week', 'backup restore lost the Go weekly reset');
    assert(sameInstant((await readRecovery(goatId))?.resetsAt, world.trustedGoatStamp), 'backup restore lost the GOAT exact reset');
    assert(world.trustedHits.length === sideQuiet, 'restored serve sent while reading trusted restrictions');
    await stopServe();
    world.dataDir = sourceDir;
  } catch (error) {
    workflowError = error;
  } finally {
    world.trustedSeamJson = '';
    world.trustedMode = 'health';
    try {
      await stopServe();
      if (originalDir) {
        await startServe(originalDir);
        await api('POST', `${prefix}/auth/login`, { username: 'cpa-acceptance', password: 'synthetic-cpa-admin-password' }, { cas: true });
        const runtime = (await api('GET', `${prefix}/external-integrations/cpa/runtime`)).value;
        requirePolicy(runtime);
        const pids = listenerPids(runtime.port);
        if (pids.length === 1) { ownedPids.add(pids[0]); world.childPid = pids[0]; }
      }
      await closeTrusted();
    } catch (error) {
      if (!workflowError) workflowError = error;
    }
  }
  if (workflowError) throw workflowError;
}
async function assertProductionFeatureOff() {
  if (!world.proxy) throw blocked('pending: production feature-off negative needs the existing deny-external mock proxy');
  const originalDir = world.dataDir;
  const server = await startTrustedProviders();
  world.trusted = server;
  const port = server.address().port;
  world.trustedSeamJson = JSON.stringify({ opencode: `http://127.0.0.1:${port}`, 'command-code': `http://127.0.0.1:${port}` });
  let workflowError;
  try {
    await restartAcceptanceServe();
    world.proxyRefusals = [];
    world.trustedHits = [];
    for (const providerId of ['opencode', 'command-code']) {
      await api('POST', `${prefix}/providers/${providerId}/models/refresh`, {}, { cas: true, success: false });
    }
    assert(world.trustedHits.length === 0, 'production feature-off binary moved a sealed origin onto the loopback');
    assert(world.proxyRefusals.includes('opencode.ai') && world.proxyRefusals.includes('api.commandcode.ai'), 'production feature-off did not keep the official origins behind the deny-external proxy');
  } catch (error) {
    workflowError = error;
  } finally {
    world.trustedSeamJson = '';
    try {
      await stopServe();
      if (originalDir) {
        await startServe(originalDir);
        await api('POST', `${prefix}/auth/login`, { username: 'cpa-acceptance', password: 'synthetic-cpa-admin-password' }, { cas: true });
        const runtime = (await api('GET', `${prefix}/external-integrations/cpa/runtime`)).value;
        requirePolicy(runtime);
      }
      await closeTrusted();
    } catch (error) {
      if (!workflowError) workflowError = error;
    }
  }
  if (workflowError) throw workflowError;
}

stage('trusted-quota-persistence', async () => {
  const pending = 'pending: sanctioned test seam is OCG_CPA_TEST_ENDPOINTS behind existing feature ollama-cloud-loopback-test. Feature-off builds keep official sealed origins. goat.rs catalog fetch and command_code_usage.rs official usage fetch call rewrite_url for the official URL only. Projection and Go usage already call it. Dashboard sourceUrl can remain the official models URL until providers.rs records the rewritten GET. The CLI feature forwarder is outside this lane. No test-feature binary, trusted lock, or compiler grant is in this run. Canonical Go prose is "Weekly usage limit reached. Resets in 4 days." Canonical GOAT JSON uses error.code RATE_LIMITED or error.type rate_limit_error and a future RFC3339 reset. Health status ok and GOAT calibration JSON are not restrictions. Ordinary explicit 429 must not stick. No manual database write, callback, fake provider, or fake policyReady. Exit 2 while this stage is pending is not closeout.';
  if (!options.testFeatureCli && !options.productionFeatureOff) throw blocked(pending);
  if (!trustedSha) throw blocked(`pending: the trusted artifact lock is not accepted, so this stage does not start a product process. ${pending}`);
  if (options.productionFeatureOff) {
    await assertProductionFeatureOff();
    return;
  }
  await runTrustedQuotaWorkflow();
});

stage('logs-and-usage', async () => {
  requirePolicy((await api('GET', `${prefix}/external-integrations/cpa/runtime`)).value);
  const logs = (await api('GET', `${prefix}/logs/forward?limit=50`)).value;
  const chat = (logs.items ?? []).find(item => item.requestedModel === 'ocg-cpa-chat' || item.model === 'ocg-cpa-chat' || item.upstreamModel === 'ocg-cpa-chat');
  assert(chat, 'forward logs have no ocg-cpa-chat row');
  assert(chat.promptTokens === 11 && chat.completionTokens === 7, 'forward log tokens are not the reported upstream usage');
  assert(chat.accountId === world.accountA || chat.credentialAccountId === world.accountA, 'forward log is not credential A');
  const usage = (await api('GET', `${prefix}/accounts/${world.accountA}/usage`)).value;
  assert(usage && typeof usage === 'object', 'account usage was not readable');
  assert((logs.summary?.promptTokens ?? 0) >= 11 && (logs.summary?.completionTokens ?? 0) >= 7, 'forward log summary does not include the reported usage');
});

stage('cas-failed-apply-rollback', async () => {
  requirePolicy((await api('GET', `${prefix}/external-integrations/cpa/runtime`)).value);
  const contract = (await api('GET', `${prefix}/contract`)).value;
  const stale = { expectedRevision: contract.revision, processGeneration: contract.processGeneration };
  await api('PATCH', `${prefix}/accounts/${world.accountA}`, { name: 'CPA acceptance renamed' }, { cas: true });
  const rejected = await api('PATCH', `${prefix}/accounts/${world.accountA}`, { ...stale, name: 'stale-name' }, { cas: true, success: false });
  assert(rejected.code !== 0, 'stale CAS was committed');
  assert((await api('GET', `${prefix}/accounts/${world.accountA}`)).value.name === 'CPA acceptance renamed', 'stale CAS overwrote the name');
  const missing = await api('PUT', `${prefix}/settings`, { conversationSticky: true }, { success: false });
  assert(missing.code !== 0, 'settings write without CAS was committed');
  const good = await readRuntime();
  const applied = digestOf(good.facts);
  const appliedRevision = revisionOf(good.facts, 'appliedRevision');
  const destination = (await api('GET', `${prefix}/destinations`)).value.destinations.find(row => row.id === world.destination.id);
  const runtime = (await api('GET', `${prefix}/external-integrations/cpa/runtime`)).value;
  assert(runtime.baseUrl, 'child base URL is absent');
  const models = (destination.catalog ?? []).map(model => ({
    publicModel: model.publicModel,
    upstreamModel: model.upstreamModel,
    enabled: model.enabled,
    ...(model.protocols ? { protocols: model.protocols } : {}),
    ...(model.preferred ? { preferred: model.preferred } : {}),
  }));
  const originalEndpoint = destination.protocolRoutes?.[0]?.endpointUrl ?? destination.baseUrl;
  const originalRoutes = (destination.protocolRoutes ?? []).map(route => ({
    protocol: route.protocol,
    endpointUrl: route.endpointUrl,
    authScheme: route.authScheme,
  }));
  const patched = await api('PATCH', `${prefix}/destinations/${destination.id}`, {
    name: destination.name,
    endpointUrl: runtime.baseUrl,
    upstreamProtocol: destination.protocols?.[0] ?? 'chat_completions',
    authScheme: 'bearer',
    protocolRoutes: originalRoutes.map(route => ({ ...route, endpointUrl: runtime.baseUrl })),
    models,
    authorizeCredentialIds: [world.credentialA, world.credentialB],
    enabled: true,
  }, { cas: true, success: false });
  if (patched.code === 0) {
    const heldDeadline = Date.now() + 8000;
    let held = await readRuntime();
    for (;;) {
      assert(String(digestOf(held.facts)) === String(applied), 'self-loop replaced the applied digest');
      if (statusOf(held.facts) === 'apply_failed' || Date.now() >= heldDeadline) break;
      await delay(250);
      held = await readRuntime();
    }
  }
  const kept = await readRuntime();
  assert(String(digestOf(kept.facts)) === String(applied), 'self-loop replaced the applied digest');
  if (patched.code === 0) {
    await api('PATCH', `${prefix}/destinations/${destination.id}`, {
      name: destination.name,
      endpointUrl: originalEndpoint,
      upstreamProtocol: destination.protocols?.[0] ?? 'chat_completions',
      authScheme: 'bearer',
      protocolRoutes: originalRoutes,
      models,
      authorizeCredentialIds: [world.credentialA, world.credentialB],
      enabled: true,
    }, { cas: true });
  }
  const mark = markArrivals();
  const still = await api('POST', '/v1/chat/completions', { model: 'ocg-cpa-chat', messages: [{ role: 'user', content: 'still-applied' }] }, { success: false, extra: ['--key-file', await gatewayKeyFile()] });
  const stillHits = since(mark);
  if (stillHits.length === 0) assert(still.code !== 0, 'self-loop unavailability returned success with zero sends');
  else assertSends(stillHits, { A: 1 }, 'previous applied projection');
  if (patched.code === 0) assertAutomaticApply(await waitAutomaticApply(appliedRevision), 'restored endpoint');
  const beforeSticky = await readRuntime();
  const beforeStickyRevision = revisionOf(beforeSticky.facts, 'appliedRevision');
  const beforeStickyDigest = digestOf(beforeSticky.facts);
  await api('PUT', `${prefix}/settings`, { conversationSticky: true }, { cas: true });
  const advanced = await waitAutomaticApply(beforeStickyRevision);
  if (advanced.phase !== 'applied' || String(digestOf(advanced.facts)) === String(beforeStickyDigest)) {
    throw fail('no second applied revision to roll back');
  }
  requirePolicy(advanced.value);
  const advancedDigest = digestOf(advanced.facts);
  const rolled = (await api('POST', `${prefix}/external-integrations/cpa/runtime/rollback`, {}, { cas: true })).value;
  requirePolicy(rolled);
  const rolledFacts = runtimeFacts(rolled);
  const rolledDigest = digestOf(rolledFacts);
  assert(String(rolledDigest) !== String(advancedDigest), 'rollback kept the candidate digest');
  await assertCatalogPlaneRollback();
});

async function readOwnedDestination(id) {
  const destinations = (await api('GET', `${prefix}/destinations`)).value?.destinations;
  if (!Array.isArray(destinations)) throw blocked('pending: destinations payload has no destinations array');
  return destinations.find(row => row.id === id) ?? null;
}
function catalogEditFromRow(row, enabled) {
  return {
    publicModel: row.publicModel,
    enabled,
    ...(row.protocols ? { protocols: row.protocols } : {}),
    ...(row.preferred ? { preferred: row.preferred } : {}),
  };
}
function explainPublicModels(explained) {
  return (explained?.eligible ?? []).map(candidate => candidate?.authority?.publicModel).filter(Boolean);
}
async function putDestinationCatalog(destinationId, updates) {
  return api('PUT', `${prefix}/destinations/${destinationId}/catalog`, { updates }, { cas: true });
}
async function assertCatalogPlaneRollback() {
  requirePolicy((await api('GET', `${prefix}/external-integrations/cpa/runtime`)).value);
  const destinationId = world.destination.id;
  const original = await readOwnedDestination(destinationId);
  assert(original, 'catalog-plane-rollback destination is absent');
  const originalRows = (original.catalog ?? []).filter(row => typeof row.publicModel === 'string' && row.publicModel);
  assert(originalRows.some(row => row.publicModel === PLANE_A && row.enabled !== false), 'plane-a is not in the saved catalog');
  assert(originalRows.some(row => row.publicModel === PLANE_B && row.enabled !== false), 'plane-b is not in the saved catalog');
  const key = await gatewayKeyFile();
  const restoreControls = async () => {
    const previous = revisionOf((await readRuntime()).facts, 'appliedRevision');
    await putDestinationCatalog(destinationId, originalRows.map(row => catalogEditFromRow(row, row.enabled !== false)));
    assertAutomaticApply(await waitAutomaticApply(previous), 'catalog-plane-rollback restore');
  };
  try {
    const beforeA = revisionOf((await readRuntime()).facts, 'appliedRevision');
    await putDestinationCatalog(destinationId, originalRows.map(row => catalogEditFromRow(row, row.publicModel === PLANE_A)));
    const acceptedA = assertAutomaticApply(await waitAutomaticApply(beforeA), 'catalog-plane A');
    const digestA = digestOf(acceptedA.facts);
    const appliedA = revisionOf(acceptedA.facts, 'appliedRevision');
    const explainedA = (await api('GET', `${prefix}/routing/explain?model=${encodeURIComponent(PLANE_A)}&clientProtocol=chat_completions`)).value;
    const modelsA = explainPublicModels(explainedA);
    assert(modelsA.includes(PLANE_A), 'accepted A explain omitted plane-a');
    assert(!modelsA.includes(PLANE_B), 'accepted A explain still listed plane-b');
    if (explainedA.ownedProjection?.applied?.digest) {
      assert(String(explainedA.ownedProjection.applied.digest) === String(digestA), 'accepted A explain digest is not the applied A digest');
    }
    const markA = markArrivals();
    const sendA = await api('POST', '/v1/chat/completions', {
      model: PLANE_A,
      messages: [{ role: 'user', content: 'plane-a' }],
    }, { extra: ['--key-file', key] });
    const hitsA = since(markA);
    assertSends(hitsA, { A: 1 }, 'catalog-plane A');
    assert(hitsA[0].model === PLANE_A, 'plane-a send used another upstream model');
    assert(renderedRun(sendA).includes('loopback-ok:plane-a'), 'plane-a response missed the unique marker');

    const beforeB = revisionOf((await readRuntime()).facts, 'appliedRevision');
    assert(String(beforeB) === String(appliedA), 'an intermediate plane occupied previous before B');
    await putDestinationCatalog(destinationId, [
      catalogEditFromRow(originalRows.find(row => row.publicModel === PLANE_A), true),
      catalogEditFromRow(originalRows.find(row => row.publicModel === PLANE_B), true),
    ]);
    const acceptedB = assertAutomaticApply(await waitAutomaticApply(beforeB), 'catalog-plane B');
    assert(String(digestOf(acceptedB.facts)) !== String(digestA), 'catalog-plane B kept the A digest');
    const markB = markArrivals();
    const sendB = await api('POST', '/v1/chat/completions', {
      model: PLANE_B,
      messages: [{ role: 'user', content: 'plane-b' }],
    }, { extra: ['--key-file', key] });
    const hitsB = since(markB);
    assertSends(hitsB, { A: 1 }, 'catalog-plane B');
    assert(hitsB[0].model === PLANE_B, 'plane-b send used another upstream model');
    assert(renderedRun(sendB).includes('loopback-ok:plane-b'), 'plane-b response missed the unique marker');

    const rolled = (await api('POST', `${prefix}/external-integrations/cpa/runtime/rollback`, {}, { cas: true })).value;
    requirePolicy(rolled);
    const rolledFacts = runtimeFacts(rolled);
    const rolledDigest = digestOf(rolledFacts);
    assert(String(rolledDigest) === String(digestA), 'rollback did not restore the A digest');
    const explainedRolledA = (await api('GET', `${prefix}/routing/explain?model=${encodeURIComponent(PLANE_A)}&clientProtocol=chat_completions`)).value;
    assert(explainPublicModels(explainedRolledA).includes(PLANE_A), 'rollback A explain omitted plane-a');
    const explainedRolledB = (await api('GET', `${prefix}/routing/explain?model=${encodeURIComponent(PLANE_B)}&clientProtocol=chat_completions`)).value;
    const saved = await readOwnedDestination(destinationId);
    assert((saved?.catalog ?? []).some(row => row.publicModel === PLANE_B && row.enabled !== false), 'rollback deleted saved plane-b from current configuration');
    const rolledBModels = explainPublicModels(explainedRolledB);
    assert(!rolledBModels.includes(PLANE_B) || explainedRolledB.resolved?.routeable === false, 'rollback still admitted plane-b on the applied plane');

    const markAgainA = markArrivals();
    const againA = await api('POST', '/v1/chat/completions', {
      model: PLANE_A,
      messages: [{ role: 'user', content: 'plane-a-after-rollback' }],
    }, { extra: ['--key-file', key] });
    assertSends(since(markAgainA), { A: 1 }, 'rollback A');
    assert(renderedRun(againA).includes('loopback-ok:plane-a'), 'rollback A missed the unique marker');

    const markDenyB = markArrivals();
    const denyB = await api('POST', '/v1/chat/completions', {
      model: PLANE_B,
      messages: [{ role: 'user', content: 'plane-b-after-rollback' }],
    }, { success: false, extra: ['--key-file', key] });
    assert(denyB.code !== 0, 'rollback B was accepted');
    assert(since(markDenyB).length === 0, `rollback B sent ${since(markDenyB).length} times while applied A`);
  } finally {
    await restoreControls();
  }
  results.push({
    name: 'catalog-plane-rollback',
    status: 'PASS',
    evidenceLevel: 'operator-runtime',
    detail: 'A={ocg-cpa-plane-a} then one catalog mutation B={plane-a,plane-b}; rollback restored applied A: plane-a physical allow and plane-b zero-I/O deny while plane-b remained saved',
  });
}

stage('selected-candidate-tests', async () => {
  requirePolicy((await api('GET', `${prefix}/external-integrations/cpa/runtime`)).value);
  const probes = [
    ['account-model-test', 'POST', `${prefix}/accounts/${world.accountA}/model-tests`, { modelId: 'ocg-cpa-probe' }, false, 'A'],
    ['destination-model-test', 'POST', `${prefix}/destinations/${world.destination.id}/model-tests`, { publicModel: 'ocg-cpa-probe', protocol: 'chat_completions' }, true, 'A'],
    ['verify', 'POST', `${prefix}/accounts/${world.accountB}/verify`, {}, true, 'B'],
  ];
  for (const [label, method, path, body, cas, expected] of probes) {
    const mark = markArrivals();
    const run = await api(method, path, body, { cas, success: false });
    const hits = since(mark);
    assert(run.code === 0 || hits.length > 0, `${label} neither succeeded nor reached a provider`);
    if (hits.length === 0) throw fail(`${label} completed without an upstream send`);
    assert(hits.every(item => item.label === expected), `${label} contacted ${JSON.stringify(counts(hits))} instead of ${expected}`);
    assert(hits.length === 1, `${label} sent ${hits.length} times`);
    assert(hits.every(item => item.forbiddenHeader === false && item.label !== 'client'), `${label} leaked a client credential or correlation header`);
  }
  const customMark = markArrivals();
  const customProbe = await api('POST', `${prefix}/providers/${world.providerId}/protocol-probes`, {
    accountId: world.accountA,
    modelId: 'ocg-cpa-probe',
    protocols: ['chat_completions'],
  }, { cas: true, success: false });
  const customError = cliError(customProbe.stderr);
  const customText = `${customError.message ?? ''}\n${renderedRun(customProbe)}`;
  assert(customProbe.code !== 0, 'custom provider-wide probe was accepted');
  assert(since(customMark).length === 0, 'custom provider-wide probe sent upstream');
  assert(/account-owned/i.test(customText), 'custom provider-wide probe was not the account-owned rejection');
  const supported = await supportedLoopbackProbe();
  if (!supported) {
    const detail = 'pending: no provider with protocolProbe is bound to this loopback. Supported classes are opencode, opencode-zen-free, command-code, minimax, and kimi. Custom provider-wide probe is the zero-send account-owned rejection. accountId on the probe DTO stays ignored. Do not create a provider for this harness.';
    results.push({ name: 'supported-provider-probe', status: 'BLOCKED', detail });
    console.log(`BLOCKED supported-provider-probe: ${detail}`);
  } else {
    const mark = markArrivals();
    const run = await api('POST', `${prefix}/providers/${supported.providerId}/protocol-probes`, {
      accountId: supported.accountId,
      modelId: supported.modelId,
      protocols: ['chat_completions'],
    }, { cas: true, success: false });
    const hits = since(mark);
    assert(run.code === 0 || hits.length > 0, 'supported provider probe neither succeeded nor reached a provider');
    assert(hits.length === 1, `supported provider probe sent ${hits.length} times`);
    assert(hits.every(item => item.forbiddenHeader === false && item.label !== 'client'), 'supported provider probe leaked a client credential or correlation header');
  }
  const beforeScope = revisionOf((await readRuntime()).facts, 'appliedRevision');
  await api('PATCH', `${prefix}/bindings/${world.bindingA}`, { modelScope: { kind: 'only', models: ['ocg-cpa-chat'] } }, { cas: true });
  await api('PATCH', `${prefix}/bindings/${world.bindingB}`, { modelScope: { kind: 'only', models: ['ocg-cpa-messages'] } }, { cas: true });
  assertAutomaticApply(await waitAutomaticApply(beforeScope), 'model scope');
  const chatMark = markArrivals();
  await api('POST', '/v1/chat/completions', { model: 'ocg-cpa-chat', messages: [{ role: 'user', content: 'scope-a' }] }, { extra: ['--key-file', await gatewayKeyFile()] });
  assertSends(since(chatMark), { A: 1 }, 'scope chat');
  const messagesMark = markArrivals();
  await api('POST', '/v1/messages', { model: 'ocg-cpa-messages', max_tokens: 16, messages: [{ role: 'user', content: 'scope-b' }] }, { extra: ['--key-file', world.gatewayKeyFile] });
  assertSends(since(messagesMark), { B: 1 }, 'scope messages');
  const beforeRestore = revisionOf((await readRuntime()).facts, 'appliedRevision');
  await api('PATCH', `${prefix}/bindings/${world.bindingA}`, { modelScope: { kind: 'all' } }, { cas: true });
  await api('PATCH', `${prefix}/bindings/${world.bindingB}`, { modelScope: { kind: 'all' } }, { cas: true });
  assertAutomaticApply(await waitAutomaticApply(beforeRestore), 'model scope restore');
});

stage('backup-restore-clean-stop', async () => {
  assert(!Object.prototype.hasOwnProperty.call(environment(), 'MANAGEMENT_PASSWORD'), 'backup and restore exported MANAGEMENT_PASSWORD');
  const snapshot = join(world.root, 'profile.snapshot');
  const childPort = (await api('GET', `${prefix}/external-integrations/cpa/runtime`)).value.port;
  await stopServe();
  const created = await invoke(['--data-dir', world.dataDir, 'backup', 'create', '--output', snapshot], { timeoutMs: 60000 });
  assert(created.code === 0, created.stderr);
  const receipt = parseJson(created.stdout);
  assert(receipt.state === 'created' && receipt.count > 0, 'backup create receipt was not a snapshot');
  const restoredDir = join(world.root, 'restored');
  await mkdir(restoredDir, { recursive: true });
  const restored = await invoke(['--data-dir', restoredDir, 'backup', 'restore', '--input', snapshot], { timeoutMs: 60000 });
  assert(restored.code === 0, restored.stderr);
  assert(parseJson(restored.stdout).state === 'restored', 'backup restore receipt was not restored');
  await startServe(restoredDir);
  await api('POST', `${prefix}/auth/login`, { username: 'cpa-acceptance', password: 'synthetic-cpa-admin-password' }, { cas: true });
  const runtime = (await api('GET', `${prefix}/external-integrations/cpa/runtime`)).value;
  requirePolicy(runtime);
  const mark = markArrivals();
  await api('POST', '/v1/chat/completions', { model: 'ocg-cpa-chat', messages: [{ role: 'user', content: 'restored' }] }, { extra: ['--key-file', await gatewayKeyFile()] });
  assertSends(since(mark), { A: 1 }, 'restored inference');
  const restoredChild = runtime.port;
  await stopServe();
  await delay(500);
  assert(listenerPids(restoredChild).length === 0, 'clean stop left the restored child listening');
  assert(listenerPids(childPort).length === 0, 'clean stop left the original child listening');
});

stage('stopped-key-ping', async () => {
  assert(listenerPids(world.port).length === 0, 'serve was still listening');
  const mark = markArrivals();
  const ping = await invoke([
    '--data-dir', world.dataDir, '--endpoint', world.endpoint, 'key', 'ping', world.accountA,
    '--model', 'ocg-cpa-chat', '--message', 'ping', '--max-tokens', '3',
  ], { timeoutMs: 30000 });
  assert(ping.code !== 0, 'key ping succeeded while serve was stopped');
  assert(since(mark).length === 0, 'stopped key ping contacted the provider');
  assert(!/sk-|secret|bearer /i.test(ping.stderr), 'stopped key ping printed credential material');
  if (!/requires a running serve|requires the running serve|service required/i.test(ping.stderr)) {
    throw blocked('stopped key ping exited nonzero without proving it is service-required. The command still takes the offline data-directory lock, and a custom credential rejected before send is not that proof');
  }
});

const nativeWorkRoot = join(repo, 'tmp', 'ocg3-cli-delivery', 'orchestration-20261004', 'native-harness-work');
const NATIVE_HOOK = {
  feature: 'ollama-cloud-loopback-test',
  env: 'OCG_CPA_TEST_ENDPOINTS',
  goTag: 'ocg_native_loopback_fixture',
  existingKeys: ['opencode', 'command-code'],
  keys: ['codex', 'anthropic', 'kimi.com', 'kimi.ai', 'xai.cli', 'xai.api', 'antigravity'],
};
const NATIVE_MODES = [
  { family: 'codex', lane: 'cli-import', provider: 'codex', mapKey: 'codex' },
  { family: 'anthropic', lane: 'cli-import', provider: 'anthropic', mapKey: 'anthropic' },
  { family: 'kimi.com', lane: 'cli-import', provider: 'kimi', mapKey: 'kimi.com' },
  { family: 'kimi.ai', lane: 'staged-cpa-auth', provider: 'kimi', mapKey: 'kimi.ai' },
  { family: 'xai.cli', lane: 'cli-import', provider: 'xai', mapKey: 'xai.cli' },
  { family: 'xai.api', lane: 'staged-cpa-auth', provider: 'xai', mapKey: 'xai.api' },
  { family: 'antigravity', lane: 'staged-cpa-auth', provider: 'antigravity', mapKey: 'antigravity' },
];
const NATIVE_PENDING_INTERFACES = [
  ['source-compact', 'unresolved operator entry point for NativeSourceOperation::SourceCompact except the existing public Antigravity Responses compaction_trigger. This is harness/SDK selector evidence, not a finding that the product lacks the operation'],
  ['internal', 'unresolved operator entry point for NativeSourceOperation::Internal. Public Antigravity compaction carries a parent execute/stream permit and is not automatic internal child evidence. This is harness evidence, not a finding that the product lacks the operation'],
  ['refresh-resend', 'unresolved operator entry point. Token refresh and acquisition URLs stay unmapped, and this generation map is not refresh proof'],
  ['stream-refresh', 'unresolved operator entry point. Stream-refresh is not a second client send and this is not refresh proof'],
  ['original-host', 'unresolved operator entry point. No operator control sets the CPA child outbound Host independently of the URL'],
  ['generation-kinds', 'unresolved operator entry point. The binding DTO exposes allowedEndpointIds and allowedOrigins and does not expose generationKinds'],
  ['stream-bootstrap', 'unresolved operator entry point until the owned child config shows Codex.StreamBootstrapBuffering enabled and a Codex responses stream runs that path. This is harness enablement evidence, not a finding that the product lacks bootstrap'],
];
const NATIVE_SDK_REQUIRED_INTERFACES = [
  'source-compact',
  'internal',
  'refresh-resend',
  'stream-refresh',
  'original-host',
  'generation-kinds',
  'stream-bootstrap',
];
const NATIVE_ROOT_ENV = {
  codex: 'CODEX_HOME',
  claude: 'CLAUDE_CONFIG_DIR',
  kimi: 'KIMI_CODE_HOME',
  grok: 'GROK_HOME',
};
const NATIVE_OFFICIAL_HOSTS = [
  'chatgpt.com',
  'api.anthropic.com',
  'api.kimi.com',
  'api.kimi.ai',
  'api.x.ai',
  'cli-chat-proxy.grok.com',
  'daily-cloudcode-pa.googleapis.com',
];
const NATIVE_CALLABLES = ['chat_completions', 'responses', 'messages'];
const NATIVE_OPERATIONS = ['execute', 'stream', 'count-tokens', 'internal', 'compact'];
const NATIVE_PROVIDER = {
  codex: 'codex',
  anthropic: 'anthropic',
  'kimi.com': 'kimi',
  'kimi.ai': 'kimi',
  'xai.cli': 'xai',
  'xai.api': 'xai',
  antigravity: 'antigravity',
};
const NATIVE_REGISTRY_MODELS = {
  codex: ['gpt-5.5', 'gpt-6-astra', 'gpt-6-sol', 'gpt-6.1-sol', 'gpt-6-luna', 'gpt-5.6-sol', 'gpt-5.6-terra', 'gpt-5.6-luna', 'codex-auto-review'],
  anthropic: ['claude-haiku-4-5-20251001', 'claude-sonnet-4-5-20250929', 'claude-sonnet-4-6', 'claude-opus-4-6', 'claude-opus-4-7', 'claude-opus-4-8', 'claude-opus-5', 'claude-sonnet-5', 'claude-fable-5', 'claude-fable-5-1', 'claude-opus-5-5', 'claude-sonnet-5-5', 'claude-opus-4-5-20251101', 'claude-opus-4-1-20250805', 'claude-opus-4-20250514', 'claude-sonnet-4-20250514', 'claude-3-7-sonnet-20250219', 'claude-3-5-haiku-20241022'],
  'kimi.com': ['kimi-k2', 'kimi-k2-thinking', 'kimi-k2.5', 'kimi-k2.6', 'kimi-k2.7-code', 'kimi-k2.7-code-highspeed', 'kimi-k2.8', 'kimi-k2.8-code', 'kimi-k3', 'kimi-k3-256k'],
  'kimi.ai': ['kimi-k2', 'kimi-k2-thinking', 'kimi-k2.5', 'kimi-k2.6', 'kimi-k2.7-code', 'kimi-k2.7-code-highspeed', 'kimi-k2.8', 'kimi-k2.8-code', 'kimi-k3', 'kimi-k3-256k'],
  'xai.cli': ['grok-4.7', 'grok-4.7-build-fast', 'grok-4.6', 'grok-build-0.1', 'grok-4.5', 'grok-4.3', 'grok-4.20-0309-reasoning', 'grok-4.20-0309-non-reasoning', 'grok-4.20-multi-agent-0309', 'grok-3-mini', 'grok-3-mini-fast', 'grok-composer-2.5-fast'],
  'xai.api': ['grok-4.7', 'grok-4.7-build-fast', 'grok-4.6', 'grok-build-0.1', 'grok-4.5', 'grok-4.3', 'grok-4.20-0309-reasoning', 'grok-4.20-0309-non-reasoning', 'grok-4.20-multi-agent-0309', 'grok-3-mini', 'grok-3-mini-fast', 'grok-composer-2.5-fast'],
  antigravity: ['claude-opus-4-6-thinking', 'claude-sonnet-4-6', 'gemini-3.6-flash-high', 'gemini-3.7-flash-high', 'gemini-3.8-flash-high', 'gemini-3-flash', 'gemini-3.1-flash-image', 'gemini-pro-agent', 'gemini-3.1-pro-low', 'gpt-oss-120b-medium', 'gemini-3.1-flash-lite', 'gemini-3.5-flash-lite'],
};
const NATIVE_METADATA_LOOPBACK_IS_AUTHORITY = false;
const NATIVE_ZERO_SENDS = [
  'forbidden-protocol',
  'revoked-grant',
  'wrong-mode',
  'wrong-base',
  'stale-version',
  'native-presence',
  'native-local-count',
  'source-refusal',
  'unknown-variant',
  'map-after-apply',
  'wrong-port',
];
const NATIVE_ONE_HITS = ['loss', 'truncate', 'empty', 'cancel', 'deadline'];
const NATIVE_STAGED_FILES = {
  'kimi.ai': 'ocg-staged-kimi-ai.json',
  'xai.api': 'ocg-staged-xai-api.json',
  antigravity: 'ocg-staged-antigravity.json',
  'antigravity-b': 'ocg-staged-antigravity-b.json',
};
const NATIVE_IMPORT_PROVIDERS = ['codex', 'anthropic', 'kimi', 'xai'];
const NATIVE_DISCOVERY = [
  { provider: 'codex', source: 'Codex CLI', supported: true },
  { provider: 'anthropic', source: 'Claude Code', supported: true },
  {
    provider: 'antigravity',
    source: 'Antigravity',
    supported: false,
    reason: 'Antigravity local credential storage is not compatible with this import; use login.',
  },
  { provider: 'kimi', source: 'Kimi Code', supported: true },
  { provider: 'xai', source: 'Grok CLI', supported: true },
];
const NATIVE_LOCK_FIXTURE_NAMES = [
  'missing-lock',
  'empty-lock',
  'invalid-lock-json',
  'schema-example-is-not-accepted-data',
  'self-consistent-manifest-hash-is-not-trust',
  'identity-mismatch-sourceCommit',
  'identity-mismatch-sourceVersion',
  'identity-mismatch-protocolVersion',
  'identity-mismatch-buildIdentity',
  'identity-mismatch-hostSHA256',
  'identity-mismatch-overlaySHA256',
  'omitted-buildIdentity-is-not-inferred',
  'platform-mismatch',
  'compiled-only-is-not-acceptance',
  'missing-required-capability',
  'placeholder-baff-is-not-a-record',
  'multiple-host-suite-records',
  'uppercase-record-sha-rejected',
  'executable-must-be-a-basename',
  'empty-required-capabilities',
  'host-dir-cannot-supply-the-lock',
  'missing-variant-rejected',
  'unknown-variant-rejected',
  'duplicate-platform-variant-rejected',
  'fixture-non-windows-rejected',
  'feature-off-selects-production',
  'feature-on-selects-fixture',
  'production-bytes-do-not-satisfy-fixture',
  'fixture-bytes-do-not-satisfy-production',
  'manifest-variant-cannot-select-trust',
  'manifest-build-tags-must-match-variant',
  'same-frozen-identity-for-both-variants',
  'manifest-acceptance-flag-is-not-trust',
  'production-and-fixture-bytes-must-differ',
  'well-formed-lock-accepted',
  'normalized-win32-amd64-record-accepted',
  'compiled-only-sibling-does-not-become-trust',
  'absent-real-lock-is-not-trusted',
  'fixture-did-not-create-lock',
  'no-product-process',
];
const NATIVE_PENDING = 'pending: the root trusted artifact lock is absent or the selected compile-mode variant is unaccepted. Native product, listen, and child processes stay stopped. The dictionary contract is native-harness-work/rust-hook-api.md. A metadata base_url is not a grant.';

function nativeMatrixParts(url) {
  const parsed = new URL(url);
  return {
    url,
    path: parsed.pathname,
    query: parsed.search.startsWith('?') ? parsed.search.slice(1) : '',
    host: parsed.hostname,
    origin: `${parsed.protocol}//${parsed.hostname}`,
  };
}
const NATIVE_MATRIX = {
  C1: nativeMatrixParts('https://chatgpt.com/backend-api/codex/responses'),
  C2: nativeMatrixParts('https://chatgpt.com/backend-api/codex/responses/compact'),
  A1: nativeMatrixParts('https://api.anthropic.com/v1/messages?beta=true'),
  A2: nativeMatrixParts('https://api.anthropic.com/v1/messages/count_tokens?beta=true'),
  Kc1: nativeMatrixParts('https://api.kimi.com/coding/v1/chat/completions'),
  Kc2: nativeMatrixParts('https://api.kimi.com/coding/v1/responses'),
  Kc3: nativeMatrixParts('https://api.kimi.com/coding/v1/messages?beta=true'),
  Kc4: nativeMatrixParts('https://api.kimi.com/coding/v1/messages/count_tokens?beta=true'),
  Ka1: nativeMatrixParts('https://api.kimi.ai/coding/v1/chat/completions'),
  Ka2: nativeMatrixParts('https://api.kimi.ai/coding/v1/responses'),
  Ka3: nativeMatrixParts('https://api.kimi.ai/coding/v1/messages?beta=true'),
  Ka4: nativeMatrixParts('https://api.kimi.ai/coding/v1/messages/count_tokens?beta=true'),
  X1: nativeMatrixParts('https://cli-chat-proxy.grok.com/v1/responses'),
  X2: nativeMatrixParts('https://api.x.ai/v1/responses'),
  X3: nativeMatrixParts('https://api.x.ai/v1/responses/compact'),
  G1: nativeMatrixParts('https://daily-cloudcode-pa.googleapis.com/v1internal:generateContent'),
  G2: nativeMatrixParts('https://daily-cloudcode-pa.googleapis.com/v1internal:streamGenerateContent?alt=sse'),
  G3: nativeMatrixParts('https://daily-cloudcode-pa.googleapis.com/v1internal:countTokens'),
};
const NATIVE_EXPECTED_BASES = {
  codex: 'https://chatgpt.com/backend-api/codex',
  anthropic: 'https://api.anthropic.com',
  'kimi.com': 'https://api.kimi.com/coding',
  'kimi.ai': 'https://api.kimi.ai/coding',
  'xai.cli': 'https://cli-chat-proxy.grok.com/v1',
  'xai.api': 'https://api.x.ai/v1',
  antigravity: 'https://daily-cloudcode-pa.googleapis.com',
};

function streamedNonstream(model) {
  const text = String(model ?? '');
  return text.toLowerCase().includes('claude') || text.includes('gemini-3-pro') || text.includes('gemini-3.1-flash-image');
}
function nativeRegistryModels(family) {
  return NATIVE_REGISTRY_MODELS[family] ?? [];
}
function nativeScenarioModel(family, branch = 'ordinary') {
  const models = nativeRegistryModels(family);
  if (family !== 'antigravity') return models[0] ?? '';
  const selected = models.filter(model => branch === 'streamed' ? streamedNonstream(model) : !streamedNonstream(model));
  return selected[0] ?? '';
}
function nativeBranchOf(cell) {
  return cell?.family === 'antigravity' && streamedNonstream(cell.model) ? 'streamed' : 'ordinary';
}
function nativeNetwork(target, prefixGap = false) {
  return { outcome: 'network', target, prefixGap };
}
function nativeLocal() {
  return { outcome: 'local', target: 'LOCAL', prefixGap: false };
}
function nativeRefuse() {
  return { outcome: 'refuse', target: 'REFUSE', prefixGap: false };
}
function nativeRow(family, protocol, operation, model) {
  if (!NATIVE_CALLABLES.includes(protocol)) return nativeRefuse();
  if (family === 'codex') {
    if (operation === 'count-tokens') return nativeLocal();
    if (operation === 'compact') return nativeNetwork('C2');
    if (operation === 'execute' || operation === 'stream' || operation === 'internal') return nativeNetwork('C1');
  }
  if (family === 'anthropic') {
    if (operation === 'count-tokens') return nativeNetwork('A2');
    if (operation === 'compact') return nativeRefuse();
    if (operation === 'execute' || operation === 'stream' || operation === 'internal') return nativeNetwork('A1');
  }
  if (family === 'kimi.com' || family === 'kimi.ai') {
    const ids = family === 'kimi.com'
      ? { chat_completions: 'Kc1', responses: 'Kc2', messages: 'Kc3', count: 'Kc4' }
      : { chat_completions: 'Ka1', responses: 'Ka2', messages: 'Ka3', count: 'Ka4' };
    if (operation === 'compact') return nativeRefuse();
    if (operation === 'count-tokens') return nativeNetwork(ids.count);
    if (operation === 'execute' || operation === 'stream' || operation === 'internal') return nativeNetwork(ids[protocol]);
  }
  if (family === 'xai.cli' || family === 'xai.api') {
    if (operation === 'count-tokens') return nativeLocal();
    if (operation === 'compact') return nativeNetwork('X3', family === 'xai.cli');
    if (operation === 'execute' || operation === 'stream' || operation === 'internal') {
      return nativeNetwork(family === 'xai.cli' ? 'X1' : 'X2');
    }
  }
  if (family === 'antigravity') {
    if (operation === 'count-tokens') return nativeNetwork('G3');
    if (operation === 'stream') return nativeNetwork('G2');
    if (operation === 'execute' || operation === 'internal' || operation === 'compact') {
      return nativeNetwork(streamedNonstream(model) ? 'G2' : 'G1');
    }
  }
  return nativeRefuse();
}
function nativeMode(family) {
  return NATIVE_MODES.find(item => item.family === family) ?? null;
}
function nativeOutcome(family, protocol, operation, model) {
  return nativeRow(family, protocol, operation, model);
}
function nativeImportClass(family) {
  const mode = nativeMode(family);
  if (family === 'antigravity') return 'unsupported-local-file';
  if (mode?.lane === 'staged-cpa-auth') return 'staged-cpa-auth';
  if (family === 'kimi.com') return 'source-default-domain';
  if (family === 'xai.cli') return 'source-default-cli';
  return 'file';
}
function nativeDisposition(family, row) {
  if (row.outcome === 'refuse' || row.target === 'REFUSE') return 'source-refusal';
  if (row.outcome === 'local') return 'local-unsent';
  if (row.outcome !== 'network') return 'source-refusal';
  return nativeMode(family)?.lane === 'staged-cpa-auth' ? 'staged-generation' : 'cli-import-generation';
}
function publicAntigravityCompaction(cell) {
  return cell?.family === 'antigravity' && cell?.protocol === 'responses' && cell?.ingress === 'responses' && cell?.operation === 'compact';
}
function nativeClientRequest(cell) {
  if (!cell || cell.outcome === 'refuse' || cell.target === 'REFUSE' || cell.ingress === 'source-alt') return null;
  if (cell.operation === 'internal') return null;
  if (cell.operation === 'compact') {
    if (!publicAntigravityCompaction(cell)) return null;
    const model = cell.model;
    const stream = streamedNonstream(model);
    return {
      path: '/v1/responses',
      body: {
        model,
        stream,
        input: [
          { type: 'message', role: 'user', content: [{ type: 'input_text', text: stream ? 'native-compact-stream' : 'native-compact' }] },
          { type: 'compaction_trigger' },
        ],
      },
      kind: stream ? 'sse' : 'json',
      parentPermit: stream ? 'stream' : 'execute',
      internalChild: false,
    };
  }
  const model = cell.model;
  const stream = cell.operation === 'stream';
  if (cell.ingress === 'gemini-countTokens') {
    return {
      path: nativeGeminiPath(model, 'countTokens'),
      body: { contents: [{ role: 'user', parts: [{ text: 'native-count' }] }] },
      kind: 'count',
    };
  }
  if (cell.operation === 'count-tokens') {
    if (cell.protocol === 'messages' || cell.target === 'A2' || cell.target === 'Kc4' || cell.target === 'Ka4' || cell.target === 'LOCAL') {
      return {
        path: '/v1/messages/count_tokens',
        body: { model, messages: [{ role: 'user', content: 'native-count' }] },
        kind: 'count',
      };
    }
    return {
      path: nativeGeminiPath(model, 'countTokens'),
      body: { contents: [{ role: 'user', parts: [{ text: 'native-count' }] }] },
      kind: 'count',
    };
  }
  if (cell.ingress === 'gemini-generateContent') {
    return {
      path: nativeGeminiPath(model, 'generateContent'),
      body: { contents: [{ role: 'user', parts: [{ text: 'native-execute' }] }] },
      kind: 'json',
    };
  }
  if (cell.ingress === 'gemini-streamGenerateContent') {
    return {
      path: nativeGeminiPath(model, 'streamGenerateContent'),
      body: { contents: [{ role: 'user', parts: [{ text: 'native-stream' }] }] },
      kind: 'sse',
    };
  }
  if (cell.protocol === 'messages') {
    return {
      path: '/v1/messages',
      body: { model, max_tokens: 16, stream, messages: [{ role: 'user', content: stream ? 'native-stream' : 'native-execute' }] },
      kind: stream ? 'sse' : 'json',
    };
  }
  if (cell.protocol === 'responses') {
    return {
      path: '/v1/responses',
      body: { model, stream, input: stream ? 'native-stream' : 'native-execute' },
      kind: stream ? 'sse' : 'json',
    };
  }
  return {
    path: '/v1/chat/completions',
    body: { model, stream, messages: [{ role: 'user', content: stream ? 'native-stream' : 'native-execute' }] },
    kind: stream ? 'sse' : 'json',
  };
}
function nativeCell(family, protocol, operation, model, ingress) {
  const row = nativeOutcome(family, protocol, operation, model);
  const matrix = NATIVE_MATRIX[row.target];
  return {
    family,
    ingress,
    protocol,
    operation,
    model,
    outcome: row.outcome,
    target: row.target,
    prefixGap: row.prefixGap,
    method: matrix ? 'POST' : null,
    url: matrix?.url ?? null,
    path: matrix?.path ?? null,
    query: matrix ? matrix.query : null,
    importClass: nativeImportClass(family),
    disposition: nativeDisposition(family, row),
    liveSend: row.outcome !== 'refuse' && nativeClientRequest({
      family, ingress, protocol, operation, model, outcome: row.outcome, target: row.target,
    }) != null,
  };
}
function nativeCatalog() {
  const families = ['codex', 'anthropic', 'kimi.com', 'kimi.ai', 'xai.cli', 'xai.api', 'antigravity'];
  const cells = [];
  for (const family of families) {
    for (const protocol of NATIVE_CALLABLES) {
      for (const operation of NATIVE_OPERATIONS) cells.push(nativeCell(family, protocol, operation, nativeScenarioModel(family), protocol));
    }
    for (const [ingress, operation] of [
      ['gemini-generateContent', 'execute'],
      ['gemini-streamGenerateContent', 'stream'],
      ['gemini-countTokens', 'count-tokens'],
    ]) cells.push(nativeCell(family, 'chat_completions', operation, nativeScenarioModel(family), ingress));
    if (family === 'antigravity') {
      const streamedModel = nativeScenarioModel(family, 'streamed');
      for (const protocol of NATIVE_CALLABLES) {
        for (const operation of ['execute', 'internal', 'compact']) {
          cells.push(nativeCell(family, protocol, operation, streamedModel, protocol));
        }
      }
      cells.push(nativeCell(family, 'chat_completions', 'execute', streamedModel, 'gemini-generateContent'));
      cells.push({
        family,
        ingress: 'source-alt',
        protocol: 'chat_completions',
        operation: 'stream',
        model: nativeScenarioModel(family),
        outcome: 'refuse',
        target: 'REFUSE',
        prefixGap: false,
        method: null,
        url: null,
        path: null,
        query: 'alt=other',
        importClass: 'unsupported-local-file',
        disposition: 'source-refusal',
        liveSend: false,
      });
    }
  }
  return cells;
}
function nativeFindCell(catalog, family, ingress, operation, model) {
  return catalog.find(cell => cell.family === family && cell.ingress === ingress && cell.operation === operation && cell.model === model);
}
function nativeCellId(cell) {
  return [cell.family, cell.ingress, cell.protocol, cell.operation, cell.model].join('|');
}
function nativeScenarioPlan() {
  const items = [];
  for (const cell of nativeCatalog()) {
    const id = nativeCellId(cell);
    if (cell.outcome === 'refuse' || cell.disposition === 'source-refusal' || cell.target === 'REFUSE') {
      items.push({
        id,
        kind: 'source-refusal',
        interface: '',
        cell,
        reason: 'SOURCE classification: source matrix refuses this cell; the harness opens no URL and this is not a runtime refusal',
        evidenceLevel: 'source-classification',
      });
      continue;
    }
    if (publicAntigravityCompaction(cell) && cell.outcome === 'network') {
      items.push({
        id,
        kind: 'network',
        interface: '',
        cell,
        reason: 'public Antigravity Responses compaction_trigger; parent execute/stream permit, not automatic internal child',
        evidenceLevel: 'operator-runtime',
      });
      continue;
    }
    if ((cell.operation === 'compact' || cell.operation === 'internal') && cell.outcome === 'network') {
      const iface = cell.operation === 'compact' ? 'source-compact' : 'internal';
      const reason = NATIVE_PENDING_INTERFACES.find(item => item[0] === iface)?.[1] ?? iface;
      items.push({ id, kind: 'pending', interface: iface, cell, reason, evidenceLevel: 'required-sdk-host' });
      continue;
    }
    if (cell.liveSend && cell.outcome === 'local') {
      items.push({ id, kind: 'local-unsent', interface: '', cell, reason: '', evidenceLevel: 'operator-runtime' });
      continue;
    }
    if (cell.liveSend && cell.outcome === 'network') {
      items.push({ id, kind: 'network', interface: '', cell, reason: '', evidenceLevel: 'operator-runtime' });
      continue;
    }
    items.push({
      id,
      kind: 'pending',
      interface: 'unclassified',
      cell,
      reason: `pending: ${id} has no client surface and is not a source refusal`,
      evidenceLevel: 'operator-runtime',
    });
  }
  for (const [iface, reason] of NATIVE_PENDING_INTERFACES) {
    if (iface === 'source-compact' || iface === 'internal') continue;
    items.push({
      id: `interface:${iface}`,
      kind: 'pending',
      interface: iface,
      cell: null,
      reason,
      evidenceLevel: NATIVE_SDK_REQUIRED_INTERFACES.includes(iface) ? 'required-sdk-host' : 'operator-runtime',
    });
  }
  return items;
}
function nativePlanCoverage(plan = nativeScenarioPlan()) {
  const count = kind => plan.filter(item => item.kind === kind).length;
  return {
    catalogCells: nativeCatalog().length,
    network: count('network'),
    localUnsent: count('local-unsent'),
    sourceRefusal: count('source-refusal'),
    pending: count('pending'),
    runnable: count('network') + count('local-unsent'),
    publicCompaction: plan.filter(item => item.cell && publicAntigravityCompaction(item.cell) && item.kind === 'network').length,
    sdkRequired: plan.filter(item => item.evidenceLevel === 'required-sdk-host').length,
    interfaces: [...new Set(plan.filter(item => item.kind === 'pending').map(item => item.interface).filter(Boolean))],
  };
}
function nativeCliAccountName(provider) {
  if (provider === 'codex') return nativeImportName('codex', 'synthetic-codex-account');
  if (provider === 'anthropic') return nativeImportName('claude', 'synthetic-claude-refresh');
  if (provider === 'kimi') return nativeImportName('kimi', 'synthetic-kimi-refresh');
  if (provider === 'xai') return nativeImportName('xai', 'synthetic-xai-principal');
  return '';
}
function stagedAuthDocument(kind) {
  const expired = '2030-01-01T00:00:00Z';
  if (kind === 'kimi.ai') {
    return { type: 'kimi', domain: 'kimi.ai', access_token: 'synthetic-kimi-ai-access', refresh_token: 'synthetic-kimi-ai-refresh', expired };
  }
  if (kind === 'xai.api') {
    return {
      type: 'xai', auth_kind: 'oauth', using_api: true, token_type: 'Bearer',
      access_token: 'synthetic-xai-api-access', refresh_token: 'synthetic-xai-api-refresh', expired,
    };
  }
  if (kind === 'antigravity') {
    return {
      type: 'antigravity', access_token: 'synthetic-ag-access', refresh_token: 'synthetic-ag-refresh',
      expired, project_id: 'synthetic-ag-project',
    };
  }
  if (kind === 'antigravity-b') {
    return {
      type: 'antigravity', access_token: 'synthetic-ag-b-access', refresh_token: 'synthetic-ag-b-refresh',
      expired, project_id: 'synthetic-ag-project-b',
    };
  }
  if (kind === 'xai-no-auth-kind') {
    return {
      type: 'xai', using_api: true, token_type: 'Bearer',
      access_token: 'synthetic-xai-bad-mode-access', refresh_token: 'synthetic-xai-bad-mode-refresh', expired,
    };
  }
  throw fail(`unknown staged auth kind ${kind}`);
}
function rememberStaged(document) {
  for (const value of Object.values(document)) {
    if (typeof value === 'string' && value.startsWith('synthetic-')) remember(value);
  }
}
function duplicateTopLevelKeys(text) {
  const keys = [];
  let depth = 0;
  for (let index = 0; index < text.length; index += 1) {
    const ch = text[index];
    if (ch === '"') {
      let cursor = index + 1;
      let value = '';
      while (cursor < text.length) {
        if (text[cursor] === '\\') { cursor += 2; continue; }
        if (text[cursor] === '"') break;
        value += text[cursor];
        cursor += 1;
      }
      if (depth === 1 && /^\s*:/.test(text.slice(cursor + 1))) keys.push(value);
      index = cursor;
      continue;
    }
    if (ch === '{' || ch === '[') depth += 1;
    else if (ch === '}' || ch === ']') depth -= 1;
  }
  const seen = new Set();
  const duplicates = [];
  for (const key of keys) {
    if (seen.has(key)) duplicates.push(key);
    seen.add(key);
  }
  return duplicates;
}
function nativeOriginAllowed(value) {
  if (typeof value !== 'string') return false;
  const matched = value.match(/^http:\/\/(?:127\.0\.0\.1|localhost|\[::1\]):(\d+)$/);
  if (!matched) return false;
  const port = Number(matched[1]);
  if (!Number.isInteger(port) || port <= 0 || port > 65535 || matched[1] !== String(port)) return false;
  try {
    const parsed = new URL(value);
    return parsed.protocol === 'http:' && !parsed.username && !parsed.password && !parsed.search && !parsed.hash && (parsed.pathname === '/' || parsed.pathname === '');
  } catch { return false; }
}
function classifyNativeMap(text) {
  if (typeof text !== 'string' || text.trim() === '') return { ok: false, reason: 'empty map text' };
  let body;
  try { body = JSON.parse(text); } catch { return { ok: false, reason: 'malformed JSON' }; }
  if (!body || typeof body !== 'object' || Array.isArray(body)) return { ok: false, reason: 'map is not an object' };
  const duplicates = duplicateTopLevelKeys(text);
  if (duplicates.length) return { ok: false, reason: `duplicate key ${duplicates[0]}` };
  if (Object.prototype.hasOwnProperty.call(body, 'cpa')) return { ok: false, reason: 'cpa is unknown and refuses the whole map' };
  const allowed = new Set([...NATIVE_HOOK.keys, ...NATIVE_HOOK.existingKeys]);
  for (const [key, value] of Object.entries(body)) {
    if (!allowed.has(key)) return { ok: false, reason: `unknown key ${key}` };
    if (typeof value !== 'string' || !nativeOriginAllowed(value)) return { ok: false, reason: `malformed origin for ${key}` };
  }
  return { ok: true, reason: '' };
}
function nativeOfficialOrigin(family) {
  const row = nativeOutcome(family, 'chat_completions', 'execute', nativeScenarioModel(family));
  return NATIVE_MATRIX[row.target]?.origin ?? '';
}
function nativeSeamBody(port) {
  if (!Number.isInteger(port) || port <= 0 || port > 65535) throw fail('native loopback port is not an explicit nonzero port');
  const origin = `http://127.0.0.1:${port}`;
  const parsed = new URL(origin);
  if (parsed.protocol !== 'http:' || parsed.hostname !== '127.0.0.1' || parsed.port !== String(port) || parsed.username || parsed.password || parsed.search || parsed.hash || (parsed.pathname !== '/' && parsed.pathname !== '')) {
    throw fail('native seam origin is not an HTTP loopback origin');
  }
  const body = {};
  for (const key of NATIVE_HOOK.keys) body[key] = origin;
  return body;
}
function nativeSeamJson(port) {
  return JSON.stringify(nativeSeamBody(port));
}
function nativeProviderRoots(root) {
  return {
    codex: join(root, 'codex'),
    claude: join(root, 'claude'),
    kimi: join(root, 'kimi'),
    grok: join(root, 'grok'),
  };
}
function nativeSourceLayout(root) {
  const roots = nativeProviderRoots(root);
  return {
    codex: join(roots.codex, 'auth.json'),
    claude: join(roots.claude, '.credentials.json'),
    kimi: join(roots.kimi, 'credentials', 'kimi-code.json'),
    xai: join(roots.grok, 'auth.json'),
  };
}
function syntheticJwt(claims) {
  const header = Buffer.from(JSON.stringify({ alg: 'none', typ: 'JWT' })).toString('base64url');
  const body = Buffer.from(JSON.stringify(claims)).toString('base64url');
  const signature = Buffer.from('synthetic-not-a-signature').toString('base64url');
  return `${header}.${body}.${signature}`;
}
function nativeSourceDocuments() {
  return {
    codex: {
      auth_mode: 'chatgpt',
      tokens: {
        id_token: syntheticJwt({
          synthetic: true,
          email: 'synthetic-codex@example.invalid',
          'https://api.openai.com/auth': { chatgpt_account_id: 'synthetic-codex-account' },
        }),
        access_token: syntheticJwt({ synthetic: true, exp: 1893456000 }),
        refresh_token: 'synthetic-refresh-codex',
        account_id: 'synthetic-codex-account',
      },
    },
    claude: {
      claudeAiOauth: {
        scopes: ['user:inference'],
        expiresAt: 1893456000000,
        accessToken: 'synthetic-claude-access',
        refreshToken: 'synthetic-claude-refresh',
      },
    },
    kimi: {
      expires_at: 1893456000,
      scope: 'synthetic',
      token_type: 'bearer',
      access_token: 'synthetic-kimi-access',
      refresh_token: 'synthetic-kimi-refresh',
    },
    xai: {
      'https://auth.x.ai::b1a00492-073a-47ea-816f-4c329264a828': {
        auth_mode: 'oidc',
        oidc_issuer: 'https://auth.x.ai',
        oidc_client_id: 'b1a00492-073a-47ea-816f-4c329264a828',
        key: 'synthetic-xai-key',
        refresh_token: 'synthetic-xai-refresh',
        expires_at: '2030-01-01T00:00:00Z',
        email: 'synthetic-xai@example.invalid',
        principal_id: 'synthetic-xai-principal',
      },
    },
  };
}
function nativeImportName(provider, identity) {
  const digest = createHash('sha256').update(`${provider}:${identity}`).digest('hex');
  return `ocg-cli-${provider}-${digest.slice(0, 24)}.json`;
}
function nativeGeminiPath(modelId, action) {
  if (!/^[A-Za-z0-9._:-]+$/.test(modelId)) throw fail('discovered native model id is not a single path segment');
  return `/v1beta/models/${modelId}:${action}`;
}
function officialRefusalsSince(mark) {
  return world.proxyRefusals.slice(mark).filter(host => NATIVE_OFFICIAL_HOSTS.includes(host));
}
function nativeHitsSince(mark) {
  return world.nativeHits.slice(mark);
}
function assertNoNativeDial(hitMark, refusalMark, label) {
  const hits = nativeHitsSince(hitMark);
  assert(hits.length === 0, `${label} sent ${hits.length} native requests`);
  const refused = officialRefusalsSince(refusalMark);
  assert(refused.length === 0, `${label} dialed ${refused.join(', ')}`);
}
function catalogModelsForScope(destination, scope) {
  const listed = (destination?.catalog ?? [])
    .filter(item => item && item.enabled !== false && typeof item.publicModel === 'string' && item.publicModel)
    .map(item => item.publicModel);
  if (!scope || scope.kind === 'all') return listed;
  if (scope.kind === 'only') {
    const allowed = new Set((scope.models ?? []).filter(model => typeof model === 'string'));
    return listed.filter(model => allowed.has(model));
  }
  return [];
}
function bindingsForOrigin(identities, origin) {
  const expected = new URL(origin).hostname;
  const found = [];
  for (const identity of identities ?? []) {
    for (const credential of identity.credentials ?? []) {
      for (const binding of credential.bindings ?? []) {
        const origins = binding.allowedOrigins ?? [];
        if (origins.some(item => {
          try { return new URL(item).hostname === expected; } catch { return false; }
        })) found.push(binding);
      }
    }
  }
  return found;
}
const NATIVE_WIRE = {
  chatJson: '{"id":"ok","choices":[{"message":{"role":"assistant","content":"yes"}}]}',
  chatSse: 'data: {"id":"chatcmpl-1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"ok"}}]}\n\ndata: [DONE]\n\n',
  codexSse: 'data: {"type":"response.output_item.done","output_index":0,"item":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"yes"}]}}\n\ndata: {"type":"response.completed","response":{"id":"resp_1","object":"response","status":"completed","model":"gpt-5.5","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"yes"}]}],"usage":{"input_tokens":2,"output_tokens":1}}}\n\n',
  messagesJson: '{"id":"msg","type":"message","role":"assistant","model":"claude-sonnet","stop_reason":"end_turn","content":[{"type":"text","text":"yes"}],"usage":{"input_tokens":1,"output_tokens":1}}',
  messagesSse: 'event: message_start\ndata: {"type":"message_start","message":{"id":"msg_123","type":"message","role":"assistant","content":[],"model":"claude-opus-5","stop_reason":null,"usage":{"input_tokens":100,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":1}}}\n\nevent: content_block_start\ndata: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}\n\nevent: content_block_delta\ndata: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hello"}}\n\nevent: content_block_stop\ndata: {"type":"content_block_stop","index":0}\n\nevent: message_delta\ndata: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":15}}\n\nevent: message_stop\ndata: {"type":"message_stop"}\n\n',
  antigravityJson: '{"response":{"candidates":[{"finishReason":"STOP","content":{"role":"model","parts":[{"text":"yes"}]}}],"usageMetadata":{"promptTokenCount":3,"candidatesTokenCount":4}}}',
  antigravitySse: 'data: {"response":{"candidates":[{"content":{"role":"model","parts":[{"text":"first"}]},"finishReason":"STOP"}],"modelVersion":"gemini-3.7-flash","responseId":"resp-split"},"traceId":"trace-split"}\n\ndata: {"response":{"candidates":[{"content":{"role":"model","parts":[{"text":""}]}}],"usageMetadata":{"promptTokenCount":11,"candidatesTokenCount":22,"totalTokenCount":33},"modelVersion":"gemini-3.7-flash","responseId":"resp-split"},"traceId":"trace-split"}\n\n',
  messagesCount: '{"input_tokens":11}',
  antigravityCount: '{"totalTokens":9}',
  kimiResponsesJson: '{"id":"resp_123","object":"response","status":"completed","model":"k3","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"hello world"}]}],"usage":{"total_tokens":10,"input_tokens":6,"output_tokens":4}}',
  kimiResponsesSse: 'event: response.created\ndata: {"type":"response.created","response":{"id":"resp_stream","status":"in_progress","service_tier":"default","model":"k3"}}\n\nevent: response.output_item.added\ndata: {"type":"response.output_item.added","output_index":0,"item":{"type":"message","role":"assistant","content":[]}}\n\nevent: response.output_text.delta\ndata: {"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":"hello stream"}\n\nevent: response.completed\ndata: {"type":"response.completed","response":{"id":"resp_stream","status":"completed","usage":{"total_tokens":12,"input_tokens":5,"output_tokens":7}}}\n\n',
};
function nativeUpstreamWire(cell, streamFlag) {
  const source = typeof cell === 'string' ? { path: cell, stream: streamFlag === true } : (cell ?? {});
  const path = String(source.path ?? '');
  const family = String(source.family ?? '');
  const operation = String(source.operation ?? '');
  const stream = source.stream === true || operation === 'stream' || streamFlag === true;
  if (path.includes('count_tokens')) return { type: 'application/json', body: NATIVE_WIRE.messagesCount, marker: '11' };
  if (path.includes('countTokens')) return { type: 'application/json', body: NATIVE_WIRE.antigravityCount, marker: '9' };
  if (path.includes('streamGenerateContent') || (stream && path.includes('generateContent'))) {
    return { type: 'text/event-stream', body: NATIVE_WIRE.antigravitySse, marker: 'first' };
  }
  if (path.includes('generateContent')) return { type: 'application/json', body: NATIVE_WIRE.antigravityJson, marker: 'yes' };
  if (path.includes('/responses')) {
    if (family === 'kimi.com' || family === 'kimi.ai') {
      return stream
        ? { type: 'text/event-stream', body: NATIVE_WIRE.kimiResponsesSse, marker: 'hello stream' }
        : { type: 'application/json', body: NATIVE_WIRE.kimiResponsesJson, marker: 'hello world' };
    }
    if (family === 'codex' || family === 'xai.cli' || family === 'xai.api') {
      return { type: 'text/event-stream', body: NATIVE_WIRE.codexSse, marker: 'yes' };
    }
    return null;
  }
  if (path.includes('/messages')) {
    return stream
      ? { type: 'text/event-stream', body: NATIVE_WIRE.messagesSse, marker: 'hello' }
      : { type: 'application/json', body: NATIVE_WIRE.messagesJson, marker: 'yes' };
  }
  return stream
    ? { type: 'text/event-stream', body: NATIVE_WIRE.chatSse, marker: 'ok' }
    : { type: 'application/json', body: NATIVE_WIRE.chatJson, marker: 'yes' };
}
async function startNativeLoopback() {
  world.nativeHits = [];
  world.nativeBehavior = 'fail';
  world.nativeUpstreamCell = undefined;
  world.native429Bearers = new Set();
  const server = await openNativeSink(world.nativeHits);
  world.nativeLoopback = server;
  return server;
}
function respondNative(req, res, hits) {
  req.resume();
  const raw = String(req.url ?? '');
  const splitAt = raw.indexOf('?');
  const authorization = String(req.headers.authorization ?? '');
  const bearer = authorization.toLowerCase().startsWith('bearer ') ? authorization.slice(7).trim() : '';
  const behavior = world.nativeBehavior || 'fail';
  const hit = {
    method: req.method,
    path: splitAt === -1 ? raw : raw.slice(0, splitAt),
    query: splitAt === -1 ? '' : raw.slice(splitAt + 1),
    host: String(req.headers.host ?? ''),
    bearer,
    stream: behavior === 'sse' || behavior === 'truncate' || behavior === 'empty' || behavior === 'hang',
  };
  hits.push(hit);
  if (behavior === 'json' || behavior === 'sse' || behavior === 'count') {
    const wire = nativeUpstreamWire({ ...(world.nativeUpstreamCell ?? {}), path: hit.path, stream: behavior === 'sse' });
    if (!wire) {
      res.writeHead(502, { 'content-type': 'text/plain; charset=utf-8', connection: 'close' });
      res.end('native-loopback-recorded');
      return;
    }
    hit.stream = wire.type === 'text/event-stream';
    res.writeHead(200, { 'content-type': wire.type, connection: 'close' });
    res.end(wire.body);
    return;
  }
  if (behavior === 'loss') {
    res.writeHead(200, { 'content-type': 'application/json' });
    res.destroy();
    return;
  }
  if (behavior === 'truncate') {
    res.writeHead(200, { 'content-type': 'text/event-stream' });
    res.write('data: {"choices":[{"delta":{"content":"loop');
    res.end();
    return;
  }
  if (behavior === 'empty') {
    res.writeHead(200, { 'content-type': 'text/event-stream' });
    res.end();
    return;
  }
  if (behavior === 'hang') {
    res.writeHead(200, { 'content-type': 'text/event-stream' });
    world.openResponses.add(res);
    res.on('close', () => world.openResponses.delete(res));
    return;
  }
  if (behavior === '429') {
    res.writeHead(429, { 'content-type': 'application/json', connection: 'close' });
    res.end(JSON.stringify({ error: { message: 'synthetic explicit rejection', type: 'insufficient_quota' } }));
    return;
  }
  if (behavior === '429-pair') {
    const seen = world.native429Bearers ?? (world.native429Bearers = new Set());
    const first = seen.size === 0;
    seen.add(bearer || `empty-${seen.size}`);
    if (first) {
      res.writeHead(429, { 'content-type': 'application/json', connection: 'close' });
      res.end(JSON.stringify({ error: { message: 'synthetic explicit rejection', type: 'insufficient_quota' } }));
      return;
    }
    const wire = nativeUpstreamWire({ ...(world.nativeUpstreamCell ?? {}), path: hit.path, stream: false });
    if (!wire) {
      res.writeHead(502, { 'content-type': 'text/plain; charset=utf-8', connection: 'close' });
      res.end('native-loopback-recorded');
      return;
    }
    hit.stream = wire.type === 'text/event-stream';
    res.writeHead(200, { 'content-type': wire.type, connection: 'close' });
    res.end(wire.body);
    return;
  }
  res.writeHead(502, { 'content-type': 'text/plain; charset=utf-8', connection: 'close' });
  res.end('native-loopback-recorded');
}
function openNativeSink(hits) {
  const server = createNetServer((req, res) => {
    if (!loopback(req.socket.remoteAddress)) { res.writeHead(403); res.end(); return; }
    respondNative(req, res, hits);
  });
  return new Promise((resolveListen, rejectListen) => {
    server.listen(0, '127.0.0.1', error => error ? rejectListen(error) : resolveListen(server));
  });
}
async function closeNativeLoopback() {
  for (const response of [...(world.openResponses ?? [])]) {
    try { response.destroy(); } catch {}
  }
  world.openResponses = new Set();
  const servers = [world.nativeLoopback, world.nativeShadow].filter(Boolean);
  world.nativeLoopback = undefined;
  world.nativeShadow = undefined;
  for (const server of servers) await new Promise(done => server.close(done));
}
async function rememberNativeDocuments(documents) {
  const visit = value => {
    if (typeof value === 'string') {
      if (value.startsWith('synthetic-')) remember(value);
      const parts = value.split('.');
      if (parts.length === 3) {
        try {
          const claims = JSON.parse(Buffer.from(parts[1], 'base64url').toString('utf8'));
          if (claims?.synthetic === true) remember(value);
        } catch {}
      }
      return;
    }
    if (Array.isArray(value)) { value.forEach(visit); return; }
    if (value && typeof value === 'object') Object.values(value).forEach(visit);
  };
  visit(documents);
}
async function writeNativeSources(root) {
  const documents = nativeSourceDocuments();
  await rememberNativeDocuments(documents);
  const layout = nativeSourceLayout(root);
  for (const [key, file] of Object.entries(layout)) {
    await mkdir(dirname(file), { recursive: true });
    await writeFile(file, `${JSON.stringify(documents[key])}\n`);
  }
}
function nativeAdmission() {
  if (!trustedSha) return NATIVE_PENDING;
  if (!world.proxy) return 'pending: the deny-external proxy is absent, so native workflows will not dial official hosts';
  if (!world.root || !world.dataDir) return 'pending: the isolated acceptance profile is absent';
  if (!options.testFeatureCli && !options.productionFeatureOff) return NATIVE_PENDING;
  if (options.testFeatureCli && options.productionFeatureOff) return 'pending: compile mode flags are mutually exclusive';
  return '';
}
function assertNativeGuard() {
  const admission = nativeAdmission();
  if (admission) throw blocked(admission);
}
function fixtureLane() {
  return options.testFeatureCli === true;
}
function seamOriginOf(port) {
  return `http://127.0.0.1:${port}`;
}
function originHostOf(origin) {
  try { return new URL(origin).hostname; } catch { return ''; }
}
function sameSeamOrigin(origin, port) {
  try {
    const parsed = new URL(origin);
    return parsed.protocol === 'http:' && parsed.hostname === '127.0.0.1' && parsed.port === String(port) && (parsed.pathname === '/' || parsed.pathname === '') && !parsed.search && !parsed.hash && !parsed.username;
  } catch { return false; }
}
function assertGrantShape(binding, family, port) {
  const origins = binding.allowedOrigins ?? [];
  const ids = binding.allowedEndpointIds ?? [];
  assert(ids.length > 0, `${family} discovery wrote no endpoint grant`);
  assert(origins.length > 0, `${family} discovery wrote no origin grant`);
  if (fixtureLane()) {
    assert(origins.every(origin => sameSeamOrigin(origin, port)), `${family} grant origins ${origins.join(',')} are not the applied seam`);
    return;
  }
  assert(origins.every(origin => {
    const host = originHostOf(origin);
    return host && !['127.0.0.1', 'localhost', '::1'].includes(host);
  }), `${family} stored a loopback origin as a grant`);
  const expected = nativeOfficialOrigin(family);
  assert(origins.some(origin => originHostOf(origin) === originHostOf(expected)), `${family} grant missed the official origin host`);
}
function identityForAccount(identities, account) {
  const name = account?.name ?? '';
  const stem = name.replace(/\.json$/i, '');
  return (identities ?? []).find(identity => {
    const label = identity?.identity?.label ?? '';
    const notes = String(identity?.identity?.notes ?? '');
    if (label === name || label === stem || notes.includes(name)) return true;
    return (identity.credentials ?? []).some(item => String(item?.credential?.secretRef ?? '').includes(name) || item?.legacy?.id === name);
  }) ?? null;
}
function nativeBearerClass(bearer) {
  const classified = classifyBearer(bearer);
  if (classified === 'client' || classified === 'none') return classified;
  for (const secret of secrets) if (secret === bearer) return 'staged';
  return classified === 'other' ? 'other' : classified;
}
function ownedAuthDir() {
  return join(world.dataDir, 'cpa', 'auth');
}
async function stageOwnedAuth(name, document) {
  if (name !== basename(name) || name.includes('..')) throw fail('staged auth name is not a direct basename');
  const dir = ownedAuthDir();
  let info;
  try { info = await stat(dir); }
  catch { throw blocked('pending: data/cpa/auth is absent; the harness does not create a parallel auth directory'); }
  if (!info.isDirectory()) throw blocked('pending: data/cpa/auth is not a directory');
  const file = join(dir, name);
  if (!within(dir, file)) throw fail('staged auth path left data/cpa/auth');
  rememberStaged(document);
  await writeFile(file, `${JSON.stringify(document)}\n`);
  return file;
}
async function removeOwnedAuth(name) {
  if (name !== basename(name) || name.includes('..')) throw fail('staged auth removal is not a direct basename');
  const file = join(ownedAuthDir(), name);
  if (!within(ownedAuthDir(), file)) throw fail('staged auth removal left data/cpa/auth');
  try { await unlink(file); }
  catch (error) { if (error?.code !== 'ENOENT') throw error; }
}
async function reconcileStaged(label) {
  const previous = revisionOf((await readRuntime()).facts, 'appliedRevision');
  const refreshed = await api('POST', `${prefix}/external-integrations/cpa/models/refresh`, {}, { cas: true, success: false });
  if (refreshed.code !== 0) throw blocked(`pending: models/refresh did not reconcile ${label}: ${String(refreshed.stderr).slice(0, 300)}`);
  const applied = await waitAutomaticApply(previous);
  if (applied.phase === 'failed') throw fail(`${label} reconcile apply failed`);
  if (applied.phase === 'pending') throw blocked(`pending: ${label} reconcile apply stayed pending`);
  if (applied.phase === 'unavailable') throw fail(`${label} reconcile dropped readiness`);
  return applied;
}
async function listCpaAccounts() {
  const value = (await api('GET', `${prefix}/external-integrations/cpa/accounts`)).value;
  if (!Array.isArray(value?.accounts)) throw blocked(`pending: cpa accounts have no accounts array; keys ${Object.keys(value ?? {}).join(',')}`);
  return value.accounts;
}
async function listIdentities() {
  const value = (await api('GET', `${prefix}/accounts`)).value;
  if (!Array.isArray(value?.identities)) throw blocked(`pending: accounts payload has no identities array; keys ${Object.keys(value ?? {}).join(',')}`);
  return value.identities;
}
async function readForwardItems() {
  const logs = (await api('GET', `${prefix}/logs/forward?limit=80`)).value;
  if (!logs || typeof logs !== 'object' || !Array.isArray(logs.items)) {
    throw blocked(`pending: forward logs have no items array; keys ${Object.keys(logs ?? {}).join(',')}`);
  }
  return logs.items;
}
function forwardId(item) {
  return item?.request_id ?? item?.id ?? '';
}
async function snapshotForwardIds() {
  const items = await readForwardItems();
  return new Set(items.map(forwardId).filter(Boolean));
}
async function captureMarks() {
  return {
    hits: world.nativeHits.length,
    refusals: world.proxyRefusals.length,
    proxyHits: world.proxyHits.length,
    logs: await snapshotForwardIds(),
    capturedLogs: true,
  };
}
function syncMarks() {
  return { hits: world.nativeHits.length, refusals: world.proxyRefusals.length, proxyHits: world.proxyHits.length, logs: new Set(), capturedLogs: false };
}
async function awaitForwardRows(before, model, { expectRow }) {
  const tries = expectRow ? 12 : 4;
  let fresh = [];
  for (let attempt = 0; attempt < tries; attempt += 1) {
    const items = await readForwardItems();
    const matching = items.filter(item => item.requestedModel === model || item.model === model || item.upstreamModel === model);
    const unlabeled = matching.find(item => !forwardId(item));
    if (unlabeled) throw blocked(`pending: forward log row for ${model} has no request_id; keys ${Object.keys(unlabeled).join(',')}`);
    fresh = matching.filter(item => !before.has(forwardId(item)));
    if (expectRow && fresh.length) return fresh;
    if (!expectRow && fresh.length) return fresh;
    await delay(250);
  }
  return fresh;
}
function assertRowAttribution(row, model, label, attempts) {
  const attributed = row.requestedModel === model || row.model === model || row.upstreamModel === model;
  assert(attributed, `${label} log is not attributed to the sent model`);
  if (row.attempt !== undefined && row.attempt !== null) assert(Number(row.attempt) === attempts, `${label} attempt was ${row.attempt}`);
  if (row.error_source !== undefined && row.error_source !== null) assert(typeof row.error_source === 'string', `${label} error_source is not a string`);
}
async function assertZeroPhysical(marks, label, model) {
  assert(nativeHitsSince(marks.hits).length === 0, `${label} sent ${nativeHitsSince(marks.hits).length} native requests`);
  assert(officialRefusalsSince(marks.refusals).length === 0, `${label} dialed an official host`);
  assert(world.proxyHits.length === marks.proxyHits, `${label} forwarded through the deny proxy`);
  if (model && marks.capturedLogs) {
    const rows = await awaitForwardRows(marks.logs, model, { expectRow: false });
    for (const row of rows) {
      assertRowAttribution(row, model, label, 1);
      assert(!(row.promptTokens > 0 || row.completionTokens > 0), `${label} logged upstream tokens`);
    }
  }
}
function assertPhysicalHit(hit, cell, port) {
  const matrix = NATIVE_MATRIX[cell.target];
  assert(matrix, `${cell.family} ${cell.operation} has no matrix target`);
  assert(hit.method === 'POST', `${cell.family} ${cell.operation} method was ${hit.method}`);
  assert(hit.path === matrix.path, `${cell.family} ${cell.operation} path ${hit.path} is not the matrix path`);
  assert(hit.query === matrix.query, `${cell.family} ${cell.operation} query ${hit.query} is not the matrix query`);
  assert(String(hit.host).toLowerCase() === `127.0.0.1:${port}`, `${cell.family} ${cell.operation} host ${hit.host} is not the seam`);
  const bearerClass = nativeBearerClass(hit.bearer);
  assert(bearerClass !== 'client', `${cell.family} forwarded the client key`);
  if (cell.outcome === 'network') assert(bearerClass === 'staged' || bearerClass === 'other', `${cell.family} bearer class was ${bearerClass}`);
}
function assertFeatureOffDenial(run, cell, marks) {
  assert(run.timedOut !== true, `${cell.family} ${cell.operation} feature-off was killed by the harness`);
  assert(run.code !== 0, `${cell.family} ${cell.operation} feature-off send succeeded`);
  assert(nativeHitsSince(marks.hits).length === 0, `${cell.family} feature-off rewrote onto the loopback`);
  assert(world.proxyHits.length === marks.proxyHits, `${cell.family} feature-off forwarded an external host`);
  const host = NATIVE_MATRIX[cell.target]?.host;
  assert(world.proxyRefusals.slice(marks.refusals).includes(host), `${cell.family} feature-off reached neither the loopback nor the deny proxy`);
  assert(!renderedRun(run).includes('"policyReady":true'), `${cell.family} feature-off fabricated readiness`);
}
function sseDataObjects(text) {
  const objects = [];
  for (const line of String(text ?? '').split(/\r?\n/)) {
    if (!line.startsWith('data:')) continue;
    const payload = line.slice(5).trim();
    if (!payload || payload === '[DONE]') continue;
    try { objects.push(JSON.parse(payload)); } catch {}
  }
  return objects;
}
function clientShape(cell) {
  if (publicAntigravityCompaction(cell)) return streamedNonstream(cell.model) ? 'compaction-sse' : 'compaction-json';
  if (cell?.ingress === 'gemini-streamGenerateContent') return 'gemini-sse';
  if (cell?.ingress === 'gemini-generateContent') return 'gemini-json';
  const stream = cell?.operation === 'stream';
  if (cell?.protocol === 'responses') return stream ? 'responses-sse' : 'responses-json';
  if (cell?.protocol === 'messages') return stream ? 'messages-sse' : 'messages-json';
  return stream ? 'chat-sse' : 'chat-json';
}
function responseOutputText(value) {
  const output = Array.isArray(value?.output) ? value.output : [];
  for (const item of output) {
    const content = Array.isArray(item?.content) ? item.content : [];
    const text = content.find(part => typeof part?.text === 'string' && part.text)?.text;
    if (text) return text;
  }
  return '';
}
function geminiPartText(value) {
  const candidates = value?.candidates ?? value?.response?.candidates;
  const parts = candidates?.[0]?.content?.parts;
  if (!Array.isArray(parts)) return '';
  return parts.find(part => typeof part?.text === 'string' && part.text)?.text ?? '';
}
function assertClientPayload(run, cell, label) {
  const name = label || `${cell?.family ?? 'client'} ${cell?.operation ?? 'send'}`;
  assert(run.code === 0, `${name} CLI exit was ${run.code}: ${String(run.stderr).slice(0, 300)}`);
  const shape = clientShape(cell);
  const text = typeof run.value === 'string' ? run.value : renderedRun(run);
  if (shape === 'compaction-json') {
    assert(run.value?.status === 'completed', `${name} compaction JSON is not completed`);
    assert(run.value?.object === 'response.compaction' || run.value?.object === 'response', `${name} compaction JSON object was ${run.value?.object}`);
    const output = Array.isArray(run.value?.output) ? run.value.output : [];
    const capsule = output.find(item => item?.type === 'compaction' && typeof item.encrypted_content === 'string' && item.encrypted_content);
    assert(capsule, `${name} compaction JSON has no capsule output`);
    return;
  }
  if (shape === 'compaction-sse') {
    const frames = sseDataObjects(text);
    const completed = frames.some(item => item?.type === 'response.completed' || item?.response?.status === 'completed');
    const capsule = frames.some(item => item?.item?.type === 'compaction' || (Array.isArray(item?.response?.output) && item.response.output.some(part => part?.type === 'compaction')));
    assert(capsule, `${name} compaction SSE has no capsule item`);
    assert(completed, `${name} compaction SSE has no completed frame`);
    return;
  }
  if (shape === 'chat-json') {
    const content = run.value?.choices?.[0]?.message?.content;
    assert(typeof content === 'string' && content.length > 0, `${name} chat JSON has no message content`);
    return;
  }
  if (shape === 'chat-sse') {
    const deltas = sseDataObjects(text).map(item => item?.choices?.[0]?.delta?.content).filter(item => typeof item === 'string' && item);
    assert(deltas.length > 0 && text.includes('[DONE]'), `${name} chat SSE has no delta content`);
    return;
  }
  if (shape === 'responses-json') {
    assert(run.value?.object === 'response' && run.value?.status === 'completed', `${name} responses JSON is not a completed response`);
    assert(responseOutputText(run.value).length > 0, `${name} responses JSON has no output text`);
    return;
  }
  if (shape === 'responses-sse') {
    const frames = sseDataObjects(text);
    const completed = frames.some(item => item?.type === 'response.completed' || item?.response?.status === 'completed');
    const textFrame = frames.some(item => {
      if (typeof item?.delta === 'string' && item.delta) return true;
      if (responseOutputText(item?.response).length > 0) return true;
      const parts = item?.item?.content;
      return Array.isArray(parts) && parts.some(part => typeof part?.text === 'string' && part.text);
    });
    assert(textFrame, `${name} responses SSE has no output text`);
    assert(completed, `${name} responses SSE has no completed frame`);
    return;
  }
  if (shape === 'messages-json') {
    const blocks = Array.isArray(run.value?.content) ? run.value.content : [];
    const block = blocks.find(item => item?.type === 'text' && typeof item.text === 'string' && item.text);
    assert(block, `${name} messages JSON has no text block`);
    return;
  }
  if (shape === 'messages-sse') {
    const frames = sseDataObjects(text);
    const delta = frames.some(item => item?.delta?.type === 'text_delta' && typeof item.delta.text === 'string' && item.delta.text);
    assert(delta && text.includes('message_stop'), `${name} messages SSE has no text delta`);
    return;
  }
  if (shape === 'gemini-json') {
    assert(geminiPartText(run.value).length > 0, `${name} Gemini JSON has no candidate text`);
    return;
  }
  const frames = sseDataObjects(text);
  assert(frames.some(item => geminiPartText(item).length > 0), `${name} Gemini SSE has no candidate text`);
}
function assertLocalCount(run, label) {
  const rendered = renderedRun(run);
  assert(run.code === 0, `${label} CLI exit was ${run.code}: ${String(run.stderr).slice(0, 300)}`);
  assert(!rendered.includes('Gemini countTokens is not available'), `${label} used the Gemini estimation 501`);
  if (!numericCount(run.value)) throw blocked(`${label} returned no numeric local total and no upstream send`);
}
function chatExecuteCell(family) {
  const model = nativeScenarioModel(family);
  return nativeCatalog().find(cell => cell.family === family && cell.ingress === 'chat_completions' && cell.operation === 'execute' && cell.model === model);
}
function eligibleIsTarget(candidate, state) {
  const authority = candidate?.authority;
  if (!authority || typeof authority !== 'object') return false;
  if (authority.bindingId !== state.binding?.id) return false;
  if (!state.credentialId || authority.credentialId !== state.credentialId) return false;
  if (typeof authority.nativeProvider !== 'string' || authority.nativeProvider !== state.nativeProvider) return false;
  return true;
}
async function readTargetExplain(cell, state, model) {
  const protocol = cell.ingress.startsWith('gemini-') ? 'chat_completions' : cell.protocol;
  const explained = (await api('GET', `${prefix}/routing/explain?model=${encodeURIComponent(model)}&clientProtocol=${protocol}`)).value;
  if (!Array.isArray(explained?.eligible)) {
    throw blocked(`pending: routing explain for ${cell.family} ${model} has no eligible array; keys ${Object.keys(explained ?? {}).join(',')}`);
  }
  const matched = explained.eligible.filter(candidate => eligibleIsTarget(candidate, state));
  const foreign = explained.eligible.filter(candidate => !eligibleIsTarget(candidate, state));
  return { explained, matched, foreign };
}
async function assertTargetEligible(cell, state, model) {
  const { matched, foreign } = await readTargetExplain(cell, state, model);
  if (!matched.length) {
    const ids = foreign.map(candidate => candidate?.authority?.bindingId ?? candidate?.accountId ?? 'absent').join(',');
    throw blocked(`pending: ${cell.family} ${model} explain has no eligible candidate for binding ${state.binding.id} credential ${state.credentialId} provider ${state.nativeProvider}; eligible bindings ${ids || 'none'}`);
  }
  if (foreign.length) {
    const ids = foreign.map(candidate => candidate?.authority?.bindingId ?? 'absent').join(',');
    throw fail(`${cell.family} ${model} explain still includes another binding after isolation: ${ids}`);
  }
}
async function settleBindingApply(previousApplied, label) {
  const outcome = await waitAutomaticApply(previousApplied);
  if (outcome.phase === 'unchanged') {
    throw blocked(`product dependency: ${label} did not schedule an owned apply. desired ${outcome.desired} applied ${outcome.applied} status ${outcome.status}. The binding PATCH stored the edit and bumped the settings revision. The existing wait ended unchanged. The harness does not start the runtime and does not wait again.`);
  }
  return assertAutomaticApply(outcome, label);
}
async function isolateForeignBindings(prepared, family) {
  const changed = [];
  const restore = async () => {
    for (const id of changed) {
      const before = revisionOf((await readRuntime()).facts, 'appliedRevision');
      const patched = await api('PATCH', `${prefix}/bindings/${id}`, { enabled: true }, { cas: true, success: false });
      if (patched.code !== 0) throw fail(`binding ${id} restore failed: ${String(patched.stderr).slice(0, 300)}`);
      await settleBindingApply(before, `binding ${id} restore`);
    }
  };
  try {
    for (const [name, item] of Object.entries(prepared.families)) {
      if (name === family || !item?.binding?.id) continue;
      const before = revisionOf((await readRuntime()).facts, 'appliedRevision');
      await api('PATCH', `${prefix}/bindings/${item.binding.id}`, { enabled: false }, { cas: true });
      changed.push(item.binding.id);
      await settleBindingApply(before, `binding ${item.binding.id} isolation`);
    }
  } catch (error) {
    try { await restore(); } catch { /* the isolation error names the missed apply */ }
    throw error;
  }
  return restore;
}
async function prepareNativeOperator() {
  assertNativeGuard();
  const root = join(world.root, 'home', 'native');
  const roots = nativeProviderRoots(root);
  const realHome = process.env.USERPROFILE || process.env.HOME || '';
  if (realHome && (resolve(root) === resolve(realHome) || within(realHome, root))) throw fail('native roots overlap the installed user profile');
  await writeNativeSources(root);
  const server = await startNativeLoopback();
  const seamPort = server.address().port;
  const seam = nativeSeamJson(seamPort);
  if (!seam || seam.includes('opencode') || seam.includes('command-code') || classifyNativeMap(seam).ok !== true) {
    throw blocked('native seam JSON is not the seven-key loopback map');
  }
  world.nativeRoots = roots;
  world.trustedSeamJson = seam;
  await restartAcceptanceServe();
  const quietHits = world.nativeHits.length;
  const quietRefusals = world.proxyRefusals.length;
  const discovered = (await api('GET', `${prefix}/external-integrations/cpa/cli-imports`)).value;
  const sources = discovered.sources ?? [];
  assert(sources.length === NATIVE_DISCOVERY.length, 'CLI discovery did not return the five native sources');
  for (const expected of NATIVE_DISCOVERY) {
    const source = sources.find(item => item.provider === expected.provider);
    assert(source, `${expected.provider} was absent from CLI discovery`);
    assert(source.source === expected.source, `${expected.provider} source was ${source.source}`);
    assert(source.supported === expected.supported, `${expected.provider} supported was ${source.supported}`);
    if (!expected.supported) {
      assert(source.available === false, 'Antigravity local storage was reported available');
      assert(source.reason === expected.reason, 'Antigravity discovery reason did not match the source');
    } else assert(source.available === true, `${expected.provider} synthetic source was not visible`);
  }
  const importedNames = {};
  let snapshot = await readRuntime();
  for (const provider of NATIVE_IMPORT_PROVIDERS) {
    const previous = revisionOf(snapshot.facts, 'appliedRevision');
    const imported = (await api('POST', `${prefix}/external-integrations/cpa/cli-imports`, { provider }, { cas: true })).value;
    assert(imported.provider === provider, `${provider} import returned ${imported.provider}`);
    assert(imported.outcome === 'imported' || imported.outcome === 'alreadyImported', `${provider} import outcome was ${imported.outcome}`);
    assert(imported.name === nativeCliAccountName(provider), `${provider} import name was ${imported.name}`);
    importedNames[provider] = imported.name;
    const applied = await waitAutomaticApply(previous);
    if (applied.phase === 'unchanged' && imported.outcome === 'alreadyImported') requirePolicy(applied.value);
    else assertAutomaticApply(applied, `${provider} import`);
    snapshot = await readRuntime();
  }
  assertNoNativeDial(quietHits, quietRefusals, 'native import');
  const beforeAg = await listCpaAccounts();
  assert(!beforeAg.some(account => account.provider === 'antigravity'), 'Antigravity was imported without a supported local file');
  const agFile = join(ownedAuthDir(), NATIVE_STAGED_FILES.antigravity);
  let agExists = true;
  try { await stat(agFile); } catch { agExists = false; }
  assert(!agExists, 'Antigravity staged file existed before the unsupported import');
  const agImport = await api('POST', `${prefix}/external-integrations/cpa/cli-imports`, { provider: 'antigravity' }, { cas: true, success: false });
  assert(agImport.code !== 0, 'unsupported Antigravity import succeeded');
  const agText = `${typeof agImport.value === 'string' ? agImport.value : stringifyJson(agImport.value ?? '')}\n${agImport.stderr}`;
  assert(agText.includes('not compatible with this import'), `Antigravity import reason was ${agText.slice(0, 400)}`);
  agExists = true;
  try { await stat(agFile); } catch { agExists = false; }
  assert(!agExists, 'unsupported Antigravity import created a staged auth file');
  let authInfo;
  try { authInfo = await stat(ownedAuthDir()); }
  catch { throw blocked('pending: data/cpa/auth is absent; the harness does not create a parallel auth directory'); }
  if (!authInfo.isDirectory()) throw blocked('pending: data/cpa/auth is not a directory');
  for (const kind of ['kimi.ai', 'xai.api', 'antigravity']) await stageOwnedAuth(NATIVE_STAGED_FILES[kind], stagedAuthDocument(kind));
  await restartAcceptanceServe();
  await reconcileStaged('staged generation');
  const accounts = await listCpaAccounts();
  const xaiText = JSON.stringify(accounts.find(account => account.name === importedNames.xai) ?? {});
  assert(!/"using_api"\s*:\s*true/.test(xaiText), 'XAI using_api was synthesized from the CLI import');
  const retired = (await api('GET', `${prefix}/cpa/models`)).value;
  assert(!(retired.models ?? []).some(model => model.enabled === true), 'retired CPA catalog enabled a model as a route grant');
  const destinationPayload = (await api('GET', `${prefix}/destinations`)).value;
  if (!Array.isArray(destinationPayload?.destinations)) {
    throw blocked(`pending: destinations payload has no destinations array; keys ${Object.keys(destinationPayload ?? {}).join(',')}`);
  }
  const credentialPayload = (await api('GET', `${prefix}/credentials`)).value;
  if (!Array.isArray(credentialPayload?.credentials)) {
    throw blocked(`pending: credentials payload has no credentials array; keys ${Object.keys(credentialPayload ?? {}).join(',')}`);
  }
  const identities = await listIdentities();
  const nameFor = {
    codex: importedNames.codex,
    anthropic: importedNames.anthropic,
    'kimi.com': importedNames.kimi,
    'xai.cli': importedNames.xai,
    'kimi.ai': NATIVE_STAGED_FILES['kimi.ai'],
    'xai.api': NATIVE_STAGED_FILES['xai.api'],
    antigravity: NATIVE_STAGED_FILES.antigravity,
  };
  const families = {};
  for (const [family, name] of Object.entries(nameFor)) {
    const account = accounts.find(item => item.name === name);
    if (!account) {
      families[family] = { pending: `pending: ${name} was not listed by cpa accounts after reconcile` };
      continue;
    }
    const identity = identityForAccount(identities, account);
    if (!identity) {
      const labels = identities.map(item => item?.identity?.label ?? '').filter(Boolean);
      families[family] = { pending: `pending: ${name} has no identity label; labels ${labels.join(',')}`, account };
      continue;
    }
    const bindings = (identity.credentials ?? []).flatMap(item => item.bindings ?? []);
    if (bindings.length !== 1) {
      families[family] = { pending: `pending: ${name} has ${bindings.length} bindings`, account };
      continue;
    }
    const binding = bindings[0];
    assertGrantShape(binding, family, seamPort);
    const summary = (identity.credentials ?? []).find(item => (item.bindings ?? []).some(candidate => candidate.id === binding.id));
    const credentialId = summary?.credential?.id ?? '';
    const credentialRow = credentialPayload.credentials.find(row => row.id === credentialId || (summary?.legacy?.id && row.legacyAccountId === summary.legacy.id));
    if (!credentialRow) {
      families[family] = { pending: `pending: ${name} credential ${credentialId || 'absent'} is not in credentials[].id or legacyAccountId`, account, binding };
      continue;
    }
    if (!credentialRow.destinationId) {
      families[family] = { pending: `pending: ${name} credential has no destinationId`, account, binding };
      continue;
    }
    const destination = destinationPayload.destinations.find(item => item.id === credentialRow.destinationId);
    if (!destination) {
      families[family] = { pending: `pending: destination ${credentialRow.destinationId} is absent from the destination list`, account, binding };
      continue;
    }
    const models = catalogModelsForScope(destination, binding.modelScope);
    const supported = nativeRegistryModels(family).filter(model => models.includes(model));
    const branches = family === 'antigravity'
      ? {
        ordinary: supported.find(model => !streamedNonstream(model)) ?? '',
        streamed: supported.find(model => streamedNonstream(model)) ?? '',
      }
      : { ordinary: supported[0] ?? '', streamed: '' };
    if (!branches.ordinary && !branches.streamed) {
      families[family] = { pending: `pending: ${family} catalog publicModels ${models.join(',') || 'none'} include no ${NATIVE_PROVIDER[family]} registry model`, account, binding, models };
      continue;
    }
    families[family] = {
      modelId: branches.ordinary || branches.streamed,
      models,
      branches,
      binding,
      credentialId: credentialRow.id,
      nativeProvider: NATIVE_PROVIDER[family],
      account,
      savedIds: [...(binding.allowedEndpointIds ?? [])],
      savedOrigins: [...(binding.allowedOrigins ?? [])],
      pending: '',
    };
  }
  return { seamPort, families, authDirReady: true };
}
async function explainProtocol(family, modelId, protocol) {
  const explained = (await api('GET', `${prefix}/routing/explain?model=${encodeURIComponent(modelId)}&clientProtocol=${protocol}`)).value;
  if (!Array.isArray(explained?.eligible)) throw blocked(`pending: routing explain for ${family} ${protocol} has no eligible array; keys ${Object.keys(explained ?? {}).join(',')}`);
  if (explained.eligible.length === 0) throw fail(`${family} ${protocol} explain admitted nobody`);
  return explained;
}
async function driveNativeCell(item, familyState, key, port) {
  const cell = item.cell;
  const branch = nativeBranchOf(cell);
  const selected = familyState.branches?.[branch] ?? '';
  if (!selected) {
    item.kind = 'pending';
    item.interface = 'catalog-model';
    item.reason = `pending: ${cell.family} ${branch} has no current registry model in the credential catalog; publicModels ${(familyState.models ?? []).join(',')}`;
    return;
  }
  const driven = { ...cell, model: selected };
  const request = nativeClientRequest(driven);
  if (!request) throw fail(`${item.id} was driven without a client request`);
  if (publicAntigravityCompaction(driven)) {
    assert(request.parentPermit === (streamedNonstream(selected) ? 'stream' : 'execute'), `${item.id} public compaction parent permit drifted`);
    assert(request.internalChild === false, `${item.id} labeled public compaction as an automatic internal child`);
    assert(JSON.stringify(request.body.input ?? []).includes('compaction_trigger'), `${item.id} public compaction omitted compaction_trigger`);
  }
  await assertTargetEligible(driven, familyState, selected);
  const marks = await captureMarks();
  world.nativeUpstreamCell = { family: cell.family, operation: cell.operation, protocol: cell.protocol, path: cell.path };
  world.nativeBehavior = cell.outcome === 'local' ? 'fail' : request.kind;
  const run = await api('POST', request.path, request.body, { success: false, extra: ['--key-file', key] });
  if (cell.outcome === 'local') {
    await assertZeroPhysical(marks, 'native-local-count', selected);
    assertLocalCount(run, `${cell.family} local count`);
    return;
  }
  if (!fixtureLane()) {
    assertFeatureOffDenial(run, cell, marks);
    const rows = await awaitForwardRows(marks.logs, selected, { expectRow: false });
    for (const row of rows) assertRowAttribution(row, selected, `${cell.family} ${cell.operation}`, 1);
    return;
  }
  const hits = nativeHitsSince(marks.hits);
  assert(hits.length === 1, `${cell.family} ${cell.operation} sent ${hits.length} times`);
  assertPhysicalHit(hits[0], cell, port);
  assert(officialRefusalsSince(marks.refusals).length === 0, `${cell.family} dialed an official host`);
  const rows = await awaitForwardRows(marks.logs, selected, { expectRow: true });
  if (!rows.length) throw blocked(`pending: ${cell.family} ${cell.operation} published no forward row for ${selected}`);
  assert(rows.length === 1, `${cell.family} ${cell.operation} published ${rows.length} forward rows`);
  assertRowAttribution(rows[0], selected, `${cell.family} ${cell.operation}`, 1);
  if (request.kind === 'count') {
    assert(run.code === 0, `${cell.family} count CLI exit was ${run.code}`);
    if (!numericCount(run.value)) throw blocked(`pending: ${cell.family} count returned no numeric total after one upstream send`);
    assert(!renderedRun(run).includes('Gemini countTokens is not available'), `${cell.family} count used the Gemini estimation 501`);
    return;
  }
  assertClientPayload(run, driven, `${cell.family} ${cell.operation}`);
}
function assertSourceRefusal(item) {
  const marks = syncMarks();
  item.evidenceLevel = 'source-classification';
  assert(item.cell.outcome === 'refuse' || item.cell.disposition === 'source-refusal', `${item.id} is not a source refusal`);
  assert(item.cell.url === null, `${item.id} source refusal still has a matrix URL`);
  assert(nativeHitsSince(marks.hits).length === 0, `source-refusal ${item.id} changed the hit log`);
  assert(officialRefusalsSince(marks.refusals).length === 0, `source-refusal ${item.id} dialed an official host`);
  assert(world.proxyHits.length === marks.proxyHits, `source-refusal ${item.id} forwarded through the proxy`);
}
async function publicModelForAccount(account) {
  if (!account) return '';
  const identities = await listIdentities();
  const identity = identityForAccount(identities, account);
  const destinations = (await api('GET', `${prefix}/destinations`)).value?.destinations ?? [];
  const credentials = (await api('GET', `${prefix}/credentials`)).value?.credentials ?? [];
  for (const summary of identity?.credentials ?? []) {
    const credentialId = summary?.credential?.id ?? '';
    const row = credentials.find(item => item.id === credentialId || (summary?.legacy?.id && item.legacyAccountId === summary.legacy.id));
    const destination = destinations.find(item => item.id === row?.destinationId);
    const models = catalogModelsForScope(destination, summary?.bindings?.[0]?.modelScope);
    if (models[0]) return models[0];
  }
  return '';
}
async function assertPublicNegativeSend(label, path, body, key, model) {
  const marks = await captureMarks();
  world.nativeBehavior = 'json';
  const run = await api('POST', path, body, { success: false, extra: ['--key-file', key] });
  assert(run.code !== 0, `${label} returned success`);
  await assertZeroPhysical(marks, label, model);
  return run;
}
async function assertWrongMode(key) {
  const marks = syncMarks();
  const name = 'ocg-staged-bad-mode.json';
  await stageOwnedAuth(name, stagedAuthDocument('xai-no-auth-kind'));
  try {
    await reconcileStaged('wrong-mode');
    const bad = (await listCpaAccounts()).find(account => account.name === name);
    if (bad && bad.disabled !== true && bad.unavailable !== true) {
      const identity = identityForAccount(await listIdentities(), bad);
      const bindings = identity?.credentials?.flatMap(item => item.bindings ?? []) ?? [];
      const granted = bindings.some(binding => (binding.allowedEndpointIds ?? []).length > 0 && binding.enabled !== false);
      assert(!granted, 'wrong-mode became a grant');
    }
    assert(world.nativeHits.length === marks.hits, 'wrong-mode sent a native request');
    assert(officialRefusalsSince(marks.refusals).length === 0, 'wrong-mode dialed an official host');
    assert(world.proxyHits.length === marks.proxyHits, 'wrong-mode forwarded through the deny proxy');
    const affected = await publicModelForAccount(bad);
    if (affected) {
      await assertPublicNegativeSend('wrong-mode', '/v1/chat/completions', {
        model: affected,
        messages: [{ role: 'user', content: 'native-wrong-mode' }],
      }, key, affected);
    } else {
      results.push({
        name: 'wrong-mode-public-model',
        status: 'PASS',
        evidenceLevel: 'operator-runtime',
        detail: 'wrong-mode created no published model; unavailable authority recorded; no substituted target and no fabricated send',
      });
    }
  } finally {
    await removeOwnedAuth(name);
    await reconcileStaged('wrong-mode cleanup');
  }
}
async function assertWrongBase(key) {
  const marks = syncMarks();
  const shadowHits = [];
  const shadow = await openNativeSink(shadowHits);
  const shadowPort = shadow.address().port;
  const name = 'ocg-staged-bad-base.json';
  const document = stagedAuthDocument('kimi.ai');
  document.base_url = `http://127.0.0.1:${shadowPort}`;
  document.access_token = 'synthetic-kimi-bad-base-access';
  document.refresh_token = 'synthetic-kimi-bad-base-refresh';
  try {
    await stageOwnedAuth(name, document);
    const applied = await reconcileStaged('wrong-base');
    if (applied.phase === 'unavailable') throw fail('wrong-base dropped readiness');
    for (const identity of await listIdentities()) {
      for (const credential of identity.credentials ?? []) {
        for (const binding of credential.bindings ?? []) {
          assert(!(binding.allowedOrigins ?? []).some(origin => sameSeamOrigin(origin, shadowPort)), 'wrong-base stored a metadata loopback as a grant');
        }
      }
    }
    assert(shadowHits.length === 0, 'wrong-base dialed the metadata loopback');
    assert(world.nativeHits.length === marks.hits, 'wrong-base sent on the applied seam');
    assert(officialRefusalsSince(marks.refusals).length === 0, 'wrong-base dialed an official host');
    const bad = (await listCpaAccounts()).find(account => account.name === name);
    const affected = await publicModelForAccount(bad);
    if (affected) {
      const beforeShadow = shadowHits.length;
      await assertPublicNegativeSend('wrong-base', '/v1/chat/completions', {
        model: affected,
        messages: [{ role: 'user', content: 'native-wrong-base' }],
      }, key, affected);
      assert(shadowHits.length === beforeShadow, 'wrong-base public send reached the metadata loopback');
    } else {
      results.push({
        name: 'wrong-base-public-model',
        status: 'PASS',
        evidenceLevel: 'operator-runtime',
        detail: 'wrong-base created no published model; unavailable authority recorded; no substituted target and no fabricated send',
      });
    }
  } finally {
    await removeOwnedAuth(name);
    await reconcileStaged('wrong-base cleanup');
    await new Promise(done => shadow.close(done));
  }
}
async function assertStaleVersion(state, modelId) {
  const marks = syncMarks();
  const stale = await api('PATCH', `${prefix}/bindings/${state.binding.id}`, {
    expectedRevision: 0,
    processGeneration: 0,
    modelScope: { kind: 'only', models: [modelId] },
  }, { cas: true, success: false });
  assert(stale.code !== 0, 'stale-version binding revision was accepted');
  assert(world.nativeHits.length === marks.hits, 'stale-version sent a native request');
  assert(officialRefusalsSince(marks.refusals).length === 0, 'stale-version dialed an official host');
  results.push({
    name: 'stale-version',
    status: 'PASS',
    evidenceLevel: 'cas-binding',
    detail: 'stale binding CAS refusal; not a held native credential/version resend fence',
  });
}
async function assertNarrowedGrants(state, key, modelId) {
  const previous = revisionOf((await readRuntime()).facts, 'appliedRevision');
  await api('PATCH', `${prefix}/bindings/${state.binding.id}`, { allowedEndpointIds: [], allowedOrigins: [] }, { cas: true });
  assertAutomaticApply(await waitAutomaticApply(previous), 'revoked-grant');
  try {
    const marks = await captureMarks();
    world.nativeBehavior = 'json';
    const revoked = await api('POST', '/v1/chat/completions', {
      model: modelId,
      messages: [{ role: 'user', content: 'native-revoked-grant' }],
    }, { success: false, extra: ['--key-file', key] });
    assert(revoked.code !== 0, 'revoked-grant returned success');
    await assertZeroPhysical(marks, 'revoked-grant', modelId);
  } finally {
    const restore = revisionOf((await readRuntime()).facts, 'appliedRevision');
    await api('PATCH', `${prefix}/bindings/${state.binding.id}`, {
      allowedEndpointIds: state.savedIds,
      allowedOrigins: state.savedOrigins,
    }, { cas: true });
    assertAutomaticApply(await waitAutomaticApply(restore), 'native grant restore');
  }
}
async function assertNativePresence(state, key, modelId) {
  const previous = revisionOf((await readRuntime()).facts, 'appliedRevision');
  await api('PATCH', `${prefix}/bindings/${state.binding.id}`, { enabled: false }, { cas: true });
  assertAutomaticApply(await waitAutomaticApply(previous), 'native-presence binding');
  try {
    const marks = await captureMarks();
    world.nativeBehavior = 'json';
    const hidden = await api('POST', '/v1/chat/completions', {
      model: modelId,
      messages: [{ role: 'user', content: 'native-presence' }],
    }, { success: false, extra: ['--key-file', key] });
    assert(hidden.code !== 0, 'native-presence returned success');
    await assertZeroPhysical(marks, 'native-presence', modelId);
  } finally {
    const restore = revisionOf((await readRuntime()).facts, 'appliedRevision');
    await api('PATCH', `${prefix}/bindings/${state.binding.id}`, { enabled: true }, { cas: true });
    assertAutomaticApply(await waitAutomaticApply(restore), 'native-presence restore');
  }
  if (!state.account?.authIndex) throw blocked(`pending: ${state.account?.name ?? 'account'} account status has no authIndex`);
  await api('PATCH', `${prefix}/external-integrations/cpa/accounts/status`, {
    name: state.account.name,
    authIndex: state.account.authIndex,
    disabled: true,
  }, { cas: true });
  try {
    const updated = (await listCpaAccounts()).find(account => account.name === state.account.name);
    assert(updated?.disabled === true, 'native-presence account status did not disable');
    const marks = await captureMarks();
    world.nativeBehavior = 'json';
    const hidden = await api('POST', '/v1/chat/completions', {
      model: modelId,
      messages: [{ role: 'user', content: 'native-presence-account' }],
    }, { success: false, extra: ['--key-file', key] });
    assert(hidden.code !== 0, 'native-presence account status returned success');
    await assertZeroPhysical(marks, 'native-presence', modelId);
  } finally {
    await api('PATCH', `${prefix}/external-integrations/cpa/accounts/status`, {
      name: state.account.name,
      authIndex: state.account.authIndex,
      disabled: false,
    }, { cas: true, success: false });
  }
}
async function assertForbidden(modelId, key) {
  const marks = await captureMarks();
  const forbidden = await api('POST', nativeGeminiPath(modelId, 'embedContent'), {
    content: { parts: [{ text: 'native-forbidden-protocol' }] },
  }, { success: false, extra: ['--key-file', key] });
  assert(forbidden.code !== 0, 'forbidden-protocol embedContent succeeded');
  await assertZeroPhysical(marks, 'forbidden-protocol', modelId);
}
async function sendNative(cell, modelId, key, behavior) {
  const request = nativeClientRequest({ ...cell, model: modelId });
  world.nativeUpstreamCell = { family: cell.family, operation: cell.operation, protocol: cell.protocol, path: cell.path };
  world.nativeBehavior = behavior;
  const marks = await captureMarks();
  const run = await api('POST', request.path, request.body, { success: false, extra: ['--key-file', key], timeoutMs: behavior === 'hang' ? 20000 : undefined });
  return { run, marks, request };
}
async function assertOnePhysical(label, cell, port, sent) {
  await delay(400);
  const hits = nativeHitsSince(sent.marks.hits);
  assert(hits.length === 1, `${label} sent ${hits.length} times`);
  assert(sent.run.code !== 0, `${label} was reported as success`);
  assert(officialRefusalsSince(sent.marks.refusals).length === 0, `${label} dialed an official host`);
  if (fixtureLane()) assertPhysicalHit(hits[0], cell, port);
  const rows = await awaitForwardRows(sent.marks.logs, cell.modelId, { expectRow: false });
  for (const row of rows) assertRowAttribution(row, cell.modelId, label, 1);
}
async function assertTransport(prepared, state, key) {
  const cell = chatExecuteCell(state.family || 'codex');
  const tagged = { ...cell, modelId: state.modelId };
  for (const behavior of ['loss', 'truncate', 'empty']) {
    if (!fixtureLane()) {
      const sent = await sendNative(cell, state.modelId, key, behavior);
      assertFeatureOffDenial(sent.run, cell, sent.marks);
      continue;
    }
    const sent = await sendNative(cell, state.modelId, key, behavior);
    await assertOnePhysical(behavior, tagged, prepared.seamPort, sent);
  }
  if (fixtureLane()) {
    world.nativeBehavior = 'hang';
    const marks = await captureMarks();
    const cancel = begin([
      '--data-dir', world.dataDir, '--endpoint', world.endpoint, 'api', 'POST', '/v1/chat/completions',
      '--key-file', key, '--input', await writeJson('native-cancel', { model: state.modelId, messages: [{ role: 'user', content: 'native-cancel' }] }),
      '--output', join(world.root, 'native-cancel-output.json'),
    ], 30000);
    for (let attempt = 0; attempt < 50 && nativeHitsSince(marks.hits).length === 0; attempt += 1) await delay(100);
    assert(nativeHitsSince(marks.hits).length === 1, 'cancel did not reach exactly one physical send');
    killOwned(cancel.child.pid);
    const cancelled = await cancel.done;
    assert(cancelled.code !== 0, 'cancel was reported as success');
    await delay(500);
    assert(nativeHitsSince(marks.hits).length === 1, 'cancel replayed');
    const settings = (await api('GET', `${prefix}/settings`)).value;
    if (typeof settings.nonStreamTimeoutSecs !== 'number') throw blocked(`pending: settings has no nonStreamTimeoutSecs; keys ${Object.keys(settings ?? {}).join(',')}`);
    await api('PUT', `${prefix}/settings`, { nonStreamTimeoutSecs: 1 }, { cas: true });
    try {
      const deadline = await sendNative(cell, state.modelId, key, 'hang');
      assert(deadline.run.timedOut !== true, 'deadline was killed by the harness');
      await assertOnePhysical('deadline', tagged, prepared.seamPort, deadline);
    } finally {
      await api('PUT', `${prefix}/settings`, { nonStreamTimeoutSecs: settings.nonStreamTimeoutSecs }, { cas: true });
    }
    const bindings = Object.values(prepared.families).map(item => item.binding).filter(Boolean);
    for (const binding of bindings) {
      if (binding.id === state.binding.id) continue;
      await api('PATCH', `${prefix}/bindings/${binding.id}`, { enabled: false }, { cas: true });
    }
    try {
      world.native429Bearers = new Set();
      const single = await sendNative(cell, state.modelId, key, '429');
      await assertOnePhysical('confirmed-429', tagged, prepared.seamPort, single);
    } finally {
      for (const binding of bindings) await api('PATCH', `${prefix}/bindings/${binding.id}`, { enabled: true }, { cas: true, success: false });
    }
    return;
  }
  const denied = await sendNative(cell, state.modelId, key, 'hang');
  assertFeatureOffDenial(denied.run, cell, denied.marks);
}
function explainAuthorities(explained) {
  const routes = explained?.desiredRoutes ?? [];
  return routes.map(route => route?.authority).filter(item => item && typeof item === 'object');
}
async function assertMapAfterApply(prepared, state, key) {
  const modelId = state.modelId;
  const shadowHits = [];
  const shadow = await openNativeSink(shadowHits);
  world.nativeShadow = shadow;
  const previousSeam = world.trustedSeamJson;
  const shadowPort = shadow.address().port;
  world.trustedSeamJson = nativeSeamJson(shadowPort);
  let regranted = false;
  try {
    world.nativeUpstreamCell = { family: 'codex', operation: 'execute', protocol: 'responses', path: NATIVE_MATRIX.C1.path };
    world.nativeBehavior = 'json';
    const before = world.nativeHits.length;
    const run = await api('POST', '/v1/chat/completions', {
      model: modelId,
      messages: [{ role: 'user', content: 'native-stale-map' }],
    }, { success: false, extra: ['--key-file', key] });
    assert(shadowHits.length === 0, 'wrong-port reached the unapplied origin');
    assert(world.nativeHits.length - before <= 1, 'map-after-apply sent more than once to the applied origin');
    const savedOrigins = Object.values(prepared.families).flatMap(item => item.savedOrigins ?? []);
    const origins = (await listIdentities()).flatMap(identity => (identity.credentials ?? []).flatMap(item => (item.bindings ?? []).flatMap(binding => binding.allowedOrigins ?? [])));
    for (const origin of savedOrigins) assert(origins.includes(origin), 'map-after-apply changed applied grants');
    if (!fixtureLane()) assert(run.code !== 0, 'map-after-apply feature-off succeeded');
    await restartAcceptanceServe();
    world.nativeBehavior = 'json';
    const shadowBefore = shadowHits.length;
    const refused = await api('POST', '/v1/chat/completions', {
      model: modelId,
      messages: [{ role: 'user', content: 'native-old-grants' }],
    }, { success: false, extra: ['--key-file', key] });
    assert(shadowHits.length === shadowBefore, 'old grants reached the new seam');
    assert(refused.code !== 0, 'old grants were treated as the new seam');
    if (!fixtureLane()) return;
    const explained = (await api('GET', `${prefix}/routing/explain?model=${encodeURIComponent(modelId)}&clientProtocol=chat_completions`)).value;
    const authorities = explainAuthorities(explained);
    const matched = authorities.filter(item => sameSeamOrigin(item.origin, shadowPort) && typeof item.endpointId === 'string' && item.endpointId);
    if (!matched.length) {
      throw blocked(`pending: explain published no endpointId on the new seam; origins ${authorities.map(item => item.origin ?? '').join(',')}; keys ${Object.keys(explained ?? {}).join(',')}`);
    }
    const previous = revisionOf((await readRuntime()).facts, 'appliedRevision');
    await api('PATCH', `${prefix}/bindings/${state.binding.id}`, {
      allowedEndpointIds: [...new Set(matched.map(item => item.endpointId))],
      allowedOrigins: [...new Set(matched.map(item => item.origin))],
    }, { cas: true });
    regranted = true;
    assertAutomaticApply(await waitAutomaticApply(previous), 'map-after-apply regrant');
    world.nativeBehavior = 'json';
    const appliedRun = await api('POST', '/v1/chat/completions', {
      model: modelId,
      messages: [{ role: 'user', content: 'native-applied-map' }],
    }, { success: false, extra: ['--key-file', key] });
    assert(shadowHits.length === shadowBefore + 1, 'regranted map did not reach the new seam');
    const hit = shadowHits[shadowHits.length - 1];
    assert(String(hit.host).toLowerCase() === `127.0.0.1:${shadowPort}`, `regranted map host ${hit.host} is not the new seam`);
    assert(hit.path === NATIVE_MATRIX.C1.path && hit.query === NATIVE_MATRIX.C1.query, 'regranted map path or query drifted');
    assertClientPayload(appliedRun, { family: 'codex', protocol: 'chat_completions', operation: 'execute', ingress: 'chat_completions' }, 'map-after-apply');
  } finally {
    if (regranted) {
      await api('PATCH', `${prefix}/bindings/${state.binding.id}`, {
        allowedEndpointIds: state.savedIds,
        allowedOrigins: state.savedOrigins,
      }, { cas: true, success: false });
    }
    world.trustedSeamJson = previousSeam;
    world.nativeBehavior = 'fail';
    await restartAcceptanceServe();
  }
}
async function assertConfirmed429(prepared, ag, key) {
  const exemplar = nativeScenarioModel('antigravity', streamedNonstream(ag.modelId) ? 'streamed' : 'ordinary');
  const cell = nativeCatalog().find(item => item.family === 'antigravity' && item.ingress === 'chat_completions' && item.operation === 'execute' && item.model === exemplar);
  if (!cell) throw blocked(`pending: confirmed-429 has no chat execute cell for ${ag.modelId}`);
  const name = NATIVE_STAGED_FILES['antigravity-b'];
  await stageOwnedAuth(name, stagedAuthDocument('antigravity-b'));
  const others = Object.values(prepared.families).map(item => item.binding).filter(Boolean);
  try {
    await restartAcceptanceServe();
    await reconcileStaged('confirmed-429');
    const second = (await listCpaAccounts()).find(account => account.name === name);
    if (!second) throw blocked(`pending: ${name} was not listed after reconcile`);
    const identity = identityForAccount(await listIdentities(), second);
    const bindings = (identity?.credentials ?? []).flatMap(item => item.bindings ?? []);
    if (bindings.length !== 1) throw blocked(`pending: ${name} has ${bindings.length} bindings`);
    const secondBinding = bindings[0];
    await api('PATCH', `${prefix}/bindings/${secondBinding.id}`, { modelScope: { kind: 'only', models: [ag.modelId] }, enabled: true }, { cas: true });
    await api('PATCH', `${prefix}/bindings/${ag.binding.id}`, { modelScope: { kind: 'only', models: [ag.modelId] }, enabled: true }, { cas: true });
    for (const binding of others) {
      if (binding.id === ag.binding.id) continue;
      await api('PATCH', `${prefix}/bindings/${binding.id}`, { enabled: false }, { cas: true });
    }
    if (!fixtureLane()) {
      const sent = await sendNative(cell, ag.modelId, key, '429-pair');
      assert(nativeHitsSince(sent.marks.hits).length === 0, 'confirmed-429 feature-off reached the loopback');
      assert(sent.run.code !== 0, 'confirmed-429 feature-off succeeded');
      return;
    }
    const explained = await readTargetExplain(cell, ag, ag.modelId);
    const providers = explained.explained.eligible.map(candidate => candidate?.authority?.nativeProvider ?? '');
    if (providers.some(provider => provider !== ag.nativeProvider)) throw fail(`confirmed-429 eligible includes another provider ${providers.join(',')}`);
    if (!explained.matched.length || explained.explained.eligible.length < 2) throw blocked(`pending: confirmed-429 continuation has ${explained.explained.eligible.length} eligible credentials for binding ${ag.binding.id}`);
    world.native429Bearers = new Set();
    const sent = await sendNative(cell, ag.modelId, key, '429-pair');
    await delay(400);
    const hits = nativeHitsSince(sent.marks.hits);
    assert(hits.length === 2, `confirmed-429 sent ${hits.length} times`);
    assert(hits[0].bearer !== hits[1].bearer, 'confirmed-429 reused one bearer');
    assert(hits.every(hit => nativeBearerClass(hit.bearer) !== 'client' && nativeBearerClass(hit.bearer) !== 'none'), 'confirmed-429 bearer was the client key or empty');
    assertPhysicalHit(hits[0], cell, prepared.seamPort);
    assertPhysicalHit(hits[1], cell, prepared.seamPort);
    assertClientPayload(sent.run, { ...cell, model: ag.modelId }, 'confirmed-429');
  } finally {
    await removeOwnedAuth(name);
    for (const binding of others) await api('PATCH', `${prefix}/bindings/${binding.id}`, { enabled: true }, { cas: true, success: false });
    await reconcileStaged('confirmed-429 cleanup');
  }
}
async function assertUnknownAntigravityAlt(state, key) {
  const modelId = state.modelId;
  const marks = await captureMarks();
  world.nativeBehavior = 'json';
  const run = await api('POST', `${nativeGeminiPath(modelId, 'streamGenerateContent')}?alt=other`, {
    contents: [{ role: 'user', parts: [{ text: 'native-unknown-alt' }] }],
  }, { success: false, extra: ['--key-file', key] });
  assert(run.code !== 0, 'unknown Antigravity alt returned success');
  await assertZeroPhysical(marks, 'unknown-variant', modelId);
}
async function assertLaterUnrelatedSend(prepared, state, key) {
  const cell = chatExecuteCell(state.family || 'codex');
  if (!cell) throw blocked(`pending: later unrelated send has no chat execute cell for ${state.family}`);
  await assertTargetEligible(cell, state, state.modelId);
  const sent = await sendNative(cell, state.modelId, key, 'json');
  if (!fixtureLane()) {
    assertFeatureOffDenial(sent.run, cell, sent.marks);
    return;
  }
  const hits = nativeHitsSince(sent.marks.hits);
  assert(hits.length === 1, `later unrelated send sent ${hits.length} times`);
  assertPhysicalHit(hits[0], cell, prepared.seamPort);
  assert(officialRefusalsSince(sent.marks.refusals).length === 0, 'later unrelated send dialed an official host');
  assertClientPayload(sent.run, { ...cell, model: state.modelId }, 'later-unrelated-send');
}
async function runNativeNegatives(prepared, key, plan) {
  const note = (id, reason) => plan.push({
    id,
    kind: 'pending',
    interface: id,
    cell: null,
    reason,
    evidenceLevel: NATIVE_SDK_REQUIRED_INTERFACES.includes(id) ? 'required-sdk-host' : 'operator-runtime',
  });
  const codex = prepared.families.codex;
  if (!codex?.modelId || !codex.binding) {
    for (const id of ['stale-version', 'revoked-grant', 'native-presence', 'forbidden-protocol', 'loss', 'truncate', 'empty', 'cancel', 'deadline', 'map-after-apply', 'wrong-port']) {
      note(id, codex?.pending || 'pending: codex has no published model');
    }
  } else {
    codex.family = 'codex';
    await assertStaleVersion(codex, codex.modelId);
    await assertNarrowedGrants(codex, key, codex.modelId);
    await assertNativePresence(codex, key, codex.modelId);
    await assertForbidden(codex.modelId, key);
    await assertTransport(prepared, codex, key);
    await assertMapAfterApply(prepared, codex, key);
  }
  await assertWrongMode(key);
  await assertWrongBase(key);
  const ag = prepared.families.antigravity;
  if (!ag?.modelId || !ag.binding) {
    note('confirmed-429', ag?.pending || 'pending: antigravity has no published model');
    note('unknown-variant', ag?.pending || 'pending: antigravity has no published model');
  } else {
    await assertUnknownAntigravityAlt(ag, key);
    await assertConfirmed429(prepared, ag, key);
  }
  if (codex?.modelId && codex.binding) {
    await isolateForeignBindings(prepared, 'codex').then(async restore => {
      try { await assertLaterUnrelatedSend(prepared, { ...codex, family: 'codex' }, key); }
      finally { await restore(); }
    });
  }
}
function nativeProjectionDiff(before, after) {
  const fields = [];
  const secretNames = new Set(['secretRefName']);
  const walk = (left, right, path) => {
    if (Object.is(left, right)) return;
    if (Array.isArray(left) || Array.isArray(right)) {
      if (JSON.stringify(left) !== JSON.stringify(right)) fields.push(path || 'root');
      return;
    }
    if (!left || !right || typeof left !== 'object' || typeof right !== 'object') {
      fields.push(path || 'root');
      return;
    }
    for (const key of new Set([...Object.keys(left), ...Object.keys(right)])) {
      const next = path ? `${path}.${key}` : key;
      if (secretNames.has(key)) {
        if (left[key] !== right[key]) fields.push(next);
        continue;
      }
      walk(left[key], right[key], next);
    }
  };
  walk(before, after, '');
  return fields;
}
async function nativeAuthFingerprints() {
  const dir = ownedAuthDir();
  let names;
  try { names = await readdir(dir); }
  catch { throw blocked('pending: data/cpa/auth is absent before native archive'); }
  const files = {};
  for (const name of names.sort()) {
    if (name !== basename(name) || name.includes('..')) throw fail('native auth name is not a direct basename');
    const file = join(dir, name);
    if (!within(dir, file)) throw fail('native auth fingerprint left data/cpa/auth');
    files[name] = await sha256File(file);
  }
  return files;
}
function projectNativeControl(identities, destinations, accounts) {
  const credentials = [];
  for (const identity of identities ?? []) {
    const identityId = identity?.identity?.id ?? '';
    for (const summary of identity.credentials ?? []) {
      const credential = summary.credential ?? {};
      credentials.push({
        identityId,
        id: credential.id ?? '',
        purpose: credential.purpose ?? null,
        enabled: credential.enabled === true,
        version: credential.version ?? null,
        authStateVersion: credential.authStateVersion ?? null,
        materialKind: credential.materialKind ?? null,
        secretRefName: credential.secretRef ? basename(String(credential.secretRef)) : null,
        bindings: (summary.bindings ?? []).map(binding => ({
          id: binding.id ?? '',
          connectionId: binding.connectionId ?? '',
          enabled: binding.enabled === true,
          modelScope: binding.modelScope ?? null,
          allowedEndpointIds: [...(binding.allowedEndpointIds ?? [])].sort(),
          allowedOrigins: [...(binding.allowedOrigins ?? [])].sort(),
        })).sort((left, right) => String(left.id).localeCompare(String(right.id))),
      });
    }
  }
  credentials.sort((left, right) => String(left.id).localeCompare(String(right.id)));
  const destinationRows = (destinations ?? []).map(destination => ({
    id: destination.id ?? '',
    baseUrl: destination.baseUrl ?? null,
    protocols: destination.protocols ?? [],
    catalog: (destination.catalog ?? []).map(model => ({
      publicModel: model.publicModel ?? '',
      upstreamModel: model.upstreamModel ?? '',
      protocols: model.protocols ?? [],
      preferred: model.preferred ?? null,
      enabled: model.enabled !== false,
    })),
  })).sort((left, right) => String(left.id).localeCompare(String(right.id)));
  const accountRows = (accounts ?? []).map(account => ({
    name: account.name ?? '',
    provider: account.provider ?? '',
    authIndex: account.authIndex ?? null,
    disabled: account.disabled === true,
  })).sort((left, right) => String(left.name).localeCompare(String(right.name)));
  return { credentials, destinationRows, accountRows };
}
async function readNativeProjection() {
  const identities = await listIdentities();
  const destinations = (await api('GET', `${prefix}/destinations`)).value?.destinations;
  if (!Array.isArray(destinations)) throw blocked('pending: destinations payload has no destinations array');
  const accounts = await listCpaAccounts();
  return projectNativeControl(identities, destinations, accounts);
}
function seededArchiveToken(family) {
  if (family === 'antigravity') return stagedAuthDocument('antigravity').access_token;
  if (family === 'kimi.ai') return stagedAuthDocument('kimi.ai').access_token;
  if (family === 'xai.api') return stagedAuthDocument('xai.api').access_token;
  const documents = nativeSourceDocuments();
  if (family === 'codex') return documents.codex.tokens.access_token;
  if (family === 'anthropic') return documents.claude.claudeAiOauth.accessToken;
  if (family === 'kimi.com') return documents.kimi.access_token;
  if (family === 'xai.cli') {
    const entry = documents.xai['https://auth.x.ai::b1a00492-073a-47ea-816f-4c329264a828'];
    return typeof entry?.key === 'string' ? entry.key : null;
  }
  return null;
}
function physicalBearerAttribution(family, bearer) {
  const bearerClass = nativeBearerClass(bearer);
  const seeded = seededArchiveToken(family);
  if (!seeded) {
    return {
      kind: 'ambiguous',
      matched: false,
      note: `LIMIT: seeded synthetic token for ${family} is not unambiguously known; bearer class ${bearerClass} is not a public DTO auth-filepath or material-fingerprint proof`,
    };
  }
  const seededPrefix = secretSafeDigest(seeded).slice(0, 16);
  if (!bearer) {
    return {
      kind: 'mismatch',
      matched: false,
      seededPrefix,
      actualPrefix: 'none',
      note: `owned seeded ${family} token is unambiguous but the physical bearer was empty; seeded sha256 ${seededPrefix}`,
    };
  }
  const actualDigest = secretSafeDigest(bearer);
  const seededDigest = secretSafeDigest(seeded);
  const actualPrefix = actualDigest.slice(0, 16);
  if (actualDigest === seededDigest) {
    return {
      kind: 'match',
      matched: true,
      actualPrefix,
      seededPrefix,
      note: `physical synthetic bearer sha256 ${actualPrefix} matched the owned seeded ${family} fixture token. LIMIT: this is not public DTO auth-filepath or material-fingerprint proof`,
    };
  }
  return {
    kind: 'mismatch',
    matched: false,
    actualPrefix,
    seededPrefix,
    note: `owned seeded ${family} token is unambiguous but the physical bearer sha256 ${actualPrefix} did not match seeded sha256 ${seededPrefix}; bearer class ${bearerClass}`,
  };
}
function archiveBearerVerdict(attribution) {
  if (attribution?.kind === 'match' && attribution.matched === true) return { status: 'PASS', claimBearer: true, detail: attribution.note };
  if (attribution?.kind === 'mismatch') {
    return {
      status: 'FAIL',
      claimBearer: false,
      detail: `${attribution.note}. Hash-only diagnostics; not a public DTO auth-filepath or material-fingerprint proof`,
    };
  }
  return {
    status: 'OPEN',
    claimBearer: false,
    detail: attribution?.note || 'LIMIT: archive bearer identity mapping is ambiguous; attribution remains required incomplete',
  };
}
function nativeArchiveFamily(prepared) {
  const readyFamily = NATIVE_MODES.map(mode => mode.family).find(family => prepared.families[family]?.modelId && prepared.families[family]?.binding);
  if (!readyFamily) return { readyFamily: '', state: null, cell: null };
  const state = prepared.families[readyFamily];
  return { readyFamily, state, cell: chatExecuteCell(readyFamily) };
}
async function readArchivePin(cell, state) {
  const { matched } = await readTargetExplain(cell, state, state.modelId);
  const authority = matched[0]?.authority;
  if (!authority || typeof authority !== 'object') return null;
  return {
    bindingId: authority.bindingId ?? '',
    credentialId: authority.credentialId ?? '',
    nativeProvider: authority.nativeProvider ?? '',
    endpointFingerprint: typeof authority.endpointFingerprint === 'string' ? authority.endpointFingerprint : '',
    authId: authority.authId ?? null,
    credentialVersion: authority.credentialVersion ?? null,
    endpointId: authority.endpointId ?? null,
  };
}
function assertRestoredRoutePin(authority, pin, state) {
  assert(authority?.bindingId === state.binding.id, 'restored explain binding is not the family binding');
  assert(authority?.credentialId === state.credentialId, 'restored explain credential is not the family credential');
  assert(authority?.nativeProvider === state.nativeProvider, 'restored explain provider is not the family provider');
  if (!pin?.endpointFingerprint) {
    return 'LIMIT: pre-stop RoutingRouteAuthority exposed no endpointFingerprint, so the restored native pin is not proven';
  }
  assert(typeof authority?.endpointFingerprint === 'string' && authority.endpointFingerprint === pin.endpointFingerprint, 'restored endpointFingerprint does not match the pre-stop route pin');
  assert(String(authority.authId) === String(pin.authId), 'restored authId does not match the pre-stop route pin');
  assert(String(authority.credentialVersion) === String(pin.credentialVersion), 'restored credentialVersion does not match the pre-stop route pin');
  assert(String(authority.endpointId) === String(pin.endpointId), 'restored endpointId does not match the pre-stop route pin');
  return 'public route pin endpointFingerprint, authId, credentialVersion, and endpointId matched the pre-stop explain. LIMIT: bearer class staged or other is not an auth-file credential pin; material fingerprints, ciphers, hop tokens, and auth file paths are absent from RoutingRouteAuthority';
}
async function waitRestoredOwnedReady(previousGeneration) {
  let last = await readRuntime();
  const deadline = Date.now() + 8000;
  for (;;) {
    const desired = revisionOf(last.facts, 'desiredRevision');
    const applied = revisionOf(last.facts, 'appliedRevision');
    const status = statusOf(last.facts);
    const contract = (await api('GET', `${prefix}/contract`)).value;
    const generation = contract?.processGeneration;
    const generationMoved = generation !== undefined && generation !== null && String(generation) !== String(previousGeneration);
    const caughtUp = desired !== null && applied !== null && String(desired) === String(applied);
    const owned = last.value?.running === true && last.value?.owned === true;
    if (status === 'apply_failed') throw fail(`restored native runtime apply failed; desired ${desired} applied ${applied}`);
    if (status === 'applied' && caughtUp && owned && generationMoved && policyReadyNow(last) && Date.now() < deadline) {
      await delay(250);
      const confirm = await readRuntime();
      const confirmContract = (await api('GET', `${prefix}/contract`)).value;
      const desired2 = revisionOf(confirm.facts, 'desiredRevision');
      const applied2 = revisionOf(confirm.facts, 'appliedRevision');
      const generation2 = confirmContract?.processGeneration;
      if (statusOf(confirm.facts) === 'applied' && String(desired2) === String(applied2) && String(applied2) === String(applied) && confirm.value?.running === true && confirm.value?.owned === true && policyReadyNow(confirm) && String(generation2) === String(generation)) {
        requirePolicy(confirm.value);
        return { runtime: confirm, contract: confirmContract };
      }
      last = confirm;
      continue;
    }
    if (Date.now() >= deadline) {
      throw fail(`restored native runtime did not reach owned Ready within 8000ms; status ${status} desired ${desired} applied ${applied} generation moved ${generationMoved}`);
    }
    await delay(250);
    last = await readRuntime();
  }
}
async function assertNativeArchiveRestore(prepared) {
  const selected = nativeArchiveFamily(prepared);
  const beforePin = selected.cell ? await readArchivePin(selected.cell, selected.state) : null;
  const beforeAuth = await nativeAuthFingerprints();
  assert(Object.keys(beforeAuth).length > 0, 'native archive found no owned auth files');
  const beforeProjection = await readNativeProjection();
  const beforeRuntime = await readRuntime();
  requirePolicy(beforeRuntime.value);
  const beforeContract = (await api('GET', `${prefix}/contract`)).value;
  const previousGeneration = beforeContract?.processGeneration;
  assert(previousGeneration !== undefined && previousGeneration !== null, 'contract processGeneration was absent before native archive');
  const childPort = beforeRuntime.value.port;
  const previousPids = listenerPids(childPort);
  assert(previousPids.length === 1, `native archive child port ${childPort} has ${previousPids.length} listeners`);
  const snapshot = join(world.root, 'native-profile.snapshot');
  await stopServe();
  const created = await invoke(['--data-dir', world.dataDir, 'backup', 'create', '--output', snapshot], { timeoutMs: 60000 });
  assert(created.code === 0, created.stderr);
  const createReceipt = parseJson(created.stdout);
  assert(createReceipt.state === 'created' && createReceipt.count > 0, 'native backup create receipt was not a snapshot');
  for (const secret of secrets) assert(!created.stdout.includes(secret), 'native backup create printed a secret');
  const snapshotSha = await sha256File(snapshot);
  assert(/^[0-9a-f]{64}$/.test(snapshotSha), 'native backup bytes have no SHA-256');
  assert(snapshotSha !== String(previousGeneration), 'backup SHA and process generation were collapsed into one proof');
  const restoredDir = join(world.root, 'native-restored');
  await mkdir(restoredDir, { recursive: true });
  const restored = await invoke(['--data-dir', restoredDir, 'backup', 'restore', '--input', snapshot], { timeoutMs: 60000 });
  assert(restored.code === 0, restored.stderr);
  assert(parseJson(restored.stdout).state === 'restored', 'native backup restore receipt was not restored');
  for (const secret of secrets) assert(!restored.stdout.includes(secret), 'native backup restore printed a secret');
  await startServe(restoredDir);
  await api('POST', `${prefix}/auth/login`, { username: 'cpa-acceptance', password: 'synthetic-cpa-admin-password' }, { cas: true });
  const restoredReady = await waitRestoredOwnedReady(previousGeneration);
  const reopened = restoredReady.runtime;
  const nextContract = restoredReady.contract;
  assert(String(nextContract.processGeneration) !== String(previousGeneration), 'reopened process reused the stopped process generation');
  const nextPids = listenerPids(reopened.value.port);
  assert(nextPids.length === 1, `restored child port ${reopened.value.port} has ${nextPids.length} listeners`);
  assert(!previousPids.includes(nextPids[0]), 'restored child reused the stopped listener pid');
  ownedPids.add(nextPids[0]);
  world.childPid = nextPids[0];
  const afterAuth = await nativeAuthFingerprints();
  assert(JSON.stringify(afterAuth) === JSON.stringify(beforeAuth), 'restored native auth basename SHA-256 set changed');
  const afterProjection = await readNativeProjection();
  const drifted = nativeProjectionDiff(beforeProjection, afterProjection);
  assert(drifted.length === 0, `restored native identity fields differ: ${drifted.join(', ')}`);
  const readyFamily = selected.readyFamily;
  if (!readyFamily) {
    if (!fixtureLane()) {
      results.push({ name: 'native-oauth-archive-restore', status: 'SKIP', detail: 'feature-off archive ownership, connection, version, grants, and owned Ready completed; no ready family for a route pin or physical send' });
      console.log('SKIP native-oauth-archive-restore physical send: feature-off has no ready family');
      return;
    }
    throw blocked('pending: native archive restore found no family with a published model');
  }
  const state = selected.state;
  const cell = selected.cell;
  if (!cell) throw blocked(`pending: ${readyFamily} has no chat execute cell`);
  let restoreArchiveIsolation = async () => {};
  try {
    restoreArchiveIsolation = await isolateForeignBindings(prepared, readyFamily);
    await assertTargetEligible(cell, state, state.modelId);
    const explained = await readTargetExplain(cell, state, state.modelId);
    const pinNote = assertRestoredRoutePin(explained.matched[0]?.authority, beforePin, state);
    const key = await gatewayKeyFile();
    const request = nativeClientRequest({ ...cell, model: state.modelId });
    if (!request) throw fail('native archive send has no client request');
    world.nativeUpstreamCell = { family: readyFamily, operation: cell.operation, protocol: cell.protocol, path: cell.path };
    world.nativeBehavior = 'json';
    const marks = await captureMarks();
    const run = await api('POST', request.path, request.body, { success: false, extra: ['--key-file', key] });
    if (!fixtureLane()) {
      assertFeatureOffDenial(run, cell, marks);
      results.push({ name: 'native-oauth-archive-restore', status: 'SKIP', detail: `feature-off send stayed off the loopback; archive bytes, new process generation, owned Ready, and ${pinNote}` });
      console.log('SKIP native-oauth-archive-restore physical send: production feature-off keeps the official origin');
      return;
    }
    const hits = nativeHitsSince(marks.hits);
    assert(hits.length === 1, `native archive restore sent ${hits.length} times`);
    assertPhysicalHit(hits[0], cell, prepared.seamPort);
    assert(officialRefusalsSince(marks.refusals).length === 0, 'native archive restore dialed an official host');
    assertClientPayload(run, { ...cell, model: state.modelId }, 'native-oauth-archive-restore');
    assert(snapshotSha !== String(nextContract.processGeneration), 'restored process generation replaced the backup byte hash');
    const attribution = physicalBearerAttribution(readyFamily, hits[0].bearer);
    const bearerGate = archiveBearerVerdict(attribution);
    const six = `stopped backup create and restore, new process generation, owned Ready, ownership, connection, version, grants, ${pinNote}, and one physical synthetic send`;
    if (bearerGate.status === 'FAIL') throw fail(`native archive restore bearer attribution failed: ${bearerGate.detail}`);
    if (bearerGate.status === 'OPEN') {
      results.push({
        name: 'native-oauth-archive-restore',
        status: 'PASS',
        evidenceLevel: 'operator-runtime',
        detail: `${six}. Bearer attribution not claimed.`,
      });
      results.push({
        name: 'native-oauth-archive-bearer',
        status: 'OPEN',
        evidenceLevel: 'required-incomplete',
        detail: bearerGate.detail,
      });
      console.log(`OPEN native-oauth-archive-bearer: ${bearerGate.detail}`);
      return;
    }
    results.push({
      name: 'native-oauth-archive-restore',
      status: 'PASS',
      evidenceLevel: 'operator-runtime',
      detail: `${six}. ${bearerGate.detail}`,
    });
  } finally {
    await restoreArchiveIsolation();
  }
}
async function runNativeOperatorWorkflows() {
  let workflowError;
  let restoreIsolation = async () => {};
  try {
    const prepared = await prepareNativeOperator();
    await assertNativeArchiveRestore(prepared);
    const plan = nativeScenarioPlan();
    const key = await gatewayKeyFile();
    let isolatedFamily = '';
    for (const item of plan) {
      if (item.kind === 'pending' && item.evidenceLevel === 'required-sdk-host') continue;
      if (item.kind === 'pending') continue;
      if (item.kind === 'source-refusal') {
        assertSourceRefusal(item);
        continue;
      }
      const state = prepared.families[item.cell.family];
      if (!state?.modelId || !state.binding) {
        item.kind = 'pending';
        item.interface = item.interface || 'catalog-model';
        item.reason = state?.pending || `pending: ${item.cell.family} has no published model`;
        item.evidenceLevel = 'operator-runtime';
        continue;
      }
      if (isolatedFamily !== item.cell.family) {
        await restoreIsolation();
        restoreIsolation = await isolateForeignBindings(prepared, item.cell.family);
        isolatedFamily = item.cell.family;
      }
      await driveNativeCell(item, state, key, prepared.seamPort);
      item.evidenceLevel = item.evidenceLevel || 'operator-runtime';
    }
    await restoreIsolation();
    restoreIsolation = async () => {};
    await runNativeNegatives(prepared, key, plan);
    const operatorPending = plan.filter(item => item.kind === 'pending' && item.evidenceLevel !== 'required-sdk-host' && !NATIVE_SDK_REQUIRED_INTERFACES.includes(item.interface));
    if (operatorPending.length) {
      throw blocked(`operator pending remains:\n${operatorPending.map(item => `${item.id}: ${item.reason}`).join('\n')}`);
    }
    const sdkRequired = [];
    for (const item of plan) {
      if (item.kind !== 'pending') continue;
      if (item.evidenceLevel !== 'required-sdk-host' && !NATIVE_SDK_REQUIRED_INTERFACES.includes(item.interface)) continue;
      const iface = item.interface || 'unclassified';
      if (sdkRequired.some(row => row.interface === iface)) continue;
      sdkRequired.push({ interface: iface, reason: item.reason, id: item.id });
    }
    for (const [iface, reason] of NATIVE_PENDING_INTERFACES) {
      if (!NATIVE_SDK_REQUIRED_INTERFACES.includes(iface)) continue;
      if (sdkRequired.some(row => row.interface === iface)) continue;
      if (iface === 'source-compact' && plan.some(item => item.cell && publicAntigravityCompaction(item.cell) && item.kind === 'network')) {
        sdkRequired.push({ interface: iface, reason, id: `interface:${iface}` });
        continue;
      }
      sdkRequired.push({ interface: iface, reason, id: `interface:${iface}` });
    }
    for (const row of sdkRequired) {
      results.push({
        name: `sdk-required:${row.interface}`,
        status: 'OPEN',
        evidenceLevel: 'required-sdk-host',
        detail: `${row.reason}. Missing critical native HTTP stream-refresh/bootstrap/private-internal/final-dispatch/no-replay evidence stays required open, not PASS or SKIP. Current 011f unfiltered host and selected TestOCGNativeDispatchGuard receipts are not this operator row. Full CLI stays incomplete.`,
      });
      console.log(`OPEN sdk-required:${row.interface}: required SDK/host evidence is not closed by this operator run`);
    }
    results.push({
      name: 'native-operator-evidence-split',
      status: 'PASS',
      evidenceLevel: 'operator-runtime',
      detail: `operator cells ran; public Antigravity compaction is an operator gate; ${sdkRequired.length} required SDK/host interfaces remain OPEN; closeout false`,
    });
  } catch (error) {
    workflowError = error;
  } finally {
    try { await restoreIsolation(); } catch (error) { if (!workflowError) workflowError = error; }
    world.trustedSeamJson = '';
    world.nativeRoots = undefined;
    world.nativeBehavior = 'fail';
    world.nativeUpstreamCell = undefined;
    try { await closeNativeLoopback(); } catch (error) { if (!workflowError) workflowError = error; }
    try { await stopServe(); } catch (error) { if (!workflowError) workflowError = error; }
  }
  if (workflowError) throw workflowError;
}
async function assertNativeProductionFeatureOff() {
  if (!options.productionFeatureOff) throw fail('production feature-off was entered without --production-feature-off');
  await runNativeOperatorWorkflows();
}

stage('native-operator-workflows', async () => {
  if (options.nativeFixtures) throw fail('native fixture tried to start a product process');
  const admission = nativeAdmission();
  if (admission) throw blocked(admission);
  if (options.productionFeatureOff) {
    await assertNativeProductionFeatureOff();
    return;
  }
  await runNativeOperatorWorkflows();
});

stage('separate-receipts', async () => {
  const rows = [
    { name: 'linux-macos-cross-build', status: 'not-claimed', detail: 'separate from this Windows x64 process receipt' },
    { name: 'real-oauth-provider', status: 'not-claimed', detail: 'loopback API keys are not native OAuth or a real provider' },
    { name: 'whole-goal-closeout', status: 'not-claimed', detail: 'this harness is not the full CLI closeout' },
    { name: 'separate-receipts', status: 'not-claimed', detail: 'records the three non-claims; this row is not acceptance and not closeout' },
  ];
  for (const row of rows) {
    results.push(row);
    console.log(`${row.status} ${row.name}: ${row.detail}`);
  }
});

async function gatewayKeyFile() {
  if (world.gatewayKeyFile) return world.gatewayKeyFile;
  const connection = (await api('GET', `${prefix}/connection`)).value;
  if (!connection.primaryKey || connection.primaryKey === '[redacted]') throw fail('private connection output did not contain the client key');
  world.primaryKey = remember(connection.primaryKey);
  world.clientKeys.add(connection.primaryKey);
  world.gatewayKeyFile = join(world.root, 'gateway-key.txt');
  await writeFile(world.gatewayKeyFile, connection.primaryKey);
  return world.gatewayKeyFile;
}
async function writeJson(name, value) {
  const path = join(world.root, `${name}.json`);
  await writeFile(path, stringifyJson(value));
  return path;
}
async function restrictionSnapshot() {
  const credentials = (await api('GET', `${prefix}/credentials`)).value.credentials ?? [];
  const restrictions = (await api('GET', `${prefix}/routing/temporary-unavailability/restrictions`)).value;
  const recovery = credentials.map(row => row.quotaRecovery ?? null);
  return JSON.stringify({ recovery, restrictions: restrictions.restrictions ?? restrictions.items ?? restrictions });
}

async function writeReceipt(status) {
  const path = join(workRoot, 'receipt.json');
  const payload = {
    status,
    closeout: false,
    wholeCli: 'incomplete',
    at: new Date().toISOString(),
    binary: options.binary,
    hostDir: options.hostDir,
    profile: world.dataDir ? world.root : null,
    artifactLock: {
      path: artifactLockStatus.path,
      present: artifactLockStatus.present,
      accepted: artifactLockStatus.accepted,
      trustedSHA256: artifactLockStatus.trustedSHA256,
    },
    placeholderSHA256,
    candidateSHA256: candidateSha || null,
    results,
  };
  await mkdir(workRoot, { recursive: true });
  await writeFile(path, `${JSON.stringify(payload, null, 2)}\n`);
}

async function lockPresence() {
  try {
    const info = await stat(artifactLockPath);
    return { present: true, size: info.size, mtimeMs: info.mtimeMs };
  } catch (error) {
    if (error?.code === 'ENOENT') return { present: false };
    return { present: true, unreadable: true };
  }
}
async function runLockFixtures() {
  const cases = [];
  const before = await lockPresence();
  const run = (name, fn) => {
    try {
      fn();
      cases.push({ name, status: 'PASS' });
    } catch (error) {
      cases.push({ name, status: 'FAIL', detail: String(error.message ?? error) });
    }
  };
  const expect = (condition, message) => { if (!condition) throw new Error(message); };
  const hex = {
    file: 'ab'.repeat(32),
    other: 'cd'.repeat(32),
    host: '12'.repeat(32),
    overlay: '34'.repeat(32),
    build: '56'.repeat(32),
    linux: '78'.repeat(32),
    mac: '90'.repeat(32),
    second: 'ef'.repeat(32),
  };
  const sampleManifest = (overrides = {}) => ({
    sourceCommit: sourceIdentity.sourceCommit,
    sourceVersion: sourceIdentity.sourceVersion,
    protocolVersion: sourceIdentity.protocolVersion,
    buildIdentity: hex.build,
    hostSHA256: hex.host,
    overlaySHA256: hex.overlay,
    capabilities: [...sourceIdentity.capabilities],
    executableSHA256: hex.file,
    variant: 'production',
    buildTags: [],
    build: { goos: 'windows', goarch: 'amd64' },
    ...overrides,
  });
  const sampleLock = () => ({
    schemaVersion: 1,
    sourceCommit: sourceIdentity.sourceCommit,
    sourceVersion: sourceIdentity.sourceVersion,
    protocolVersion: sourceIdentity.protocolVersion,
    buildIdentity: hex.build,
    hostSHA256: hex.host,
    overlaySHA256: hex.overlay,
    requiredCapabilities: ['attempt-boundary', 'policy-ipc-v1'],
    artifacts: [
      { os: 'windows', arch: 'x86_64', variant: 'production', executable: 'ocg-cpa-host.exe', sha256: hex.file, verification: 'host-suite' },
      { os: 'linux', arch: 'x86_64', variant: 'production', executable: 'ocg-cpa-host-linux-amd64', sha256: hex.linux, verification: 'compiled-only' },
      { os: 'macos', arch: 'aarch64', variant: 'production', executable: 'ocg-cpa-host-darwin-arm64', sha256: hex.mac, verification: 'compiled-only' },
    ],
  });
  const pairedLock = () => {
    const lock = sampleLock();
    lock.artifacts.push({
      os: 'windows', arch: 'x86_64', variant: 'native-loopback-fixture', executable: 'ocg-cpa-host.exe', sha256: hex.second, verification: 'host-suite',
    });
    return lock;
  };
  const fixtureManifest = (overrides = {}) => sampleManifest({
    variant: 'native-loopback-fixture',
    buildTags: ['ocg_native_loopback_fixture'],
    executableSHA256: hex.second,
    ...overrides,
  });
  const decide = (lock, manifest, fileSha, platform = { os: 'win32', arch: 'x64' }, compileMode = 'production') => evaluateArtifactLock({
    lockText: typeof lock === 'string' || lock == null ? lock : JSON.stringify(lock),
    manifest,
    fileSha,
    platform,
    compileMode,
  });
  const rejected = (name, decision, needle) => {
    expect(decision.trustedSha === '', `${name} returned trustedSha ${decision.trustedSha}`);
    expect(decision.problems.length > 0, `${name} returned no problem`);
    if (needle) {
      expect(decision.problems.some(item => item.toLowerCase().includes(needle.toLowerCase())), `${name} problems were ${decision.problems.join('; ')}`);
    }
  };
  run('missing-lock', () => rejected('missing-lock', decide(null, sampleManifest(), hex.file), 'absent'));
  run('empty-lock', () => rejected('empty-lock', decide('   ', sampleManifest(), hex.file), 'empty'));
  run('invalid-lock-json', () => rejected('invalid-lock-json', decide('{', sampleManifest(), hex.file), 'not JSON'));
  run('schema-example-is-not-accepted-data', () => {
    const example = `{
      "schemaVersion": 1,
      "sourceCommit": "6fecc6e5567912661654a4eaf9b8f5436facd1c2",
      "sourceVersion": "v8.0.10",
      "protocolVersion": 1,
      "buildIdentity": "64 lowercase hex",
      "hostSHA256": "64 lowercase hex",
      "overlaySHA256": "64 lowercase hex",
      "requiredCapabilities": ["capability names from accepted host"],
      "artifacts": [
        {"os":"windows","arch":"x86_64","executable":"ocg-cpa-host.exe","sha256":"64 lowercase hex","verification":"host-suite"}
      ]
    }`;
    rejected('schema-example-is-not-accepted-data', decide(example, sampleManifest(), hex.file), '64 lowercase hex');
  });
  run('self-consistent-manifest-hash-is-not-trust', () => {
    const decision = decide(sampleLock(), sampleManifest({ executableSHA256: hex.other }), hex.other);
    rejected('self-consistent-manifest-hash-is-not-trust', decision, 'trusted lock record');
    expect(decision.problems.some(item => item.includes('executableSHA256')), decision.problems.join('; '));
    expect(decision.problems.some(item => item.includes('executable bytes')), decision.problems.join('; '));
  });
  for (const [field, value] of [
    ['sourceCommit', '0123456789abcdef0123456789abcdef01234567'],
    ['sourceVersion', 'v0.0.0'],
    ['protocolVersion', 2],
    ['buildIdentity', hex.other],
    ['hostSHA256', hex.other],
    ['overlaySHA256', hex.other],
  ]) {
    run(`identity-mismatch-${field}`, () => {
      rejected(`identity-mismatch-${field}`, decide(sampleLock(), sampleManifest({ [field]: value }), hex.file), field);
    });
  }
  run('omitted-buildIdentity-is-not-inferred', () => {
    const lock = sampleLock();
    delete lock.buildIdentity;
    const decision = decide(lock, sampleManifest(), hex.file);
    rejected('omitted-buildIdentity-is-not-inferred', decision, 'buildIdentity');
    expect(decision.trustedSha !== hex.build, 'omitted buildIdentity was filled from the manifest');
  });
  run('platform-mismatch', () => {
    const manifest = sampleManifest({ build: { goos: 'linux', goarch: 'amd64' } });
    const decision = decide(sampleLock(), manifest, hex.file, { os: 'linux', arch: 'x64' });
    rejected('platform-mismatch', decision, 'no host-suite record');
  });
  run('compiled-only-is-not-acceptance', () => {
    const lock = sampleLock();
    lock.artifacts[0].verification = 'compiled-only';
    rejected('compiled-only-is-not-acceptance', decide(lock, sampleManifest(), hex.file), 'no host-suite record');
  });
  run('missing-required-capability', () => {
    const manifest = sampleManifest();
    manifest.capabilities = manifest.capabilities.filter(item => item !== 'policy-ipc-v1');
    rejected('missing-required-capability', decide(sampleLock(), manifest, hex.file), 'policy-ipc-v1');
  });
  run('placeholder-baff-is-not-a-record', () => {
    const lock = sampleLock();
    lock.artifacts[0].sha256 = placeholderSHA256;
    const decision = decide(lock, sampleManifest({ executableSHA256: placeholderSHA256 }), placeholderSHA256);
    rejected('placeholder-baff-is-not-a-record', decision, 'baff0e76');
  });
  run('multiple-host-suite-records', () => {
    const lock = sampleLock();
    lock.artifacts.push({ os: 'win32', arch: 'amd64', variant: 'production', executable: 'ocg-cpa-host.exe', sha256: hex.second, verification: 'host-suite' });
    rejected('multiple-host-suite-records', decide(lock, sampleManifest(), hex.file), 'more than one host-suite');
  });
  run('uppercase-record-sha-rejected', () => {
    const lock = sampleLock();
    lock.artifacts[0].sha256 = hex.file.toUpperCase();
    rejected('uppercase-record-sha-rejected', decide(lock, sampleManifest(), hex.file), 'sha256');
  });
  run('executable-must-be-a-basename', () => {
    for (const executable of ['..\\ocg-cpa-host.exe', 'sub/ocg-cpa-host.exe', '..']) {
      const lock = sampleLock();
      lock.artifacts[0].executable = executable;
      const decision = decide(lock, sampleManifest(), hex.file);
      rejected(executable, decision, 'basename');
      expect(decision.executableName === '', `${executable} was selected as the executable`);
    }
  });
  run('empty-required-capabilities', () => {
    const lock = sampleLock();
    lock.requiredCapabilities = [];
    rejected('empty-required-capabilities', decide(lock, sampleManifest(), hex.file), 'requiredCapabilities');
  });
  run('host-dir-cannot-supply-the-lock', () => {
    const hostDirLock = join(options.hostDir, 'artifact-lock.json');
    expect(artifactLockPath === join(repo, 'runtime', 'cpa', 'artifact-lock.json'), artifactLockPath);
    expect(hostDirLock !== artifactLockPath, 'host dir selects the trusted lock');
    const unusedHostDirLock = sampleLock();
    void unusedHostDirLock;
    rejected('host-dir-cannot-supply-the-lock', decide(null, sampleManifest(), hex.file), 'absent');
  });
  run('well-formed-lock-accepted', () => {
    const decision = decide(sampleLock(), sampleManifest(), hex.file);
    expect(decision.problems.length === 0, decision.problems.join('; '));
    expect(decision.trustedSha === hex.file, `trusted ${decision.trustedSha}`);
    expect(decision.executableName === 'ocg-cpa-host.exe', decision.executableName);
  });
  run('normalized-win32-amd64-record-accepted', () => {
    const lock = sampleLock();
    lock.artifacts[0].os = 'win32';
    lock.artifacts[0].arch = 'amd64';
    const decision = decide(lock, sampleManifest(), hex.file);
    expect(decision.problems.length === 0, decision.problems.join('; '));
    expect(decision.trustedSha === hex.file, `trusted ${decision.trustedSha}`);
  });
  run('compiled-only-sibling-does-not-become-trust', () => {
    const lock = sampleLock();
    lock.artifacts.push({ os: 'windows', arch: 'x86_64', variant: 'native-loopback-fixture', executable: 'other.exe', sha256: hex.other, verification: 'compiled-only' });
    const decision = decide(lock, sampleManifest(), hex.file);
    expect(decision.problems.length === 0, decision.problems.join('; '));
    expect(decision.trustedSha === hex.file, `trusted ${decision.trustedSha}`);
  });
  run('missing-variant-rejected', () => {
    const lock = sampleLock();
    delete lock.artifacts[0].variant;
    rejected('missing-variant-rejected', decide(lock, sampleManifest(), hex.file), 'variant is absent or unknown');
  });
  run('unknown-variant-rejected', () => {
    const lock = sampleLock();
    lock.artifacts[0].variant = 'candidate';
    rejected('unknown-variant-rejected', decide(lock, sampleManifest(), hex.file), 'variant is absent or unknown');
  });
  run('duplicate-platform-variant-rejected', () => {
    const lock = sampleLock();
    lock.artifacts.push({ os: 'windows', arch: 'x86_64', variant: 'production', executable: 'other.exe', sha256: hex.other, verification: 'compiled-only' });
    rejected('duplicate-platform-variant-rejected', decide(lock, sampleManifest(), hex.file), 'duplicate os/arch/variant windows/x86_64/production');
  });
  run('fixture-non-windows-rejected', () => {
    const lock = sampleLock();
    lock.artifacts.push({ os: 'linux', arch: 'x86_64', variant: 'native-loopback-fixture', executable: 'ocg-cpa-host-linux-amd64', sha256: hex.second, verification: 'compiled-only' });
    rejected('fixture-non-windows-rejected', decide(lock, sampleManifest(), hex.file), 'fixture record is not the Windows host');
  });
  run('feature-off-selects-production', () => {
    const decision = decide(pairedLock(), sampleManifest(), hex.file);
    expect(decision.problems.length === 0, decision.problems.join('; '));
    expect(decision.trustedSha === hex.file, `production trusted ${decision.trustedSha}`);
    expect(decision.variant === 'production', decision.variant);
  });
  run('feature-on-selects-fixture', () => {
    const decision = decide(pairedLock(), fixtureManifest(), hex.second, { os: 'win32', arch: 'x64' }, 'native-loopback-fixture');
    expect(decision.problems.length === 0, decision.problems.join('; '));
    expect(decision.trustedSha === hex.second, `fixture trusted ${decision.trustedSha}`);
  });
  run('production-bytes-do-not-satisfy-fixture', () => {
    rejected('production-bytes-do-not-satisfy-fixture', decide(pairedLock(), fixtureManifest(), hex.file, { os: 'win32', arch: 'x64' }, 'native-loopback-fixture'), 'executable bytes');
  });
  run('fixture-bytes-do-not-satisfy-production', () => {
    rejected('fixture-bytes-do-not-satisfy-production', decide(pairedLock(), sampleManifest({ executableSHA256: hex.second }), hex.second), 'executable bytes');
  });
  run('manifest-variant-cannot-select-trust', () => {
    const manifest = fixtureManifest();
    const decision = decide(pairedLock(), manifest, hex.second);
    rejected('manifest-variant-cannot-select-trust', decision, 'selected compile mode');
    expect(decision.trustedSha === '', `manifest variant selected trust ${decision.trustedSha}`);
  });
  run('manifest-build-tags-must-match-variant', () => {
    rejected('manifest-build-tags-must-match-variant', decide(sampleLock(), sampleManifest({ buildTags: ['ocg_native_loopback_fixture'] }), hex.file), 'buildTags');
    rejected('fixture-tags', decide(pairedLock(), fixtureManifest({ buildTags: [] }), hex.second, { os: 'win32', arch: 'x64' }, 'native-loopback-fixture'), 'buildTags');
  });
  run('same-frozen-identity-for-both-variants', () => {
    const drifted = fixtureManifest({ sourceCommit: '0123456789abcdef0123456789abcdef01234567' });
    rejected('same-frozen-identity-for-both-variants', decide(pairedLock(), drifted, hex.second, { os: 'win32', arch: 'x64' }, 'native-loopback-fixture'), 'sourceCommit');
    const production = decide(pairedLock(), sampleManifest(), hex.file);
    const fixture = decide(pairedLock(), fixtureManifest(), hex.second, { os: 'win32', arch: 'x64' }, 'native-loopback-fixture');
    expect(production.problems.length === 0 && fixture.problems.length === 0, `${production.problems.join('; ')} ${fixture.problems.join('; ')}`);
    expect(production.trustedSha !== fixture.trustedSha, 'the two variants resolved to one hash');
  });
  run('manifest-acceptance-flag-is-not-trust', () => {
    const manifest = sampleManifest({ executableSHA256: hex.other, fullCLIAccepted: true });
    const decision = decide(sampleLock(), manifest, hex.other);
    rejected('manifest-acceptance-flag-is-not-trust', decision, 'trusted lock record');
    expect(decision.trustedSha === '', 'fullCLIAccepted authorized a manifest hash');
  });
  run('production-and-fixture-bytes-must-differ', () => {
    const lock = pairedLock();
    lock.artifacts.find(item => item.variant === 'native-loopback-fixture').sha256 = hex.file;
    rejected('production-and-fixture-bytes-must-differ', decide(lock, sampleManifest(), hex.file), 'not distinct');
  });
  let liveProblems = [];
  if (!before.present) {
    try { liveProblems = await prerequisiteReport(); }
    catch (error) { liveProblems = [`prerequisiteReport threw: ${error.message}`]; }
  }
  const after = await lockPresence();
  if (!before.present) {
    run('absent-real-lock-is-not-trusted', () => {
      expect(liveProblems.some(item => item.includes('trusted artifact lock is absent')), liveProblems.join('; '));
      expect(trustedSha === '', `missing lock produced trustedSha ${trustedSha}`);
      expect(artifactLockStatus.accepted === false, 'missing lock was accepted');
    });
  }
  run('fixture-did-not-create-lock', () => {
    expect(before.present === after.present, 'fixture changed trusted lock presence');
    expect(!(!before.present && after.present), 'fixture created runtime/cpa/artifact-lock.json');
    if (before.present && after.present) {
      expect(before.size === after.size && before.mtimeMs === after.mtimeMs, 'fixture modified the trusted lock');
    }
  });
  run('no-product-process', () => {
    expect(ownedPids.size === 0 && world.serve === undefined && world.upstream === undefined, 'fixture started a product process');
  });
  const failed = cases.some(item => item.status === 'FAIL');
  const payload = {
    status: failed ? 'fixture-fail' : 'fixture-pass',
    closeout: false,
    wholeCli: 'incomplete',
    productRuntimeStarted: false,
    at: new Date().toISOString(),
    realLockPath: artifactLockPath,
    realLockPresent: after.present,
    realLockCreatedByFixture: !before.present && after.present,
    note: 'Synthetic lock fixtures are not CLI acceptance. The real trusted lock is primary-owned and was not written by this harness.',
    cases,
  };
  await mkdir(workRoot, { recursive: true });
  await writeFile(join(workRoot, 'lock-fixture-receipt.json'), `${JSON.stringify(payload, null, 2)}\n`);
  const failedNames = cases.filter(item => item.status === 'FAIL').map(item => item.name);
  console.log(`${payload.status}; closeout false; whole CLI incomplete; real lock present ${after.present}`);
  if (failedNames.length) console.log(`failed ${failedNames.join(', ')}`);
  return failed ? 1 : 0;
}
function nativeSourceSection(source, startMark, endMark) {
  const start = source.indexOf(startMark);
  const end = source.indexOf(endMark, start + startMark.length);
  if (start < 0 || end < 0) throw new Error(`missing section ${startMark}`);
  return source.slice(start, end);
}
function nativeLockFixtureNames(source) {
  const names = [...source.matchAll(/run\('([^']+)'/g)].map(match => match[1]);
  const fields = ['sourceCommit', 'sourceVersion', 'protocolVersion', 'buildIdentity', 'hostSHA256', 'overlaySHA256'];
  if (!source.includes('identity-mismatch-${field}')) throw new Error('identity mismatch cases are not generated from the lock fields');
  for (const field of fields) {
    if (!source.includes(`['${field}'`)) throw new Error(`lock fixture field ${field} is absent`);
    names.push(`identity-mismatch-${field}`);
  }
  return names;
}
async function runNativeFixtures() {
  const cases = [];
  const run = async (name, fn) => {
    try {
      await fn();
      cases.push({ name, status: 'PASS' });
    } catch (error) {
      cases.push({ name, status: 'FAIL', detail: String(error.message ?? error) });
    }
  };
  const expect = (condition, message) => { if (!condition) throw new Error(message); };
  const before = await lockPresence();
  const inheritedSeam = process.env.OCG_CPA_TEST_ENDPOINTS;
  const scriptSource = await readFile(fileURLToPath(import.meta.url), 'utf8');
  await run('hook-is-pending', async () => {
    expect(NATIVE_HOOK.implemented === undefined, 'native hook was marked implemented');
    expect(Object.prototype.hasOwnProperty.call(NATIVE_HOOK, 'policyReady') === false, 'hook object fabricates policyReady');
    expect(NATIVE_HOOK.feature === 'ollama-cloud-loopback-test', NATIVE_HOOK.feature);
    expect(NATIVE_HOOK.env === 'OCG_CPA_TEST_ENDPOINTS', NATIVE_HOOK.env);
    expect(NATIVE_HOOK.goTag === 'ocg_native_loopback_fixture', NATIVE_HOOK.goTag);
    const seam = nativeSeamJson(9);
    expect(seam.startsWith('{') && seam.includes('http://127.0.0.1:9'), 'seam JSON was not the pure loopback map');
    expect(world.trustedSeamJson === '', 'seam JSON was assigned');
    expect(process.env.OCG_CPA_TEST_ENDPOINTS === inheritedSeam, 'seam JSON was installed into the process environment');
    const body = nativeSeamBody(9);
    expect(Object.keys(body).join(',') === NATIVE_HOOK.keys.join(','), `seam keys were ${Object.keys(body).join(',')}`);
    expect(NATIVE_HOOK.existingKeys.every(key => body[key] === undefined), 'native seam includes a Go or Command Code key');
    expect(body.loopback === undefined && body['*'] === undefined && body.cpa === undefined, 'native seam has a blanket authority key');
    expect(Object.values(body).every(value => value === 'http://127.0.0.1:9'), 'seam origin was not the explicit loopback');
    expect(nativeAdmission().includes('pending'), 'admission did not stay pending without a trusted lock');
    expect(ownedPids.size === 0 && world.nativeLoopback === undefined, 'admission started a listener');
  });
  await run('metadata-loopback-is-not-authority', async () => {
    expect(NATIVE_METADATA_LOOPBACK_IS_AUTHORITY === false, 'metadata loopback is authority');
  });
  await run('matrix-matches-source-urls', async () => {
    const expected = {
      C1: 'https://chatgpt.com/backend-api/codex/responses',
      C2: 'https://chatgpt.com/backend-api/codex/responses/compact',
      A1: 'https://api.anthropic.com/v1/messages?beta=true',
      A2: 'https://api.anthropic.com/v1/messages/count_tokens?beta=true',
      Kc1: 'https://api.kimi.com/coding/v1/chat/completions',
      Kc2: 'https://api.kimi.com/coding/v1/responses',
      Kc3: 'https://api.kimi.com/coding/v1/messages?beta=true',
      Kc4: 'https://api.kimi.com/coding/v1/messages/count_tokens?beta=true',
      Ka1: 'https://api.kimi.ai/coding/v1/chat/completions',
      Ka2: 'https://api.kimi.ai/coding/v1/responses',
      Ka3: 'https://api.kimi.ai/coding/v1/messages?beta=true',
      Ka4: 'https://api.kimi.ai/coding/v1/messages/count_tokens?beta=true',
      X1: 'https://cli-chat-proxy.grok.com/v1/responses',
      X2: 'https://api.x.ai/v1/responses',
      X3: 'https://api.x.ai/v1/responses/compact',
      G1: 'https://daily-cloudcode-pa.googleapis.com/v1internal:generateContent',
      G2: 'https://daily-cloudcode-pa.googleapis.com/v1internal:streamGenerateContent?alt=sse',
      G3: 'https://daily-cloudcode-pa.googleapis.com/v1internal:countTokens',
    };
    for (const [id, url] of Object.entries(expected)) {
      expect(NATIVE_MATRIX[id]?.url === url, `${id} URL drifted`);
      const parsed = new URL(url);
      expect(parsed.port === '', `${id} baked in a default port`);
      expect(parsed.username === '' && parsed.hash === '', `${id} has userinfo or a fragment`);
      expect(NATIVE_OFFICIAL_HOSTS.includes(parsed.hostname), `${id} host is outside the official set`);
      expect(NATIVE_MATRIX[id].path === parsed.pathname, `${id} path drifted`);
      expect(NATIVE_MATRIX[id].query === (parsed.search.startsWith('?') ? parsed.search.slice(1) : ''), `${id} query drifted`);
    }
    for (const id of ['A1', 'A2', 'Kc3', 'Kc4', 'Ka3', 'Ka4']) expect(NATIVE_MATRIX[id].query === 'beta=true', `${id} beta query drifted`);
    expect(NATIVE_MATRIX.G2.query === 'alt=sse', 'Antigravity stream query drifted');
    expect(NATIVE_MATRIX.C1.query === '' && NATIVE_MATRIX.Kc1.query === '', 'a no-query matrix URL gained a query');
    expect(NATIVE_EXPECTED_BASES['xai.cli'] === `${NATIVE_MATRIX.X1.origin}/v1`, 'XAI CLI default base drifted');
    expect(NATIVE_EXPECTED_BASES['kimi.com'] === 'https://api.kimi.com/coding', 'Kimi Code default base drifted');
    expect(NATIVE_EXPECTED_BASES['kimi.ai'] === 'https://api.kimi.ai/coding', 'Kimi .ai base drifted');
    expect(NATIVE_MATRIX.Kc1.path === '/coding/v1/chat/completions', 'Kimi Code v1 path drifted');
  });
  await run('catalog-follows-source-rows', async () => {
    const catalog = nativeCatalog();
    const find = (family, ingress, operation, model = nativeScenarioModel(family)) => {
      const cell = nativeFindCell(catalog, family, ingress, operation, model);
      if (!cell) throw new Error(`missing ${family} ${ingress} ${operation} ${model}`);
      return cell;
    };
    const live = catalog.filter(cell => cell.liveSend);
    expect(catalog.length === 137, `catalog cells were ${catalog.length}`);
    expect(live.length > 1, `runnable client sends collapsed to ${live.length}`);
    const codexExecute = live.find(cell => cell.family === 'codex' && cell.ingress === 'chat_completions' && cell.operation === 'execute');
    expect(codexExecute && codexExecute.target === 'C1' && codexExecute.method === 'POST' && codexExecute.query === '' && codexExecute.disposition === 'cli-import-generation', 'Codex chat execute drifted');
    expect(live.some(cell => cell.operation === 'internal') === false, 'internal was given a client send');
    expect(live.filter(cell => cell.operation === 'compact').every(cell => cell.family === 'antigravity' && cell.ingress === 'responses' && cell.protocol === 'responses'), 'a non-Antigravity compact cell was given a client send');
    expect(live.some(cell => cell.family === 'antigravity' && cell.ingress === 'responses' && cell.operation === 'compact' && cell.liveSend === true), 'Antigravity Responses compaction_trigger has no client send');
    expect(catalog.every(cell => !['inactive-until-proven', 'inactive', 'declared-blocked-hook', 'live-positive', 'not-claimed'].includes(cell.disposition)), 'a cell kept a declared-only disposition');
    expect(catalog.filter(cell => cell.outcome === 'network').every(cell => cell.method === 'POST'), 'a network cell is not POST');
    expect(find('codex', 'responses', 'execute').target === 'C1', 'Codex responses was unioned to another URL');
    expect(find('codex', 'messages', 'internal').target === 'C1', 'Codex internal drifted');
    expect(find('codex', 'chat_completions', 'count-tokens').target === 'LOCAL', 'Codex count is not local');
    expect(find('codex', 'chat_completions', 'compact').target === 'C2', 'Codex compact drifted');
    expect(find('codex', 'gemini-countTokens', 'count-tokens').target === 'LOCAL' && find('codex', 'gemini-countTokens', 'count-tokens').protocol === 'chat_completions', 'public Gemini Codex count drifted');
    expect(find('anthropic', 'messages', 'count-tokens').target === 'A2' && find('anthropic', 'messages', 'count-tokens').query === 'beta=true', 'Anthropic count drifted');
    expect(find('anthropic', 'chat_completions', 'compact').outcome === 'refuse', 'Anthropic compact was fabricated');
    expect(find('anthropic', 'gemini-generateContent', 'execute').target === 'A1', 'public Gemini Anthropic execute drifted');
    expect(find('kimi.com', 'chat_completions', 'execute').target === 'Kc1', 'Kimi chat drifted');
    expect(find('kimi.com', 'responses', 'stream').target === 'Kc2', 'Kimi responses drifted');
    expect(find('kimi.com', 'messages', 'internal').target === 'Kc3' && find('kimi.com', 'messages', 'internal').query === 'beta=true', 'Kimi messages drifted');
    expect(NATIVE_CALLABLES.every(protocol => find('kimi.com', protocol, 'count-tokens').target === 'Kc4'), 'Kimi count was unioned');
    expect(find('kimi.com', 'chat_completions', 'compact').outcome === 'refuse', 'Kimi compact was fabricated');
    expect(find('kimi.com', 'gemini-countTokens', 'count-tokens').target === 'Kc4', 'public Gemini Kimi count drifted');
    expect(find('kimi.ai', 'chat_completions', 'execute').target === 'Ka1' && find('kimi.ai', 'chat_completions', 'execute').disposition === 'staged-generation', 'Kimi .ai was not staged generation');
    expect(find('xai.cli', 'responses', 'execute').target === 'X1' && find('xai.cli', 'responses', 'execute').prefixGap === false, 'XAI CLI execute drifted');
    expect(find('xai.cli', 'messages', 'count-tokens').target === 'LOCAL', 'XAI count is not local');
    expect(find('xai.cli', 'chat_completions', 'compact').target === 'X3' && find('xai.cli', 'chat_completions', 'compact').prefixGap === true, 'XAI CLI compact prefix gap was widened');
    expect(find('xai.api', 'chat_completions', 'execute').target === 'X2' && find('xai.api', 'chat_completions', 'execute').outcome === 'network' && find('xai.api', 'chat_completions', 'execute').disposition === 'staged-generation', 'XAI API was not staged generation');
    expect(find('antigravity', 'chat_completions', 'execute').target === 'G1', 'Antigravity ordinary execute drifted');
    expect(find('antigravity', 'chat_completions', 'stream').target === 'G2' && find('antigravity', 'chat_completions', 'stream').query === 'alt=sse', 'Antigravity stream drifted');
    expect(find('antigravity', 'chat_completions', 'count-tokens').target === 'G3', 'Antigravity count drifted');
    expect(find('antigravity', 'chat_completions', 'compact').target === 'G1', 'Antigravity compact drifted');
    const streamedModel = nativeScenarioModel('antigravity', 'streamed');
    expect(find('codex', 'chat_completions', 'execute').model === 'gpt-5.5', 'Codex scenario model is not the Codex registry model');
    expect(find('anthropic', 'messages', 'execute').model.startsWith('claude-'), 'Anthropic scenario model is not a Claude registry model');
    expect(find('kimi.com', 'chat_completions', 'execute').model === 'kimi-k2', 'Kimi scenario model drifted');
    expect(find('xai.api', 'responses', 'execute').model === 'grok-4.7', 'XAI scenario model drifted');
    expect(find('antigravity', 'chat_completions', 'execute', streamedModel).target === 'G2', 'streamed Antigravity execute stayed on G1');
    expect(find('antigravity', 'responses', 'internal', streamedModel).target === 'G2', 'streamed Antigravity internal drifted');
    expect(find('antigravity', 'gemini-generateContent', 'execute', streamedModel).target === 'G2', 'streamed public Gemini stayed on G1');
    expect(find('antigravity', 'gemini-generateContent', 'execute').target === 'G1' && find('antigravity', 'gemini-generateContent', 'execute').protocol === 'chat_completions', 'public Gemini became a fourth protocol');
    const alt = catalog.find(cell => cell.ingress === 'source-alt');
    expect(alt && alt.outcome === 'refuse' && alt.disposition === 'source-refusal' && alt.query === 'alt=other' && alt.url === null && alt.liveSend === false, 'unknown Antigravity alt was given a target');
    expect(catalog.every(cell => cell.family === 'antigravity' ? cell.importClass === 'unsupported-local-file' : true), 'Antigravity import was marked supported');
    expect(live.some(cell => cell.family === 'kimi.ai' && cell.disposition === 'staged-generation'), 'staged Kimi .ai has no client send');
    expect(live.some(cell => cell.family === 'antigravity' && cell.disposition === 'staged-generation'), 'staged Antigravity has no client send');
    expect(live.some(cell => cell.outcome === 'local'), 'local count has no client request');
  });
  await run('model-branch-matches-source', async () => {
    expect(streamedNonstream('Claude-sonnet') === true, 'claude model branch drifted');
    expect(streamedNonstream('gemini-3-pro-preview') === true, 'gemini-3-pro branch drifted');
    expect(streamedNonstream('Gemini-3-pro') === false, 'gemini-3-pro match became case-insensitive');
    expect(streamedNonstream('gemini-3.1-flash-image') === true, 'gemini-3.1-flash-image branch drifted');
    expect(streamedNonstream('gemini-2.5-pro') === false, 'ordinary Gemini predicate drifted');
    expect(streamedNonstream(nativeScenarioModel('antigravity')) === false, 'current ordinary Antigravity model entered the streamed branch');
    expect(streamedNonstream(nativeScenarioModel('antigravity', 'streamed')) === true, 'current streamed Antigravity model left the predicate');
  });
  await run('import-shapes-match-source', async () => {
    const documents = nativeSourceDocuments();
    expect(Object.keys(documents).join(',') === 'codex,claude,kimi,xai', 'an unsupported provider file was created');
    const encoded = JSON.stringify(documents);
    expect(!encoded.includes('using_api') && !encoded.includes('base_url') && !encoded.includes('"domain"'), 'fixture wrote using_api, base_url, or a Kimi domain');
    expect(documents.codex.auth_mode === 'chatgpt', 'Codex auth mode drifted');
    expect(documents.codex.tokens.account_id === 'synthetic-codex-account', 'Codex account id drifted');
    const codexAccess = JSON.parse(Buffer.from(documents.codex.tokens.access_token.split('.')[1], 'base64url').toString('utf8'));
    const codexId = JSON.parse(Buffer.from(documents.codex.tokens.id_token.split('.')[1], 'base64url').toString('utf8'));
    expect(codexAccess.synthetic === true && codexAccess.exp === 1893456000, 'Codex access token is not the synthetic JWT');
    expect(codexId.synthetic === true && codexId['https://api.openai.com/auth'].chatgpt_account_id === 'synthetic-codex-account', 'Codex id token claim drifted');
    expect(documents.claude.claudeAiOauth.scopes.includes('user:inference'), 'Claude inference scope drifted');
    expect(documents.claude.claudeAiOauth.expiresAt === 1893456000000, 'Claude expiry drifted');
    expect(documents.kimi.token_type === 'bearer' && documents.kimi.expires_at === 1893456000, 'Kimi token wire drifted');
    const xai = documents.xai['https://auth.x.ai::b1a00492-073a-47ea-816f-4c329264a828'];
    expect(xai && xai.auth_mode === 'oidc' && xai.oidc_issuer === 'https://auth.x.ai', 'Grok auth entry drifted');
    expect(xai.oidc_client_id === 'b1a00492-073a-47ea-816f-4c329264a828', 'Grok client id drifted');
    expect(xai.key.startsWith('synthetic-') && xai.refresh_token.startsWith('synthetic-'), 'Grok token is not synthetic');
    expect(xai.using_api === undefined, 'Grok fixture set using_api');
    const root = join(nativeWorkRoot, 'layout-check');
    const layout = nativeSourceLayout(root);
    const expectedSuffix = {
      codex: join('codex', 'auth.json'),
      claude: join('claude', '.credentials.json'),
      kimi: join('kimi', 'credentials', 'kimi-code.json'),
      xai: join('grok', 'auth.json'),
    };
    for (const [key, file] of Object.entries(layout)) {
      expect(file === join(root, expectedSuffix[key]), `${key} path drifted`);
      expect(within(root, file), `${key} path left the synthetic root`);
      expect(!file.split(sep).includes('..'), `${key} path traverses`);
    }
    expect(NATIVE_DISCOVERY.find(item => item.provider === 'antigravity').supported === false, 'Antigravity discovery was marked supported');
    expect(NATIVE_DISCOVERY.find(item => item.provider === 'antigravity').reason.includes('not compatible with this import'), 'Antigravity reason drifted');
    expect(NATIVE_IMPORT_PROVIDERS.includes('antigravity') === false, 'Antigravity file import is in the POST list');
    expect(nativeImportName('codex', 'synthetic-codex-account').startsWith('ocg-cli-codex-'), 'Codex import name drifted');
  });
  await run('plan-covers-catalog', async () => {
    const catalog = nativeCatalog();
    const plan = nativeScenarioPlan();
    const coverage = nativePlanCoverage(plan);
    expect(coverage.catalogCells === 137, `catalog cells were ${coverage.catalogCells}`);
    expect(coverage.runnable > 1, `runnable scenarios were ${coverage.runnable}`);
    expect(coverage.sourceRefusal > 0, 'source refusals were dropped');
    expect(coverage.pending > 0, 'pending interfaces were closed out');
    const covered = new Map();
    for (const item of plan) {
      if (!item.cell) continue;
      const id = nativeCellId(item.cell);
      covered.set(id, (covered.get(id) ?? 0) + 1);
    }
    expect(catalog.every(cell => covered.get(nativeCellId(cell)) === 1), 'a catalog cell is missing or repeated in the plan');
    for (const [name] of NATIVE_PENDING_INTERFACES) expect(coverage.interfaces.includes(name), `${name} was closed out`);
    for (const name of NATIVE_ZERO_SENDS) expect(NATIVE_ZERO_SENDS.includes(name) && scriptSource.includes(name), `${name} is not in the driver`);
    for (const name of NATIVE_ONE_HITS) expect(scriptSource.includes(`'${name}'`) || scriptSource.includes(name), `${name} is not in the driver`);
    expect(new Set(NATIVE_ZERO_SENDS).size === NATIVE_ZERO_SENDS.length, 'zero-send classes repeat');
    const kimi = plan.find(item => item.cell?.family === 'kimi.ai' && item.cell?.operation === 'execute' && item.cell?.ingress === 'chat_completions');
    expect(kimi?.kind === 'network', 'Kimi .ai execute is not a network scenario');
    const compact = plan.find(item => item.cell?.family === 'codex' && item.cell?.operation === 'compact');
    expect(compact?.kind === 'pending' && compact.interface === 'source-compact', 'Codex compact was given a client send');
    const agCompact = plan.find(item => item.cell?.family === 'antigravity' && item.cell?.ingress === 'responses' && item.cell?.operation === 'compact' && item.cell?.model === nativeScenarioModel('antigravity'));
    expect(agCompact?.kind === 'network' && agCompact.cell.liveSend === true, 'Antigravity Responses compaction_trigger stayed pending');
    const agCompactReq = nativeClientRequest(agCompact.cell);
    expect(agCompactReq?.path === '/v1/responses' && agCompactReq.parentPermit === 'execute' && agCompactReq.internalChild === false && JSON.stringify(agCompactReq.body.input).includes('compaction_trigger'), 'ordinary Antigravity compaction request drifted');
    const streamedModel = nativeScenarioModel('antigravity', 'streamed');
    const agCompactStream = plan.find(item => item.cell?.family === 'antigravity' && item.cell?.ingress === 'responses' && item.cell?.operation === 'compact' && item.cell?.model === streamedModel);
    expect(agCompactStream?.kind === 'network', 'streamed Antigravity compaction_trigger stayed pending');
    const agCompactStreamReq = nativeClientRequest(agCompactStream.cell);
    expect(agCompactStreamReq?.body.stream === true && agCompactStreamReq.parentPermit === 'stream' && agCompactStreamReq.internalChild === false, 'streamed Antigravity compaction was labeled internal child');
    const codexStream = plan.find(item => item.cell?.family === 'codex' && item.cell?.ingress === 'responses' && item.cell?.operation === 'stream');
    expect(codexStream?.kind === 'network', 'Codex responses stream left the network plan');
    const bootstrap = NATIVE_PENDING_INTERFACES.find(item => item[0] === 'stream-bootstrap');
    expect(bootstrap?.[1].includes('StreamBootstrapBuffering') && bootstrap[1].includes('not a finding'), 'bootstrap enablement evidence drifted');
    expect(coverage.publicCompaction === 2, `public Antigravity compaction cells were ${coverage.publicCompaction}`);
    expect(scriptSource.includes('assertCatalogPlaneRollback') && scriptSource.includes('loopback-ok:plane-a') && scriptSource.includes('loopback-ok:plane-b'), 'catalog plane rollback drifted');
    expect(scriptSource.includes("'source-classification'") && scriptSource.includes("'required-sdk-host'") && scriptSource.includes("'cas-binding'"), 'evidence levels drifted');
    expect(scriptSource.includes('physicalBearerAttribution') && scriptSource.includes('seededArchiveToken') && scriptSource.includes('archiveBearerVerdict'), 'archive seeded bearer pin drifted');
    expect(scriptSource.includes("bearerGate.status === 'FAIL'") && scriptSource.includes('native-oauth-archive-bearer'), 'unambiguous archive bearer mismatch can still PASS');
    expect(NATIVE_SDK_REQUIRED_INTERFACES.includes('internal') && NATIVE_SDK_REQUIRED_INTERFACES.includes('stream-refresh'), 'required SDK interfaces dropped');
    expect(coverage.sdkRequired > 0, 'required SDK pending rows were closed out as operator PASS');
  });
  await run('map-refuses-before-install', async () => {
    const good = nativeSeamJson(9);
    expect(classifyNativeMap(good).ok === true, classifyNativeMap(good).reason);
    expect(classifyNativeMap('{}').ok === true, 'empty map was not sealed');
    expect(classifyNativeMap('{"cpa":"http://127.0.0.1:9"}').ok === false, 'cpa key was accepted');
    expect(classifyNativeMap('{"codex":"http://127.0.0.1:9","nope":"http://127.0.0.1:9"}').ok === false, 'unknown key was accepted');
    expect(classifyNativeMap('{"codex":"http://127.0.0.1:9","codex":"http://127.0.0.1:8"}').ok === false, 'duplicate key was accepted');
    expect(classifyNativeMap('{"codex":"https://chatgpt.com"}').ok === false, 'official origin was accepted');
    expect(classifyNativeMap('{"codex":"http://127.0.0.1:9/path"}').ok === false, 'path origin was accepted');
    expect(classifyNativeMap('{"codex":"http://127.0.0.1"}').ok === false, 'origin without a port was accepted');
    expect(classifyNativeMap('not-json').ok === false, 'malformed JSON was accepted');
    expect(classifyNativeMap('').ok === false, 'empty text was accepted');
    expect(nativeOriginAllowed('http://[::1]:9') === true, 'bracketed IPv6 loopback was refused');
    expect(nativeOriginAllowed('http://localhost:9') === true, 'localhost loopback was refused');
    expect(world.trustedSeamJson === '', 'map classifier installed a seam');
    expect(process.env.OCG_CPA_TEST_ENDPOINTS === inheritedSeam, 'map classifier wrote the process environment');
  });
  await run('live-source-has-no-direct-provider', async () => {
    const live = nativeSourceSection(scriptSource, 'function nativeAdmission()', "stage('native-operator-workflows'");
    expect(!live.includes('https://chatgpt.com'), 'live workflow names an official Codex URL');
    expect(!live.includes('https://api.anthropic.com'), 'live workflow names an official Anthropic URL');
    expect(!live.includes('https://api.kimi.com'), 'live workflow names an official Kimi URL');
    expect(!live.includes('https://api.kimi.ai'), 'live workflow names an official Kimi .ai URL');
    expect(!live.includes('https://api.x.ai'), 'live workflow names an official XAI URL');
    expect(!live.includes('https://cli-chat-proxy.grok.com'), 'live workflow names the XAI CLI host');
    expect(!live.includes('https://daily-cloudcode-pa.googleapis.com'), 'live workflow names the Antigravity host');
    expect(!live.includes('oauth/start'), 'live workflow starts OAuth');
    expect(!live.includes('billing/credits/grants'), 'live workflow uses credit grants as native authority');
    expect(!live.includes("api('PUT', `${prefix}/cpa/models`"), 'live workflow writes the retired CPA catalog');
    expect(live.includes('success: false'), 'live client send can treat a body as success');
    expect(live.includes('allowedEndpointIds') && live.includes('allowedOrigins'), 'live workflow does not patch both grant fields');
    expect(live.includes('data/cpa/auth'), 'live workflow does not use the owned auth directory');
    expect(live.includes('not compatible with this import'), 'live workflow dropped the Antigravity import reason');
    expect(live.includes('models/refresh'), 'live workflow does not reconcile staged auth');
    for (const name of ['wrong-mode', 'wrong-base', 'stale-version', 'revoked-grant', 'native-presence', 'forbidden-protocol', 'map-after-apply', 'confirmed-429', 'unknown-variant', 'loss', 'truncate', 'empty', 'cancel', 'deadline']) {
      expect(live.includes(name), `live workflow dropped ${name}`);
    }
    const listener = nativeSourceSection(scriptSource, 'async function startNativeLoopback', 'async function closeNativeLoopback');
    expect(listener.includes('native-loopback-recorded'), 'loopback response marker drifted');
    expect(listener.includes('502'), 'loopback does not fail the physical response');
    expect(listener.includes("'429'") && listener.includes('429-pair'), 'loopback dropped the confirmed 429 behaviors');
    expect(!listener.includes('policyReady'), 'loopback fabricates policyReady');
  });
  await run('environment-strips-provider-roots', async () => {
    const previous = { root: world.root, nativeRoots: world.nativeRoots, trustedSeamJson: world.trustedSeamJson };
    try {
      world.trustedSeamJson = '';
      world.nativeRoots = undefined;
      world.root = join(nativeWorkRoot, 'fixture-profile');
      const bare = environment();
      for (const name of ['CODEX_HOME', 'CLAUDE_CONFIG_DIR', 'KIMI_CODE_HOME', 'GROK_HOME', 'OCG_CPA_TEST_ENDPOINTS']) {
        if (bare[name] !== undefined) throw new Error(`${name} was inherited`);
      }
      world.nativeRoots = nativeProviderRoots(join(world.root, 'home', 'native'));
      const scoped = environment();
      for (const [key, name] of Object.entries(NATIVE_ROOT_ENV)) {
        if (scoped[name] !== world.nativeRoots[key]) throw new Error(`${name} was not the synthetic root`);
        if (!within(world.root, scoped[name])) throw new Error(`${name} left the fixture profile`);
      }
      if (scoped.OCG_CPA_TEST_ENDPOINTS !== undefined) throw new Error('synthetic roots set the test endpoint env');
      if (world.trustedSeamJson !== '') throw new Error('environment assigned seam JSON');
    } finally {
      world.root = previous.root;
      world.nativeRoots = previous.nativeRoots;
      world.trustedSeamJson = previous.trustedSeamJson;
    }
  });
  await run('invoke-refuses-product-process', async () => {
    let message = '';
    try { await invoke(['--version']); } catch (error) { message = String(error.message ?? error); }
    expect(message.includes('native fixture tried to start a product process'), 'invoke did not refuse');
    expect(ownedPids.size === 0 && world.serve === undefined && world.nativeLoopback === undefined, 'invoke started a product process');
  });
  await run('lock-fixture-names-retained', async () => {
    const lockSource = nativeSourceSection(scriptSource, 'async function runLockFixtures', 'async function runNativeFixtures');
    const names = nativeLockFixtureNames(lockSource);
    const expected = [...NATIVE_LOCK_FIXTURE_NAMES];
    const missing = expected.filter(name => !names.includes(name));
    const extra = names.filter(name => !expected.includes(name));
    expect(missing.length === 0 && extra.length === 0 && names.length === NATIVE_LOCK_FIXTURE_NAMES.length && names.length === 40, `lock cases missing ${missing.join(', ')}; extra ${extra.join(', ')}; count ${names.length}`);
  });
  const after = await lockPresence();
  await run('fixture-did-not-create-lock', async () => {
    expect(before.present === after.present, 'fixture changed trusted lock presence');
    expect(!(!before.present && after.present), 'fixture created runtime/cpa/artifact-lock.json');
    if (before.present && after.present) expect(before.size === after.size && before.mtimeMs === after.mtimeMs, 'fixture modified the trusted lock');
  });
  await run('no-product-process', async () => {
    expect(ownedPids.size === 0 && world.serve === undefined && world.upstream === undefined && world.nativeLoopback === undefined, 'fixture started a product process');
  });
  await run('process-env-unchanged', async () => {
    expect(process.env.OCG_CPA_TEST_ENDPOINTS === inheritedSeam, 'fixture changed OCG_CPA_TEST_ENDPOINTS');
  });
  await run('verified-corrections', async () => {
    const catalog = nativeCatalog();
    const find = (family, ingress, operation, model = nativeScenarioModel(family)) => {
      const cell = nativeFindCell(catalog, family, ingress, operation, model);
      if (!cell) throw new Error(`missing ${family} ${ingress} ${operation} ${model}`);
      return cell;
    };
    const ordinaryModel = nativeScenarioModel('antigravity');
    const streamedModel = nativeScenarioModel('antigravity', 'streamed');
    expect(nativeScenarioModel('codex') === 'gpt-5.5' && nativeScenarioModel('kimi.com') === 'kimi-k2' && nativeScenarioModel('xai.cli') === 'grok-4.7', 'family registry model drifted');
    expect(nativeRegistryModels('codex').includes('gemini-2.5-pro') === false && nativeRegistryModels('anthropic').includes('gemini-2.5-pro') === false, 'a non-Antigravity registry still requires Gemini');
    expect(nativeRegistryModels('antigravity').includes('gemini-2.5-pro') === false && nativeRegistryModels('antigravity').includes('gemini-3-pro-preview') === false, 'Antigravity still requires an absent planned name');
    expect(nativeRegistryModels('antigravity').includes('gemini-3.1-flash-image') && streamedNonstream('gemini-3.1-flash-image') === true, 'current streamed predicate model left the registry');
    expect(streamedNonstream(ordinaryModel) === false && streamedNonstream(streamedModel) === true && ordinaryModel !== streamedModel, 'Antigravity branches collapsed');
    const messagesCount = nativeClientRequest(find('anthropic', 'messages', 'count-tokens'));
    expect(messagesCount?.path === '/v1/messages/count_tokens' && messagesCount.body.model === nativeScenarioModel('anthropic') && Array.isArray(messagesCount.body.messages), 'messages count left the mounted count endpoint');
    const geminiCount = nativeClientRequest(find('codex', 'gemini-countTokens', 'count-tokens'));
    expect(geminiCount?.path === nativeGeminiPath(nativeScenarioModel('codex'), 'countTokens'), 'Gemini countTokens ingress left the Gemini path');
    const agChatCount = nativeClientRequest(find('antigravity', 'chat_completions', 'count-tokens'));
    const agMessagesCount = nativeClientRequest(find('antigravity', 'messages', 'count-tokens'));
    expect(agChatCount?.path === nativeGeminiPath(ordinaryModel, 'countTokens'), 'Antigravity chat count left the public Gemini count route');
    expect(agMessagesCount?.path === '/v1/messages/count_tokens', 'Antigravity messages count left the mounted count endpoint');
    const localChat = find('codex', 'chat_completions', 'count-tokens');
    const localResponses = find('xai.cli', 'responses', 'count-tokens');
    const localChatRequest = nativeClientRequest(localChat);
    const localResponsesRequest = nativeClientRequest(localResponses);
    expect(localChat.outcome === 'local' && localChat.target === 'LOCAL' && localChatRequest?.path === '/v1/messages/count_tokens' && localChatRequest.body.model === 'gpt-5.5', 'Codex chat count collapsed onto the Gemini estimator');
    expect(localResponses.outcome === 'local' && localResponsesRequest?.path === '/v1/messages/count_tokens' && !String(localResponsesRequest.path).includes('countTokens'), 'XAI responses count collapsed onto the Gemini estimator');
    const ordinary = find('antigravity', 'chat_completions', 'execute');
    const streamed = find('antigravity', 'chat_completions', 'execute', streamedModel);
    expect(ordinary.target === 'G1' && ordinary.model === ordinaryModel, 'ordinary Antigravity execute left G1');
    expect(streamed.target === 'G2' && streamed.model === streamedModel && streamed.model !== ordinary.model, 'streamed Antigravity execute collapsed onto the ordinary model');
    expect(nativeClientRequest(ordinary)?.body.model === ordinaryModel, 'ordinary client body substituted another model');
    expect(nativeClientRequest(streamed)?.body.model === streamedModel, 'streamed client body substituted the ordinary model');
    const codexWire = nativeUpstreamWire({ family: 'codex', operation: 'execute', protocol: 'responses', path: '/backend-api/codex/responses' });
    expect(codexWire?.type === 'text/event-stream' && codexWire.body.includes('response.completed') && codexWire.body.includes('"text":"yes"'), 'Codex responses wire is not the terminal Responses fixture');
    const xaiWire = nativeUpstreamWire({ family: 'xai.api', operation: 'execute', protocol: 'responses', path: '/v1/responses' });
    expect(xaiWire?.type === 'text/event-stream' && xaiWire.body === NATIVE_WIRE.codexSse && xaiWire.body.includes('response.completed'), 'XAI execute wire is not the terminal Responses fixture');
    const terminalFrames = sseDataObjects(NATIVE_WIRE.codexSse);
    const terminal = terminalFrames.find(item => item?.type === 'response.completed')?.response;
    expect(terminal?.object === 'response' && terminal?.status === 'completed' && typeof terminal?.id === 'string' && terminal.id.length > 0 && typeof terminal?.model === 'string' && terminal.model.length > 0 && terminal?.usage?.input_tokens === 2 && terminal?.usage?.output_tokens === 1, 'codex terminal response metadata is incomplete');
    const doneItem = terminalFrames.find(item => item?.type === 'response.output_item.done')?.item;
    expect(JSON.stringify(terminal?.output) === JSON.stringify(doneItem ? [doneItem] : []), 'codex terminal output does not match the done item');
    assertClientPayload({ code: 0, value: terminal }, { protocol: 'responses', operation: 'execute', ingress: 'responses' }, 'codex terminal JSON');
    assertClientPayload({ code: 0, value: NATIVE_WIRE.codexSse }, { protocol: 'responses', operation: 'stream', ingress: 'responses' }, 'codex terminal SSE');
    assertClientPayload({ code: 0, value: xaiWire.body }, { protocol: 'responses', operation: 'stream', ingress: 'responses' }, 'xai terminal SSE');
    const kimiWire = nativeUpstreamWire({ family: 'kimi.com', operation: 'execute', protocol: 'responses', path: '/coding/v1/responses' });
    expect(kimiWire?.type === 'application/json' && kimiWire.body.includes('"object":"response"') && kimiWire.body.includes('"status":"completed"') && kimiWire.body.includes('hello world'), 'Kimi execute wire is not complete Responses JSON');
    expect(!kimiWire.body.includes('response.completed'), 'Kimi nonstream responses wire is Codex SSE');
    const kimiStreamWire = nativeUpstreamWire({ family: 'kimi.ai', operation: 'stream', protocol: 'responses', path: '/coding/v1/responses' });
    expect(kimiStreamWire?.type === 'text/event-stream' && kimiStreamWire.body.includes('hello stream') && kimiStreamWire.body.includes('response.completed'), 'Kimi responses stream wire drifted');
    expect(nativeUpstreamWire({ path: '/coding/v1/responses' }) === null, 'responses wire without a family became Codex SSE');
    assertClientPayload({ code: 0, value: JSON.parse(NATIVE_WIRE.kimiResponsesJson) }, { protocol: 'responses', operation: 'execute', ingress: 'responses' }, 'responses JSON sample');
    assertClientPayload({ code: 0, value: NATIVE_WIRE.kimiResponsesSse }, { protocol: 'responses', operation: 'stream', ingress: 'responses' }, 'responses SSE sample');
    assertClientPayload({ code: 0, value: JSON.parse(NATIVE_WIRE.chatJson) }, { protocol: 'chat_completions', operation: 'execute', ingress: 'chat_completions' }, 'chat JSON sample');
    assertClientPayload({ code: 0, value: NATIVE_WIRE.chatSse }, { protocol: 'chat_completions', operation: 'stream', ingress: 'chat_completions' }, 'chat SSE sample');
    assertClientPayload({ code: 0, value: JSON.parse(NATIVE_WIRE.messagesJson) }, { protocol: 'messages', operation: 'execute', ingress: 'messages' }, 'messages JSON sample');
    assertClientPayload({ code: 0, value: NATIVE_WIRE.messagesSse }, { protocol: 'messages', operation: 'stream', ingress: 'messages' }, 'messages SSE sample');
    assertClientPayload({ code: 0, value: JSON.parse(NATIVE_WIRE.antigravityJson) }, { protocol: 'chat_completions', operation: 'execute', ingress: 'gemini-generateContent' }, 'Gemini JSON sample');
    let markerRejected = false;
    try { assertClientPayload({ code: 0, value: 'yes' }, { protocol: 'chat_completions', operation: 'execute', ingress: 'chat_completions' }, 'marker'); } catch { markerRejected = true; }
    expect(markerRejected, 'a marker string passed the client JSON check');
    const messagesWire = nativeUpstreamWire('/v1/messages', false);
    expect(messagesWire.type === 'application/json' && messagesWire.body.includes('"text":"yes"'), 'Messages JSON wire drifted');
    const messagesStream = nativeUpstreamWire('/v1/messages', true);
    expect(messagesStream.type === 'text/event-stream' && messagesStream.body.includes('message_stop') && messagesStream.marker === 'hello', 'Messages SSE wire drifted');
    const countWire = nativeUpstreamWire('/v1/messages/count_tokens', false);
    expect(countWire.body.includes('"input_tokens":11') && !countWire.body.includes('totalTokens'), 'messages count wire became a Gemini count body');
    const agWire = nativeUpstreamWire('/v1internal:generateContent', false);
    expect(agWire.type === 'application/json' && agWire.body.includes('finishReason') && agWire.body.includes('"text":"yes"'), 'Antigravity JSON wire drifted');
    const agStream = nativeUpstreamWire('/v1internal:streamGenerateContent', false);
    expect(agStream.type === 'text/event-stream' && agStream.marker === 'first', 'Antigravity SSE wire drifted');
    expect(nativeUpstreamWire('/v1/chat/completions', true).marker === 'ok', 'chat SSE marker drifted');
    const prepare = nativeSourceSection(scriptSource, 'async function prepareNativeOperator', 'async function explainProtocol');
    expect(prepare.includes("outcome === 'imported'") && prepare.includes("outcome === 'alreadyImported'"), 'serialized import outcomes drifted');
    expect(!prepare.includes("outcome === 'Imported'") && !prepare.includes('AlreadyImported'), 'import assertion still expects PascalCase');
    expect(prepare.includes('catalogModelsForScope') && prepare.includes('destinationId') && prepare.includes('legacyAccountId'), 'catalog lookup left the credential destination');
    expect(!prepare.includes('baseUrl') && !prepare.includes("kind: 'only'"), 'catalog lookup matches baseUrl or narrows model scope');
    const target = { authority: { bindingId: 'bind-codex', credentialId: 'cred-codex', nativeProvider: 'codex' } };
    const other = { authority: { bindingId: 'bind-claude', credentialId: 'cred-claude', nativeProvider: 'anthropic' } };
    const state = { binding: { id: 'bind-codex' }, credentialId: 'cred-codex', nativeProvider: 'codex' };
    expect(eligibleIsTarget(target, state) === true, 'matching credential was rejected');
    expect(eligibleIsTarget(other, state) === false, 'another family credential was accepted');
    expect(eligibleIsTarget({ authority: { bindingId: 'bind-codex', credentialId: 'cred-codex', nativeProvider: 'anthropic' } }, state) === false, 'a mismatched provider was accepted');
    const isolation = nativeSourceSection(scriptSource, 'async function settleBindingApply', 'async function prepareNativeOperator');
    expect(isolation.includes('waitAutomaticApply') && isolation.includes('enabled: false') && isolation.includes('enabled: true') && isolation.includes("phase === 'unchanged'"), 'isolation explains before the existing apply barrier');
    expect(!isolation.includes('setTimeout') && !isolation.includes('runtime/start'), 'isolation added a sleep or a runtime start');
    const drive = nativeSourceSection(scriptSource, 'async function driveNativeCell', 'function assertSourceRefusal');
    expect(drive.includes('nativeClientRequest(driven)') && drive.includes('assertClientPayload') && drive.includes('assertTargetEligible') && drive.includes('familyState.branches') && drive.includes('compaction_trigger'), 'driver does not send the selected credential model');
    expect(!drive.includes('model: modelId') && !drive.includes('familyState.modelId') && !drive.includes('gemini-2.5-pro'), 'driver substitutes one shared model');
    const mapFn = nativeSourceSection(scriptSource, 'async function assertMapAfterApply', 'async function assertConfirmed429');
    expect(mapFn.includes('allowedEndpointIds') && mapFn.includes('allowedOrigins') && mapFn.includes('old grants') && mapFn.includes('assertClientPayload') && mapFn.includes('endpointId'), 'map change does not refuse old grants and then regrant');
    const pair = nativeSourceSection(scriptSource, "behavior === '429-pair'", 'function openNativeSink');
    expect(pair.includes('seen.size === 0') && pair.includes('world.nativeUpstreamCell') && !pair.includes('seen.has(bearer)'), '429 pair still rejects every new bearer');
    const confirmed = nativeSourceSection(scriptSource, 'async function assertConfirmed429', 'async function runNativeNegatives');
    expect(confirmed.includes('assertClientPayload') && confirmed.includes('nativeProvider') && confirmed.includes('fixtureLane()'), 'confirmed 429 does not assert the client payload on the fixture lane');
    const bootstrapReason = NATIVE_PENDING_INTERFACES.find(item => item[0] === 'stream-bootstrap')?.[1] ?? '';
    expect(bootstrapReason.includes('StreamBootstrapBuffering') && bootstrapReason.includes('not a finding'), 'bootstrap enablement evidence drifted');
    const plan = nativeScenarioPlan();
    const streamCell = plan.find(item => item.cell?.family === 'codex' && item.cell?.ingress === 'responses' && item.cell?.operation === 'stream');
    expect(streamCell?.kind === 'network' && streamCell.cell.liveSend === true, 'Codex responses stream left the network plan');
    const bootstrapPending = plan.find(item => item.interface === 'stream-bootstrap' && !item.cell);
    expect(bootstrapPending?.kind === 'pending', 'bootstrap enablement was closed by the stream cell');
    expect(world.nativeLoopback === undefined && ownedPids.size === 0, 'verified corrections started a listener');
  });
  const failed = cases.some(item => item.status === 'FAIL');
  const coverage = nativePlanCoverage();
  const payload = {
    status: failed ? 'fixture-fail' : 'blocked-dependency',
    closeout: false,
    wholeCli: 'incomplete',
    productRuntimeStarted: false,
    hookImplemented: false,
    nativeHook: NATIVE_HOOK.feature,
    positiveSends: coverage.runnable,
    runnableScenarios: coverage.runnable,
    networkScenarios: coverage.network,
    localUnsentScenarios: coverage.localUnsent,
    sourceRefusals: coverage.sourceRefusal,
    pendingScenarios: coverage.pending,
    pendingInterfaces: coverage.interfaces,
    zeroSendClasses: NATIVE_ZERO_SENDS,
    oneHitClasses: NATIVE_ONE_HITS,
    catalogCells: coverage.catalogCells,
    at: new Date().toISOString(),
    realLockPath: artifactLockPath,
    realLockPresent: after.present,
    realLockCreatedByFixture: !before.present && after.present,
    note: 'Native fixture checks are not CLI acceptance and not closeout. The root trusted lock is absent, so no product, listen, or child process ran. Runnable scenarios stay pending behind that lock. Source generation is not OAuth, login, or refresh proof. Public Antigravity Responses compaction_trigger is an operator client send with a parent execute/stream permit; it is not automatic internal-child evidence. Codex/XAI compact and Internal stay pending as required SDK/host evidence, distinct from that public trigger. Refresh-resend, stream-refresh, original-host, and generation-kinds stay unresolved required SDK/host evidence, not operator FAIL and not PASS. A Codex responses stream stays a network scenario. stream-bootstrap stays pending until Codex.StreamBootstrapBuffering is shown enabled and that stream runs. That pending row is enablement evidence, not a finding that the product lacks bootstrap. In-process wire and client-shape checks are source evidence. They did not exercise an operator send.',
    cases,
  };
  await mkdir(nativeWorkRoot, { recursive: true });
  await writeFile(join(nativeWorkRoot, 'native-fixture-receipt.json'), `${JSON.stringify(payload, null, 2)}\n`);
  const failedNames = cases.filter(item => item.status === 'FAIL').map(item => item.name);
  console.log(`${payload.status}; closeout false; whole CLI incomplete; product runtime started false; runnable ${coverage.runnable}; pending ${coverage.pending}; real lock present ${after.present}`);
  if (failedNames.length) console.log(`failed ${failedNames.join(', ')}`);
  return failed ? 1 : 0;
}
async function main() {
  if (options.lockFixtures) {
    process.exitCode = await runLockFixtures();
    return;
  }
  if (options.nativeFixtures) {
    process.exitCode = await runNativeFixtures();
    return;
  }
  if (options.list) {
    for (const item of stages) console.log(item.name);
    console.log('linux-macos-cross-build');
    console.log('real-oauth-provider');
    console.log('whole-goal-closeout');
    return;
  }
  if (!options.prerequisites) await prepareProfile();
  else {
    await mkdir(workRoot, { recursive: true });
    world.root = join(workRoot, 'prereq-home');
    await mkdir(join(world.root, 'home'), { recursive: true });
  }
  const stopAt = options.scenario ? stages.findIndex(item => item.name === options.scenario) : stages.length - 1;
  if (options.scenario && stopAt < 0) throw fail(`unknown scenario ${options.scenario}`);
  const planned = options.prerequisites ? stages.slice(0, 1) : stages.slice(0, stopAt + 1);
  for (const item of planned) {
    if (halted) {
      results.push({ name: item.name, status: 'BLOCKED', detail: 'an earlier stage halted the profile' });
      console.log(`BLOCKED ${item.name}: an earlier stage halted the profile`);
      continue;
    }
    try {
      await item.fn();
      if (!results.some(result => result.name === item.name)) {
        results.push({ name: item.name, status: 'PASS' });
        console.log(`PASS ${item.name}`);
      }
    } catch (error) {
      const status = error.kind === 'BLOCKED' ? 'BLOCKED' : 'FAIL';
      results.push({ name: item.name, status, detail: scrub(error.message).slice(0, 900) });
      console.log(`${status} ${item.name}: ${scrub(error.message).slice(0, 700)}`);
      if (['prerequisites', 'empty-init-register-login', 'install-pinned-child'].includes(item.name)) halted = true;
    }
  }
  const failed = results.some(result => result.status === 'FAIL');
  const requiredOpen = results.some(result => result.status === 'OPEN' && (result.evidenceLevel === 'required-sdk-host' || result.evidenceLevel === 'required-incomplete'));
  const incomplete = results.some(result => result.status === 'BLOCKED') || requiredOpen || !results.some(result => result.status === 'PASS');
  const status = failed ? 'fail' : incomplete ? 'incomplete' : 'pass';
  await writeReceipt(status);
  console.log(`receipt ${status}; closeout false; whole CLI incomplete`);
  process.exitCode = failed ? 1 : incomplete ? 2 : 0;
}

process.on('SIGINT', () => { shutdown().finally(() => process.exit(2)); });
main().catch(error => {
  console.error(scrub(error.stack || error.message));
  process.exitCode = 1;
}).finally(shutdown);
