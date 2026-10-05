// Retained normal-CLI control journeys.
// Source construction only. This file does not start a product under
// node --check or --list. A later primary grant runs it against ocg.
// Construction is not runtime acceptance.
//
// Trust is runtime/cpa/artifact-lock.json. The placeholder SHA is not a
// record. Sealed provider URLs stay sealed. OCG_CPA_TEST_ENDPOINTS is not set.
// Go and GOAT quota stay in scripts/cli-cpa-acceptance.mjs.

import { spawn, spawnSync } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { createReadStream } from 'node:fs';
import { mkdir, readFile, realpath, stat, writeFile } from 'node:fs/promises';
import { createServer as createNetServer, request as httpRequest } from 'node:http';
import { basename, dirname, join, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

const repo = dirname(dirname(fileURLToPath(import.meta.url)));
const evidenceRoot = join(repo, 'tmp', 'ocg3-cli-delivery', 'orchestration-20261004', 'integration-work', 'retained-controls-work');
const prefix = '/dashboard/api/v4';
const placeholderSHA256 = 'baff0e76f37b32f8e16618b6533f9a435dc36b4bc737302745b4b9353ed45542';
const artifactLockPath = join(repo, 'runtime', 'cpa', 'artifact-lock.json');
const cliExecutable = process.platform === 'win32' ? 'ocg.exe' : 'ocg';
const uuidPattern = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
const forbiddenUpstreamHeaders = ['x-ocg-request-id', 'x-ocg-process-generation', 'x-ocg-projection-revision', 'x-ocg-ready-token', 'cookie'];
const sealed = {
  minimaxBase: 'https://api.minimax.cn/v1',
  minimaxUsage: 'https://api.minimaxi.com/v1/token_plan/remains',
  kimiBase: 'https://api.kimi.com/coding/v1',
  kimiUsage: 'https://api.kimi.com/coding/v1/usages',
  ollamaBase: 'https://ollama.com',
  ollamaModels: 'https://ollama.com/v1/models',
  ollamaPricing: 'https://ollama.com/pricing',
  stepfunPlan: 'https://api.stepfun.com/step_plan/v1/chat/completions',
  stepfunApi: 'https://api.stepfun.com/v1/chat/completions',
  deepseek: 'https://api.deepseek.com/chat/completions',
  zhipu: 'https://open.bigmodel.cn/api/paas/v4/chat/completions',
};
const zenAccountId = '00000000-0000-0000-0000-000000000002';
const primaryKeyId = '00000000-0000-0000-0000-000000000001';

const args = process.argv.slice(2);
const options = {
  binary: join(repo, 'target', 'debug', cliExecutable),
  hostDir: join(repo, 'tmp', 'ocg3-cli-delivery', 'orchestration-20261004', 'runtime-build'),
  scratch: '',
  scenario: '',
  prerequisites: false,
  list: false,
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
  else throw new Error(`unknown argument ${arg}`);
}

const stages = [];
function stage(name, fn, { limitOnly = false } = {}) { stages.push({ name, fn, limitOnly }); }
const secrets = new Set();
const results = [];
const limits = [];
const ownedPids = new Set();
const world = {
  root: '',
  dataDir: '',
  endpoint: '',
  session: '',
  port: 0,
  serve: undefined,
  upstream: undefined,
  proxy: undefined,
  arrivals: [],
  proxyHits: [],
  proxyRefusals: [],
  keys: new Map(),
  clientKeys: new Set(),
  modelIds: ['retained-discovered'],
  gatewayKeyFile: '',
  primaryKey: '',
};
let serial = 0;
let trustedSha = '';
let halted = false;

function delay(ms) { return new Promise(done => setTimeout(done, ms)); }
function fail(message) { const error = new Error(message); error.kind = 'FAIL'; return error; }
function blocked(message) { const error = new Error(message); error.kind = 'BLOCKED'; return error; }
function assert(condition, message) { if (!condition) throw fail(message); }
function remember(secret) { if (secret) secrets.add(secret); return secret; }
function scrub(text) {
  let output = String(text ?? '');
  for (const secret of [...secrets].filter(Boolean).sort((left, right) => right.length - left.length)) output = output.split(secret).join('[redacted]');
  return output.replace(/(authorization|bearer|secret|token|password|api[-_]?key)\s*[:=]\s*\S+/gi, '$1=[redacted]');
}
function limit(name, gap) {
  const row = { name, status: 'LIMIT', driver: 'scripts/cli-retained-controls-acceptance.mjs', ...gap };
  limits.push(row);
  console.log(`LIMIT ${name}: ${gap.reason}`);
  return row;
}
function parseJson(text) {
  return JSON.parse(text, (_key, value, context) => (
    typeof value === 'number' && Number.isInteger(value) && !Number.isSafeInteger(value) && /^-?\d+$/.test(context?.source ?? '')
      ? BigInt(context.source) : value
  ));
}
function stringifyJson(value) {
  return JSON.stringify(value, (_key, item) => typeof item === 'bigint' ? JSON.rawJSON(item.toString()) : item);
}
function rawCaptured(run) {
  const body = typeof run?.rawOutput === 'string'
    ? run.rawOutput
    : (typeof run?.value === 'string' ? run.value : stringifyJson(run?.value ?? ''));
  return `${body}\n${run?.stdout ?? ''}\n${run?.stderr ?? ''}`;
}
function textOf(run) {
  return scrub(rawCaptured(run));
}
function loopbackAddress(address) {
  return address === '127.0.0.1' || address === '::1' || address === '::ffff:127.0.0.1';
}
function within(parent, child) {
  const from = resolve(parent).toLowerCase();
  const to = resolve(child).toLowerCase();
  return to === from || to.startsWith(from + sep);
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
function environment() {
  const home = join(world.root, 'home');
  const env = { ...process.env, HOME: home, USERPROFILE: home, OCG_MANAGER_ENCRYPTION_KEY: 'synthetic-retained-controls-cipher' };
  for (const name of [
    'CODEX_HOME', 'CLAUDE_CONFIG_DIR', 'KIMI_CODE_HOME', 'GROK_HOME', 'MINIMAX_DATA_DIR', 'MAVIS_DATA_DIR',
    'ZCODE_PERSONAL_PROVIDER_CONFIG_FILE', 'ZCODE_DATA_BASE_DIR', 'DSH_HOME',
    'OCG_BROWSER_WORKER_URL', 'OCG_BROWSER_CONTROL_TOKEN_FILE', 'OCG_CPA_BASE_URL', 'OCG_CPA_HOST_DIR',
    'MANAGEMENT_PASSWORD', 'OCG_CPA_TEST_ENDPOINTS',
  ]) delete env[name];
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
      resolveDone({ code, stdout, stderr, timedOut, pid: child.pid });
    });
  });
  child.stdout.on('data', chunk => { stdout += chunk; });
  child.stderr.on('data', chunk => { stderr += chunk; });
  child.stdin.end();
  return done;
}
function invoke(argv, { timeoutMs = 200000 } = {}) { return begin(argv, timeoutMs); }
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
  if (success) assert(run.code === 0, `${method} ${path} failed: ${scrub(run.stderr).slice(0, 500)}`);
  let text = '';
  try { text = await readFile(output, 'utf8'); } catch {}
  let value;
  try { value = text ? parseJson(text) : undefined; } catch { value = text; }
  return { ...run, value, rawOutput: text };
}
async function freePort() {
  const server = createNetServer();
  await new Promise((done, reject) => server.listen(0, '127.0.0.1', error => error ? reject(error) : done()));
  const { port } = server.address();
  await new Promise(done => server.close(done));
  return port;
}
async function readRequest(req) {
  const chunks = [];
  for await (const chunk of req) chunks.push(chunk);
  return Buffer.concat(chunks).toString('utf8');
}
function sendJson(res, status, value) {
  res.writeHead(status, { 'content-type': 'application/json' });
  res.end(JSON.stringify(value));
}
function classifyArrival(req) {
  const names = Object.keys(req.headers).map(name => name.toLowerCase());
  const forbiddenHeader = forbiddenUpstreamHeaders.some(name => names.includes(name));
  const xApiKey = req.headers['x-api-key'];
  const authorization = String(req.headers.authorization ?? '');
  const bearer = authorization.replace(/^Bearer\s+/i, '');
  let authMode = 'none';
  let label = 'none';
  if (xApiKey) {
    authMode = 'x-api-key';
    label = world.keys.get(String(xApiKey)) ?? 'other';
  } else if (authorization) {
    authMode = 'bearer';
    if (world.clientKeys.has(bearer)) label = 'client';
    else label = world.keys.get(bearer) ?? (bearer ? 'other' : 'none');
  }
  return { forbiddenHeader, authMode, label, hasXApiKey: Boolean(xApiKey), hasAuthorization: Boolean(authorization) };
}
function recordArrival(req, bodyText) {
  let body = {};
  try { body = JSON.parse(bodyText); } catch {}
  const classified = classifyArrival(req);
  const model = body.model ?? '';
  const hit = { seq: world.arrivals.length + 1, method: req.method, path: String(req.url ?? '').split('?')[0], model, ...classified };
  world.arrivals.push(hit);
  return { body, hit };
}
function startUpstream() {
  const server = createNetServer(async (req, res) => {
    if (!loopbackAddress(req.socket.remoteAddress)) { res.writeHead(403); res.end(); return; }
    const text = await readRequest(req);
    const { body, hit } = recordArrival(req, text);
    if (hit.label === 'client' || hit.label === 'other') { sendJson(res, 401, { error: { message: 'fixture credential rejected', type: 'auth' } }); return; }
    if (hit.path.endsWith('/models')) { sendJson(res, 200, { object: 'list', data: world.modelIds.map(id => ({ id, object: 'model' })) }); return; }
    sendJson(res, 200, { id: 'chatcmpl-retained', object: 'chat.completion', created: 1, model: body.model, choices: [{ index: 0, message: { role: 'assistant', content: 'retained-loopback-ok' }, finish_reason: 'stop' }], usage: { prompt_tokens: 11, completion_tokens: 7, total_tokens: 18 } });
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
    const headers = { ...req.headers, host: target.host };
    delete headers['proxy-connection'];
    const proxyReq = httpRequest({ hostname: '127.0.0.1', port: target.port || upstreamPort, path: `${target.pathname}${target.search}`, method: req.method, headers }, proxyRes => {
      res.writeHead(proxyRes.statusCode ?? 502, proxyRes.headers);
      proxyRes.pipe(res);
    });
    proxyReq.on('error', () => { res.writeHead(502); res.end(); });
    req.pipe(proxyReq);
  });
  return new Promise(done => server.listen(0, '127.0.0.1', () => done(server)));
}
function startFixture(onRequest) {
  const hits = [];
  const server = createNetServer(async (req, res) => {
    if (!loopbackAddress(req.socket.remoteAddress)) { res.writeHead(403); res.end(); return; }
    const url = new URL(req.url ?? '/', 'http://127.0.0.1');
    const hit = { method: req.method, path: url.pathname, query: url.search, authorization: String(req.headers.authorization ?? ''), newApiUser: String(req.headers['new-api-user'] ?? '') };
    hits.push(hit);
    const text = await readRequest(req);
    const handled = onRequest(hit, text);
    if (handled === undefined) { res.writeHead(404); res.end(); return; }
    sendJson(res, handled.status ?? 200, handled.body ?? {});
  });
  return new Promise(done => server.listen(0, '127.0.0.1', () => done({ server, hits, origin() { return `http://127.0.0.1:${server.address().port}`; } })));
}
async function closeServer(server) {
  if (!server) return;
  await new Promise(done => server.close(done));
}
async function startServe(dataDir) {
  world.dataDir = dataDir;
  if (!world.port) world.port = await freePort();
  world.endpoint = `http://127.0.0.1:${world.port}`;
  const child = track(spawn(options.binary, [
    '--data-dir', dataDir, 'serve', '--host', '127.0.0.1', '--port', String(world.port), '--cpa-host-dir', options.hostDir,
  ], { env: environment(), windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] }));
  let stderr = '';
  child.stderr.on('data', chunk => { stderr = scrub(`${stderr}${chunk}`).slice(-4000); });
  child.stdout.resume();
  world.serve = child;
  for (let attempt = 0; attempt < 120; attempt += 1) {
    if (child.exitCode !== null) throw fail(`serve exited ${child.exitCode}: ${stderr.slice(0, 500)}`);
    try {
      const response = await fetch(`${world.endpoint}/dashboard/api/v4/auth/status`);
      if (response.ok) return world.port;
    } catch {}
    await delay(250);
  }
  throw fail(`serve did not answer auth status: ${stderr.slice(0, 500)}`);
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
  await stopServe();
  for (const pid of [...ownedPids]) killOwned(pid);
  await closeServer(world.upstream);
  await closeServer(world.proxy);
  world.upstream = undefined;
  world.proxy = undefined;
}
function appliedRevision(value) {
  if (value?.appliedRevision !== undefined && value?.appliedRevision !== null) return value.appliedRevision;
  const matched = /^child=(\d+);desired=(\d+);applied=(\d+);/.exec(String(value?.currentOperation ?? ''));
  return matched ? Number(matched[3]) : null;
}
async function readRuntime() {
  return (await api('GET', `${prefix}/external-integrations/cpa/runtime`)).value;
}
function readyRuntime(value) {
  return value?.applyStatus === 'applied' && value?.policyReady === true && value?.executionUnavailable === false && value?.running === true && value?.owned === true && String(value?.assetSha256 ?? '').toLowerCase() === trustedSha;
}
async function ensureReady(label) {
  const deadline = Date.now() + 8000;
  let last;
  while (Date.now() < deadline) {
    last = await readRuntime();
    if (readyRuntime(last)) return last;
    if (last?.applyStatus === 'apply_failed') throw fail(`${label} apply failed`);
    await delay(250);
  }
  throw fail(`${label} did not reach Ready`);
}
async function appliedAfter(previous, label) {
  const deadline = Date.now() + 8000;
  let last;
  while (Date.now() < deadline) {
    last = await readRuntime();
    const applied = appliedRevision(last);
    if (readyRuntime(last) && String(applied) !== String(previous)) return last;
    if (last?.applyStatus === 'apply_failed') throw fail(`${label} apply failed`);
    await delay(250);
  }
  throw fail(`${label} did not apply a new revision`);
}
async function contractRevision() {
  return (await api('GET', `${prefix}/contract`)).value?.revision;
}
function upstreamUrl() {
  return `http://127.0.0.1:${world.upstream.address().port}/v1`;
}
function rememberKey(label, secret) {
  remember(secret);
  world.keys.set(secret, label);
  return secret;
}
async function gatewayKeyFile() {
  if (world.gatewayKeyFile) return world.gatewayKeyFile;
  const connection = (await api('GET', `${prefix}/connection`)).value;
  assert(connection.primaryKey && connection.primaryKey !== '[redacted]', 'private connection output did not contain the client key');
  world.primaryKey = remember(connection.primaryKey);
  world.clientKeys.add(connection.primaryKey);
  world.gatewayKeyFile = join(world.root, 'gateway-key.txt');
  await writeFile(world.gatewayKeyFile, connection.primaryKey);
  return world.gatewayKeyFile;
}
async function sendChat(model, content, keyFile, { success = true } = {}) {
  const mark = world.arrivals.length;
  const run = await api('POST', '/v1/chat/completions', { model, messages: [{ role: 'user', content }] }, { extra: ['--key-file', keyFile], success: false });
  const hits = world.arrivals.slice(mark);
  if (success) {
    assert(run.code === 0, `chat ${model} failed: ${scrub(run.stderr).slice(0, 400)}`);
    assert(run.value?.choices?.[0]?.message?.content === 'retained-loopback-ok', `chat ${model} did not return the loopback payload`);
  }
  return { run, hits };
}
function assertHit(hit, { label, model, authMode }) {
  assert(hit, 'expected an upstream hit');
  assert(hit.forbiddenHeader === false, `${label} forwarded a private correlation header`);
  assert(hit.label !== 'client', `${label} forwarded the client key`);
  assert(hit.label === label, `${model} credential was ${hit.label}`);
  assert(hit.model === model, `${label} upstream model was ${hit.model}`);
  assert(hit.authMode === authMode, `${label} auth mode was ${hit.authMode}`);
  if (authMode === 'x-api-key') assert(hit.hasXApiKey === true && hit.hasAuthorization === false, 'x-api-key send also carried Authorization');
  if (authMode === 'none') assert(hit.hasXApiKey === false && hit.hasAuthorization === false, 'no-auth send carried a credential header');
  if (authMode === 'bearer') assert(hit.hasAuthorization === true && hit.hasXApiKey === false, 'bearer send carried x-api-key');
}
async function destinationByName(name) {
  const destinations = (await api('GET', `${prefix}/destinations`)).value?.destinations ?? [];
  const destination = destinations.find(item => item.name === name);
  assert(destination?.id, `${name} destination was not listed`);
  return destination;
}
function endpointOf(destination) {
  const route = (destination.protocolRoutes ?? []).find(item => item?.endpointUrl);
  return route?.endpointUrl || destination.baseUrl || destination.endpointUrl || '';
}
async function identityForCredential(credentialId) {
  const identities = (await api('GET', `${prefix}/accounts`)).value?.identities ?? [];
  for (const identity of identities) {
    const summary = (identity.credentials ?? []).find(item => item?.credential?.id === credentialId);
    if (summary) return { identity, summary };
  }
  throw fail(`credential ${credentialId} has no identity`);
}
async function onboard(body) {
  const before = appliedRevision(await readRuntime());
  const value = (await api('POST', `${prefix}/onboarding/commit`, { operationId: randomUUID(), authorizeCurrentEndpoint: true, ...body }, { cas: true })).value;
  await appliedAfter(before, body.connection?.name || 'onboarding');
  return value;
}
function assertNoSecret(run, secret, label) {
  assert(!String(run.stdout ?? '').includes(secret), `${label} printed a secret on stdout`);
  assert(!String(run.stderr ?? '').includes(secret), `${label} printed a secret on stderr`);
}
function hostOf(url) {
  try { return new URL(url).hostname; } catch { return ''; }
}
function assertSealedHttps(url, host, label) {
  const parsed = new URL(url);
  assert(parsed.protocol === 'https:', `${label} URL is not https`);
  assert(parsed.hostname === host, `${label} host was ${parsed.hostname}`);
  assert(!['127.0.0.1', 'localhost', '::1'].includes(parsed.hostname), `${label} URL was rewritten to loopback`);
}
function templateEndpoint(template) {
  return (template?.defaultEndpoints ?? []).map(item => item?.url).find(url => typeof url === 'string' && url) ?? '';
}
async function findTemplate(id) {
  const templates = (await api('GET', `${prefix}/templates`)).value?.templates ?? [];
  return templates.find(item => item.id === id) ?? null;
}
async function accountByName(name) {
  const records = (await api('GET', `${prefix}/account-records`)).value;
  const account = (records?.accounts ?? []).find(item => item.name === name);
  assert(account?.id, `${name} was not in account records`);
  return account;
}
async function createPlanAccount(body, label) {
  const before = appliedRevision(await readRuntime());
  const created = await api('POST', `${prefix}/accounts`, body, { cas: true, success: false });
  if (created.code !== 0) throw fail(`${label} create failed: ${textOf(created).slice(0, 400)}`);
  await appliedAfter(before, label);
  return accountByName(body.name);
}
function creditBody(model, remaining) {
  return {
    configuration: { name: 'Retained credits', currency: 'USD', creditsPerCurrency: 1, rates: [{ model, inputPerMillion: 1, outputPerMillion: 1, cacheReadPerMillion: null, cacheWritePerMillion: null }], monthly: null, sourceUrl: null },
    initialBuckets: [{ id: `retained-${model}`, kind: 'manual', label: 'Retained zero', granted: remaining, remaining, startsAt: new Date().toISOString(), expiresAt: null }],
  };
}
function bindingView(binding) {
  return {
    id: binding?.id ?? '',
    enabled: binding?.enabled === true,
    modelScope: binding?.modelScope ?? null,
    allowedEndpointIds: [...(binding?.allowedEndpointIds ?? [])].sort(),
    allowedOrigins: [...(binding?.allowedOrigins ?? [])].sort(),
  };
}
function catalogView(destination) {
  return (destination.catalog ?? []).map(model => ({
    publicModel: model.publicModel ?? '',
    upstreamModel: model.upstreamModel ?? '',
    protocols: model.protocols ?? [],
    preferred: model.preferred ?? null,
    enabled: model.enabled !== false,
  }));
}

function isLowerHex(value, length) {
  return typeof value === 'string' && new RegExp(`^[0-9a-f]{${length}}$`).test(value);
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
function safeBasename(value) {
  if (typeof value !== 'string' || !value || value.includes('/') || value.includes('\\') || value === '.' || value === '..') return '';
  return value;
}
async function acceptArtifact() {
  if (typeof JSON.rawJSON !== 'function') throw blocked('Node JSON.rawJSON is required so revision integers survive the CLI');
  if (process.platform !== 'win32' || process.arch !== 'x64') throw blocked(`pinned Windows x64 host cannot be claimed on ${process.platform}/${process.arch}`);
  const binaryName = basename(options.binary).toLowerCase();
  if (binaryName !== cliExecutable.toLowerCase()) throw blocked('binary name is not ocg');
  try { await stat(options.binary); } catch { throw blocked('ocg binary is absent'); }
  let lockText;
  try { lockText = await readFile(artifactLockPath, 'utf8'); }
  catch { throw blocked('trusted artifact lock is absent'); }
  let lock;
  try { lock = JSON.parse(lockText); } catch { throw blocked('trusted artifact lock is not JSON'); }
  if (lock?.schemaVersion !== 1) throw blocked('trusted artifact lock schemaVersion is not 1');
  const artifacts = Array.isArray(lock.artifacts) ? lock.artifacts : [];
  const currentOs = normalizePlatformToken(process.platform, 'os');
  const currentArch = normalizePlatformToken(process.arch, 'arch');
  const hostSuite = artifacts.filter(record => record && normalizePlatformToken(record.os, 'os') === currentOs && normalizePlatformToken(record.arch, 'arch') === currentArch && record.variant === 'production' && record.verification === 'host-suite');
  if (hostSuite.length !== 1) throw blocked('trusted artifact lock has no single production host-suite record for this platform');
  const record = hostSuite[0];
  if (!isLowerHex(record.sha256, 64) || record.sha256 === placeholderSHA256) throw blocked('placeholder SHA is not an accepted lock record');
  const executable = safeBasename(record.executable);
  if (!executable) throw blocked('trusted artifact lock executable is not a basename');
  const fileSha = await sha256File(join(options.hostDir, executable));
  if (fileSha !== record.sha256) throw blocked('executable bytes are not the trusted lock record');
  let manifest;
  try { manifest = JSON.parse(await readFile(join(options.hostDir, 'manifest.json'), 'utf8')); }
  catch { throw blocked('pinned manifest.json is unreadable'); }
  if (String(manifest.executableSHA256 ?? '').toLowerCase() !== record.sha256) throw blocked('manifest executableSHA256 is not the trusted lock record');
  if (manifest.variant !== 'production') throw blocked('manifest variant is not production');
  trustedSha = record.sha256;
}

async function prepareProfile() {
  const profiles = join(evidenceRoot, 'profiles');
  await mkdir(profiles, { recursive: true });
  const requested = options.scratch || join(profiles, randomUUID());
  if (!within(profiles, requested)) throw fail('scratch is outside retained-controls-work/profiles');
  const uuid = relative(profiles, requested).split(sep)[0];
  if (!uuidPattern.test(uuid) || relative(profiles, requested).includes(sep)) throw fail('scratch must be one UUID directory');
  await mkdir(requested, { recursive: true });
  const real = await realpath(requested);
  if (!within(await realpath(profiles), real)) throw fail('scratch real path left retained-controls-work/profiles');
  world.root = real;
  world.dataDir = join(real, 'data');
  world.session = join(real, 'session.json');
  await mkdir(join(real, 'home'), { recursive: true });
  await mkdir(world.dataDir, { recursive: true });
  remember('synthetic-retained-controls-cipher');
  remember('synthetic-retained-admin-password');
}

stage('configured-http-x-api-key-and-none', async () => {
  const xApiSecret = rememberKey('xapi', 'synthetic-xapi-key-retained');
  const noneModel = 'upstream-none';
  world.modelIds.push('upstream-xapi', noneModel);
  await onboard({
    mode: 'complete',
    connection: { kind: 'new', templateId: 'custom', name: 'Retained x-api-key', endpointUrl: upstreamUrl(), upstreamProtocol: 'chat_completions', authKind: 'x-api-key' },
    authorization: { kind: 'api_key', secretInput: xApiSecret, accountLabel: 'Retained x-api-key' },
    targets: [{ publicModel: 'retained-xapi', upstreamModel: 'upstream-xapi' }],
  });
  await onboard({
    mode: 'complete',
    connection: { kind: 'new', templateId: 'custom', name: 'Retained none', endpointUrl: upstreamUrl(), upstreamProtocol: 'chat_completions', authKind: 'none' },
    authorization: { kind: 'none' },
    targets: [{ publicModel: 'retained-none', upstreamModel: noneModel }],
  });
  const keyFile = await gatewayKeyFile();
  const xapi = await sendChat('retained-xapi', 'xapi', keyFile);
  assert(xapi.hits.length === 1, `x-api-key send count was ${xapi.hits.length}`);
  assertHit(xapi.hits[0], { label: 'xapi', model: 'upstream-xapi', authMode: 'x-api-key' });
  const none = await sendChat('retained-none', 'none', keyFile);
  assert(none.hits.length === 1, `no-auth send count was ${none.hits.length}`);
  assertHit(none.hits[0], { label: 'none', model: noneModel, authMode: 'none' });
});

stage('configured-http-draft-resume', async () => {
  const secret = rememberKey('draft', 'synthetic-draft-key-retained');
  world.modelIds.push('upstream-draft');
  const revisionBefore = await contractRevision();
  const grantBefore = stringifyJson((await api('GET', `${prefix}/accounts`)).value?.identities ?? []);
  const mark = world.arrivals.length;
  const discovered = await api('POST', `${prefix}/providers/models/discover`, {
    endpointUrl: upstreamUrl(), upstreamProtocol: 'chat_completions', authKind: 'bearer', key: secret,
  }, { success: false });
  assert(discovered.code === 0, `draft discover failed: ${textOf(discovered).slice(0, 400)}`);
  assert(!rawCaptured(discovered).includes(secret), 'draft discover echoed the key');
  assert(await contractRevision() === revisionBefore, 'draft discover bumped the settings revision');
  assert(stringifyJson((await api('GET', `${prefix}/accounts`)).value?.identities ?? []) === grantBefore, 'draft discover expanded grants');
  assert(world.arrivals.slice(mark).length === 1, 'draft discover did not use the saved loopback once');
  const probed = await api('POST', `${prefix}/providers/test`, {
    endpointUrl: upstreamUrl(), upstreamProtocol: 'chat_completions', authKind: 'bearer', publicModel: 'retained-draft', upstreamModel: 'upstream-draft', key: secret,
  }, { success: false });
  assert(probed.code !== 0, 'provider test succeeded before the destination was saved');
  assert(textOf(probed).includes('save the destination and credential, then use its model test'), `provider test refusal was ${textOf(probed).slice(0, 300)}`);
  assert(!rawCaptured(probed).includes(secret), 'unsaved provider test echoed the key');
  assert(world.arrivals.length === mark + 1, 'unsaved provider test contacted the upstream');
  const drafted = await onboard({
    mode: 'draft',
    connection: { kind: 'new', templateId: 'custom', name: 'Retained draft', endpointUrl: upstreamUrl(), upstreamProtocol: 'chat_completions', authKind: 'bearer' },
    authorization: { kind: 'api_key', secretInput: secret, accountLabel: 'Retained draft' },
    targets: [],
  });
  const premature = await sendChat('retained-draft', 'draft', await gatewayKeyFile(), { success: false });
  assert(premature.run.code !== 0, 'draft connection accepted inference');
  assert(premature.hits.length === 0, 'draft connection sent upstream');
  await onboard({
    mode: 'complete',
    connection: {
      kind: 'existing',
      connectionId: drafted.connectionId,
      configuration: { templateId: 'custom', name: 'Retained draft', endpointUrl: upstreamUrl(), upstreamProtocol: 'chat_completions', authKind: 'bearer' },
    },
    authorization: { kind: 'api_key', secretInput: secret, accountLabel: 'Retained draft' },
    targets: [{ publicModel: 'retained-draft', upstreamModel: 'upstream-draft' }],
  });
  const resumed = await sendChat('retained-draft', 'resume', world.gatewayKeyFile);
  assert(resumed.hits.length === 1, `resumed draft send count was ${resumed.hits.length}`);
  assertHit(resumed.hits[0], { label: 'draft', model: 'upstream-draft', authMode: 'bearer' });
});

stage('configured-http-multiple-credentials-and-scopes', async () => {
  const secretA = rememberKey('scope-a', 'synthetic-scope-a-retained');
  const secretB = rememberKey('scope-b', 'synthetic-scope-b-retained');
  world.modelIds.push('upstream-scope-a', 'upstream-scope-b');
  const created = await onboard({
    mode: 'complete',
    connection: { kind: 'new', templateId: 'custom', name: 'Retained scopes', endpointUrl: upstreamUrl(), upstreamProtocol: 'chat_completions', authKind: 'bearer' },
    authorization: { kind: 'api_key', secretInput: secretA, accountLabel: 'Retained scope A' },
    targets: [{ publicModel: 'retained-scope-a', upstreamModel: 'upstream-scope-a' }, { publicModel: 'retained-scope-b', upstreamModel: 'upstream-scope-b' }],
  });
  const first = await identityForCredential(created.credentialId);
  const bindingA = first.summary.bindings?.[0]?.id;
  assert(bindingA, 'first scoped credential has no binding');
  const before = appliedRevision(await readRuntime());
  const second = (await api('POST', `${prefix}/identities/${first.identity.identity?.id ?? first.identity.id}/credentials`, {
    connectionId: created.connectionId, secretInput: secretB, accountLabel: 'Retained scope B',
  }, { cas: true })).value;
  await appliedAfter(before, 'second credential');
  const bindingB = second.bindingId;
  assert(bindingB && bindingB !== bindingA, 'second credential did not receive its own binding');
  const scoped = appliedRevision(await readRuntime());
  await api('PATCH', `${prefix}/bindings/${bindingA}`, { modelScope: { kind: 'only', models: ['retained-scope-a'] }, enabled: true }, { cas: true });
  await api('PATCH', `${prefix}/bindings/${bindingB}`, { modelScope: { kind: 'only', models: ['retained-scope-b'] }, enabled: true }, { cas: true });
  await appliedAfter(scoped, 'binding scopes');
  world.scopeBindings = { bindingA, bindingB };
  const keyFile = await gatewayKeyFile();
  const sendA = await sendChat('retained-scope-a', 'scope-a', keyFile);
  assert(sendA.hits.length === 1, `scoped A send count was ${sendA.hits.length}`);
  assertHit(sendA.hits[0], { label: 'scope-a', model: 'upstream-scope-a', authMode: 'bearer' });
  const sendB = await sendChat('retained-scope-b', 'scope-b', keyFile);
  assert(sendB.hits.length === 1, `scoped B send count was ${sendB.hits.length}`);
  assertHit(sendB.hits[0], { label: 'scope-b', model: 'upstream-scope-b', authMode: 'bearer' });
});

stage('configured-http-alias-preference-disable-delete-empty-catalog', async () => {
  const secret = rememberKey('alias', 'synthetic-alias-key-retained');
  world.modelIds.push('upstream-alias', 'upstream-disabled');
  await onboard({
    mode: 'complete',
    connection: { kind: 'new', templateId: 'custom', name: 'Retained alias', endpointUrl: upstreamUrl(), upstreamProtocol: 'chat_completions', authKind: 'bearer' },
    authorization: { kind: 'api_key', secretInput: secret, accountLabel: 'Retained alias' },
    targets: [{ publicModel: 'retained-alias-public', upstreamModel: 'upstream-alias' }, { publicModel: 'retained-disabled', upstreamModel: 'upstream-disabled' }],
  });
  const destination = await destinationByName('Retained alias');
  const endpointUrl = endpointOf(destination);
  assert(endpointUrl === upstreamUrl(), 'alias destination left the synthetic loopback');
  const before = appliedRevision(await readRuntime());
  await api('PATCH', `${prefix}/destinations/${destination.id}`, {
    name: destination.name,
    endpointUrl,
    upstreamProtocol: destination.protocols?.[0] ?? 'chat_completions',
    authScheme: destination.authScheme,
    authorizeCredentialIds: [],
    enabled: true,
    models: [
      { publicModel: 'retained-alias', upstreamModel: 'upstream-alias', enabled: true, protocols: ['chat_completions'], preferred: 'chat_completions' },
      { publicModel: 'retained-disabled', upstreamModel: 'upstream-disabled', enabled: false, protocols: ['chat_completions'] },
    ],
  }, { cas: true });
  await appliedAfter(before, 'alias patch');
  const edited = await destinationByName('Retained alias');
  const alias = (edited.catalog ?? []).find(model => model.publicModel === 'retained-alias');
  const disabled = (edited.catalog ?? []).find(model => model.publicModel === 'retained-disabled');
  assert(alias?.upstreamModel === 'upstream-alias' && alias.preferred === 'chat_completions' && alias.enabled !== false, 'alias preference was not stored');
  assert(disabled?.enabled === false, 'disabled model stayed enabled');
  const keyFile = await gatewayKeyFile();
  const sent = await sendChat('retained-alias', 'alias', keyFile);
  assert(sent.hits.length === 1, `alias send count was ${sent.hits.length}`);
  assertHit(sent.hits[0], { label: 'alias', model: 'upstream-alias', authMode: 'bearer' });
  const hidden = await sendChat('retained-disabled', 'disabled', keyFile, { success: false });
  assert(hidden.run.code !== 0 && hidden.hits.length === 0, 'disabled model was sent');
  const oldName = await sendChat('retained-alias-public', 'old-public', keyFile, { success: false });
  assert(oldName.run.code !== 0 && oldName.hits.length === 0, 'replaced public model was sent');
  const removed = appliedRevision(await readRuntime());
  await api('PUT', `${prefix}/destinations/${destination.id}/catalog`, { updates: [], removeModels: ['retained-alias', 'retained-disabled'] }, { cas: true });
  await appliedAfter(removed, 'empty catalog');
  const empty = await destinationByName('Retained alias');
  assert((empty.catalog ?? []).length === 0, 'catalog remove left a model');
  const afterEmpty = await sendChat('retained-alias', 'empty', keyFile, { success: false });
  assert(afterEmpty.run.code !== 0 && afterEmpty.hits.length === 0, 'empty catalog still sent');
});

stage('configured-http-discovery-does-not-expand-grants', async () => {
  const secret = rememberKey('discover', 'synthetic-discover-key-retained');
  world.modelIds.push('upstream-original');
  const created = await onboard({
    mode: 'complete',
    connection: { kind: 'new', templateId: 'custom', name: 'Retained discovery', endpointUrl: upstreamUrl(), upstreamProtocol: 'chat_completions', authKind: 'bearer' },
    authorization: { kind: 'api_key', secretInput: secret, accountLabel: 'Retained discovery' },
    targets: [{ publicModel: 'retained-original', upstreamModel: 'upstream-original' }],
  });
  const found = await identityForCredential(created.credentialId);
  const binding = found.summary.bindings?.[0];
  assert(binding?.id, 'discovery credential has no binding');
  const narrowed = appliedRevision(await readRuntime());
  await api('PATCH', `${prefix}/bindings/${binding.id}`, { modelScope: { kind: 'only', models: ['retained-original'] } }, { cas: true });
  await appliedAfter(narrowed, 'discovery scope');
  const beforeBinding = bindingView((await identityForCredential(created.credentialId)).summary.bindings[0]);
  const destination = await destinationByName('Retained discovery');
  const mark = world.arrivals.length;
  const revisionBefore = await contractRevision();
  const refreshed = (await api('POST', `${prefix}/destinations/${destination.id}/catalog/refresh`, {}, { cas: true })).value;
  assert(refreshed.destination?.id === destination.id, 'catalog refresh returned another destination');
  assert(world.arrivals.slice(mark).some(hit => hit.path.endsWith('/models')), 'catalog refresh did not read the saved endpoint');
  assert(await contractRevision() !== revisionBefore || refreshed.revision, 'catalog refresh returned no revision');
  const after = await destinationByName('Retained discovery');
  assert((after.catalog ?? []).some(model => model.publicModel === 'retained-discovered' || model.upstreamModel === 'retained-discovered'), 'catalog refresh did not record the ungranted model');
  const afterBinding = bindingView((await identityForCredential(created.credentialId)).summary.bindings[0]);
  assert(stringifyJson(afterBinding.modelScope) === stringifyJson(beforeBinding.modelScope), 'catalog refresh expanded modelScope');
  assert(stringifyJson(afterBinding.allowedEndpointIds) === stringifyJson(beforeBinding.allowedEndpointIds), 'catalog refresh expanded allowedEndpointIds');
  const keyFile = await gatewayKeyFile();
  const denied = await sendChat('retained-discovered', 'discovered', keyFile, { success: false });
  assert(denied.run.code !== 0 && denied.hits.length === 0, 'ungranted discovered model was sent');
  const allowed = await sendChat('retained-original', 'original', keyFile);
  assert(allowed.hits.length === 1, 'original granted model was not sent');
  assertHit(allowed.hits[0], { label: 'discover', model: 'upstream-original', authMode: 'bearer' });
});

stage('configured-http-round-robin-rotates', async () => {
  const secretA = rememberKey('robin-a', 'synthetic-robin-a-retained');
  const secretB = rememberKey('robin-b', 'synthetic-robin-b-retained');
  world.modelIds.push('upstream-robin');
  const created = await onboard({
    mode: 'complete',
    connection: { kind: 'new', templateId: 'custom', name: 'Retained round-robin', endpointUrl: upstreamUrl(), upstreamProtocol: 'chat_completions', authKind: 'bearer' },
    authorization: { kind: 'api_key', secretInput: secretA, accountLabel: 'Retained robin A' },
    targets: [{ publicModel: 'retained-robin', upstreamModel: 'upstream-robin' }],
  });
  const first = await identityForCredential(created.credentialId);
  const identityId = first.identity.identity?.id ?? first.identity.id;
  const before = appliedRevision(await readRuntime());
  await api('POST', `${prefix}/identities/${identityId}/credentials`, { connectionId: created.connectionId, secretInput: secretB, accountLabel: 'Retained robin B' }, { cas: true });
  await appliedAfter(before, 'round-robin credential');
  const mode = appliedRevision(await readRuntime());
  await api('PUT', `${prefix}/settings`, { routingMode: 'round-robin', conversationSticky: false }, { cas: true });
  await appliedAfter(mode, 'round-robin mode');
  const keyFile = await gatewayKeyFile();
  const firstSend = await sendChat('retained-robin', 'robin-1', keyFile);
  const secondSend = await sendChat('retained-robin', 'robin-2', keyFile);
  const hits = [...firstSend.hits, ...secondSend.hits];
  assert(hits.length === 2, `round-robin send count was ${hits.length}`);
  assert(hits[0].label !== hits[1].label, 'round-robin repeated one credential');
  assert(hits.every(hit => hit.label === 'robin-a' || hit.label === 'robin-b'), `round-robin labels were ${hits.map(hit => hit.label).join(',')}`);
  assert(hits.every(hit => hit.model === 'upstream-robin' && hit.authMode === 'bearer' && hit.label !== 'client' && hit.forbiddenHeader === false), 'round-robin send changed model or credential headers');
  const restore = appliedRevision(await readRuntime());
  await api('PUT', `${prefix}/settings`, { routingMode: 'strict-priority', conversationSticky: false }, { cas: true });
  await appliedAfter(restore, 'strict-priority restore');
});

stage('client-key-primary-regenerate-and-disabled-zero-send', async () => {
  limit('client-key-model-allowlist', {
    method: 'PATCH',
    route: '/dashboard/api/v4/keys/{id}',
    input: { name: 'string', enabled: 'boolean' },
    source: 'KeyCreate and KeyUpdate accept name and enabled only. SubGatewayKey has no model scope.',
    reason: 'Binding modelScope is the upstream credential permission, not a per-client-key allowlist. This driver does not invent a client-key model field.',
    retainedCapability: 'configured-http-multiple-credentials-and-scopes',
  });
  const listed = (await api('GET', `${prefix}/connection`)).value;
  assert(listed.primaryKey === world.primaryKey, 'private connection output changed the primary key');
  const publicRun = await invoke(['--data-dir', world.dataDir, '--endpoint', world.endpoint, 'api', 'GET', `${prefix}/connection`, '--session-file', world.session]);
  assert(publicRun.code === 0, scrub(publicRun.stderr));
  assert(!String(publicRun.stdout ?? '').includes(world.primaryKey), 'stdout connection output printed the primary key');
  assert(!String(publicRun.stderr ?? '').includes(world.primaryKey), 'connection stderr printed the primary key');
  const created = (await api('POST', `${prefix}/keys`, { name: 'Retained subkey' }, { cas: true })).value;
  const sub = (await api('GET', `${prefix}/connection`)).value.subKeys.find(item => item.name === 'Retained subkey');
  assert(sub?.id && sub.value && sub.id !== primaryKeyId, 'subkey was not issued separately from the primary key');
  const subFile = join(world.root, 'subkey.txt');
  await writeFile(subFile, sub.value);
  remember(sub.value);
  world.clientKeys.add(sub.value);
  assertNoSecret(created, sub.value, 'subkey create');
  const live = await sendChat('retained-scope-a', 'subkey-live', subFile);
  assert(live.hits.length === 1 && live.hits[0].label === 'scope-a', 'enabled subkey did not use the scoped credential');
  await api('PATCH', `${prefix}/keys/${sub.id}`, { enabled: false }, { cas: true });
  const disabled = await sendChat('retained-scope-a', 'subkey-disabled', subFile, { success: false });
  assert(disabled.run.code !== 0 && disabled.hits.length === 0, 'disabled client key was sent');
  const regenerated = await api('POST', `${prefix}/keys/${sub.id}/regenerate`, {}, { cas: true });
  const rotated = (await api('GET', `${prefix}/connection`)).value.subKeys.find(item => item.id === sub.id);
  assert(rotated?.value && rotated.value !== sub.value, 'subkey regenerate kept the old secret');
  assert(rotated.enabled === false, 'regenerate enabled a disabled subkey');
  assertNoSecret(regenerated, rotated.value, 'subkey regenerate');
  assert(!String(regenerated.stdout ?? '').includes(sub.value) && !String(regenerated.stderr ?? '').includes(sub.value), 'subkey regenerate printed the old secret');
  const stale = await sendChat('retained-scope-a', 'subkey-old', subFile, { success: false });
  assert(stale.run.code !== 0 && stale.hits.length === 0, 'old subkey was sent');
  await writeFile(subFile, rotated.value);
  remember(rotated.value);
  world.clientKeys.add(rotated.value);
  const disabledReplacement = await sendChat('retained-scope-b', 'subkey-disabled-new', subFile, { success: false });
  assert(disabledReplacement.run.code !== 0 && disabledReplacement.hits.length === 0, 'disabled replacement subkey was sent');
  await api('PATCH', `${prefix}/keys/${sub.id}`, { enabled: true }, { cas: true });
  const enabled = (await api('GET', `${prefix}/connection`)).value.subKeys.find(item => item.id === sub.id);
  assert(enabled?.enabled === true && enabled.value === rotated.value, 'replacement subkey was not enabled through PATCH /keys/{id}');
  const replacement = await sendChat('retained-scope-b', 'subkey-new', subFile);
  assert(replacement.hits.length === 1 && replacement.hits[0].label === 'scope-b', 'replacement subkey did not follow binding scope');
  const oldPrimary = world.primaryKey;
  const oldPrimaryFile = world.gatewayKeyFile;
  const primaryRun = await api('POST', `${prefix}/keys/primary/regenerate`, {}, { cas: true });
  const next = (await api('GET', `${prefix}/connection`)).value;
  assert(next.primaryKey && next.primaryKey !== oldPrimary, 'primary regenerate kept the old key');
  assertNoSecret(primaryRun, next.primaryKey, 'primary regenerate');
  assert(!primaryRun.stdout.includes(oldPrimary) && !primaryRun.stdout.includes(next.primaryKey), 'primary regenerate printed a key');
  const retired = await sendChat('retained-scope-a', 'primary-old', oldPrimaryFile, { success: false });
  assert(retired.run.code !== 0 && retired.hits.length === 0, 'old primary key was sent');
  world.gatewayKeyFile = '';
  world.clientKeys.delete(oldPrimary);
  world.primaryKey = next.primaryKey;
  world.clientKeys.add(next.primaryKey);
  remember(next.primaryKey);
  world.gatewayKeyFile = join(world.root, 'gateway-key-rotated.txt');
  await writeFile(world.gatewayKeyFile, next.primaryKey);
  const current = await sendChat('retained-scope-a', 'primary-new', world.gatewayKeyFile);
  assert(current.hits.length === 1 && current.hits[0].label === 'scope-a' && current.hits[0].label !== 'client', 'replacement primary key exceeded binding scope');
});

stage('builtin-zen-free-singleton-control', async () => {
  const account = (await api('GET', `${prefix}/accounts/${zenAccountId}`)).value;
  assert(account?.id === zenAccountId, 'Zen Free singleton id was not the reserved account');
  assert(account.providerId === 'opencode-zen-free', `Zen Free provider was ${account.providerId}`);
  const created = await api('POST', `${prefix}/accounts`, { providerId: 'opencode-zen-free', name: 'Retained Zen duplicate', key: 'synthetic-zen-not-created' }, { cas: true, success: false });
  assert(created.code !== 0 && textOf(created).includes('Zen Free is a built-in singleton and cannot be created through the generic account API'), `Zen Free create refusal was ${textOf(created).slice(0, 300)}`);
  const removed = await api('DELETE', `${prefix}/accounts/${zenAccountId}`, {}, { cas: true, success: false });
  assert(removed.code !== 0 && textOf(removed).includes('Zen Free is a built-in singleton and cannot be deleted'), `Zen Free delete refusal was ${textOf(removed).slice(0, 300)}`);
  const patched = await api('PATCH', `${prefix}/accounts/${zenAccountId}`, { enabled: false }, { cas: true, success: false });
  assert(patched.code !== 0 && textOf(patched).includes('Zen Free settings must use the dedicated provider-settings endpoint'), `Zen Free generic mutation was ${textOf(patched).slice(0, 300)}`);
  assert((await api('GET', `${prefix}/accounts/${zenAccountId}`)).value?.id === zenAccountId, 'Zen Free generic mutation removed the singleton');
  const identities = (await api('GET', `${prefix}/accounts`)).value?.identities ?? [];
  const zenCredentials = identities.flatMap(identity => identity.credentials ?? []).filter(item => item?.legacy?.id === zenAccountId);
  assert(zenCredentials.every(item => item.credential?.hasMaterial !== true), 'Zen Free exposed credential material');
  const settings = (await api('GET', `${prefix}/providers/zen-free`)).value;
  assert(settings.accountId === zenAccountId, 'Zen Free settings account was not the singleton');
  const turnedOff = (await api('PATCH', `${prefix}/providers/zen-free`, { enabled: false }, { cas: true })).value;
  assert(turnedOff.accountId === zenAccountId && turnedOff.enabled === false, 'Zen Free dedicated settings did not disable the singleton');
  assert((await api('GET', `${prefix}/accounts/${zenAccountId}`)).value.enabled === false, 'Zen Free account stayed enabled after the dedicated disable');
  const restoredEnabled = settings.enabled !== false;
  const turnedOn = (await api('PATCH', `${prefix}/providers/zen-free`, { enabled: restoredEnabled }, { cas: true })).value;
  assert(turnedOn.enabled === restoredEnabled, 'Zen Free dedicated settings did not restore enablement');
  const models = (await api('GET', `${prefix}/providers/zen-free/models`)).value;
  assert(models.accountId === zenAccountId, 'Zen Free model snapshot account was not the singleton');
  assert(models.sourceUrl === 'https://opencode.ai/zen/v1/models', `Zen Free catalog source was ${models.sourceUrl}`);
  assertSealedHttps(models.sourceUrl, 'opencode.ai', 'Zen Free');
  limit('zen-free-catalog-refresh', {
    method: 'POST',
    route: '/dashboard/api/v4/providers/zen-free/models/refresh',
    input: { expectedRevision: 'current', processGeneration: 'current' },
    source: 'https://opencode.ai/zen/v1/models',
    reason: 'POST refresh fetches the sealed Zen catalog and the official protocol baseline. GET /providers/zen-free/models is the stored snapshot and is not that fetch.',
    retainedCapability: 'singleton refusals, dedicated enablement PATCH, and the stored source URL',
  });
});

stage('builtin-minimax-token-plan-local-control', async () => {
  const account = await createPlanAccount({ providerId: 'minimax', name: 'Retained MiniMax', key: remember('sk-cp-synthetic-retained-minimax') }, 'MiniMax');
  assert(account.providerId === 'minimax', `MiniMax provider was ${account.providerId}`);
  const destination = (await api('GET', `${prefix}/destinations`)).value.destinations.find(item => item.legacy?.id === 'minimax' || item.baseUrl === sealed.minimaxBase || (item.plan && account.providerId === 'minimax' && String(item.baseUrl ?? '').startsWith('https://api.minimax.cn')));
  assert(destination?.baseUrl, 'MiniMax destination was not listed');
  assertSealedHttps(destination.baseUrl, 'api.minimax.cn', 'MiniMax');
  assert(destination.baseUrl === sealed.minimaxBase || destination.baseUrl.startsWith(`${sealed.minimaxBase}/`) || destination.baseUrl.startsWith('https://api.minimax.cn'), 'MiniMax base URL was not the sealed origin');
  const usage = await api('PATCH', `${prefix}/accounts/${account.id}/usage`, { window: 'window_week', percent: 10 }, { cas: true, success: false });
  assert(usage.code !== 0 && textOf(usage).includes('manual usage calibration is unavailable for this account'), `MiniMax manual usage was ${textOf(usage).slice(0, 300)}`);
  assert((await api('GET', `${prefix}/accounts/${account.id}`)).value.enabled !== false, 'failed MiniMax calibration disabled the account');
  limit('minimax-token-plan-usage-refresh', {
    method: 'POST',
    route: `/dashboard/api/v4/accounts/${account.id}/usage/refresh`,
    input: { expectedRevision: 'current', processGeneration: 'current' },
    source: sealed.minimaxUsage,
    reason: 'MiniMax Token Plan usage refresh dials the sealed remains URL. Manual calibration stays rejected so quota is not stored as an estimate.',
    retainedCapability: 'local sk-cp account create, sealed base URL, and rejected manual usage PATCH',
  });
  limit('minimax-protocol-probe', {
    method: 'POST',
    route: '/dashboard/api/v4/providers/minimax/protocol-probes',
    input: { expectedRevision: 'current', processGeneration: 'current', accountId: account.id, modelId: 'catalog-model', protocols: ['chat_completions'] },
    source: sealed.minimaxBase,
    reason: 'MiniMax protocol probe is implemented and dials the sealed host. This driver does not call it.',
    retainedCapability: 'local account and sealed URL persistence',
  });
});

stage('builtin-kimi-code-api-key-local-control', async () => {
  const account = await createPlanAccount({ providerId: 'kimi', name: 'Retained Kimi Code', key: remember('sk-ki-synthetic-retained-kimi') }, 'Kimi Code');
  assert(account.providerId === 'kimi', `Kimi Code provider was ${account.providerId}`);
  const destinations = (await api('GET', `${prefix}/destinations`)).value.destinations ?? [];
  const destination = destinations.find(item => item.baseUrl === sealed.kimiBase || String(item.baseUrl ?? '').startsWith('https://api.kimi.com/coding'));
  assert(destination?.baseUrl, 'Kimi Code destination was not listed');
  assertSealedHttps(destination.baseUrl, 'api.kimi.com', 'Kimi Code');
  assert(!String(destination.baseUrl).includes('kimi.ai'), 'Kimi Code API key was stored on the native kimi.ai origin');
  const usage = await api('PATCH', `${prefix}/accounts/${account.id}/usage`, { window: 'window_week', percent: 10 }, { cas: true, success: false });
  assert(usage.code !== 0 && textOf(usage).includes('manual usage calibration is unavailable for this account'), `Kimi Code manual usage was ${textOf(usage).slice(0, 300)}`);
  assert((await api('GET', `${prefix}/accounts/${account.id}`)).value.enabled !== false, 'failed Kimi Code calibration disabled the account');
  limit('kimi-code-weekly-usage-refresh', {
    method: 'POST',
    route: `/dashboard/api/v4/accounts/${account.id}/usage/refresh`,
    input: { expectedRevision: 'current', processGeneration: 'current' },
    source: sealed.kimiUsage,
    reason: 'Kimi Code weekly usage refresh dials the sealed coding usages URL. This account is provider kimi, not native kimi.com or kimi.ai OAuth.',
    retainedCapability: 'local sk-ki account create, sealed coding base URL, and rejected manual usage PATCH',
  });
  limit('kimi-code-protocol-probe', {
    method: 'POST',
    route: '/dashboard/api/v4/providers/kimi/protocol-probes',
    input: { expectedRevision: 'current', processGeneration: 'current', accountId: account.id, modelId: 'catalog-model', protocols: ['chat_completions'] },
    source: sealed.kimiBase,
    reason: 'Kimi Code protocol probe dials the sealed coding host. This driver does not call it.',
    retainedCapability: 'local API-key account distinct from native OAuth',
  });
});

stage('builtin-ollama-local-tier-and-nonblocking-estimate', async () => {
  const account = await createPlanAccount({
    providerId: 'ollama', name: 'Retained Ollama', key: remember('synthetic-ollama-retained'), ollamaBillingTier: 'pro', purchaseDate: '2026-10-01',
  }, 'Ollama');
  const stored = (await api('GET', `${prefix}/accounts/${account.id}`)).value;
  assert(stored.providerId === 'ollama', `Ollama provider was ${stored.providerId}`);
  assert(stored.ollamaBillingTier === 'pro', `Ollama tier was ${stored.ollamaBillingTier}`);
  assert(stored.purchaseDate === '2026-10-01', `Ollama purchase date was ${stored.purchaseDate}`);
  assert(stored.enabled !== false, 'Ollama create disabled routing');
  const destinations = (await api('GET', `${prefix}/destinations`)).value.destinations ?? [];
  const destination = destinations.find(item => item.baseUrl === sealed.ollamaBase || String(item.baseUrl ?? '').startsWith('https://ollama.com'));
  assert(destination?.baseUrl, 'Ollama destination was not listed');
  assertSealedHttps(destination.baseUrl, 'ollama.com', 'Ollama');
  const week = await api('PATCH', `${prefix}/accounts/${account.id}/usage`, { window: 'window_week', percent: 10 }, { cas: true, success: false });
  assert(week.code !== 0 && textOf(week).includes('Ollama Cloud publishes only a monthly credit window'), `Ollama week calibration was ${textOf(week).slice(0, 300)}`);
  const month = await api('PATCH', `${prefix}/accounts/${account.id}/usage`, { window: 'window_month', percent: 100 }, { cas: true, success: false });
  assert(month.code === 0, `Ollama month calibration failed: ${textOf(month).slice(0, 300)}`);
  const after = (await api('GET', `${prefix}/accounts/${account.id}`)).value;
  assert(after.enabled !== false, 'Ollama monthly estimate disabled the account');
  for (const field of ['cooldownGenericUntil', 'cooldown5hUntil', 'cooldownWeekUntil', 'cooldownMonthUntil', 'cooldownFreeUntil']) {
    assert(after[field] == null, `Ollama monthly estimate wrote ${field}`);
  }
  if ((destination.catalog ?? []).length === 0) {
    limit('ollama-model-metadata', {
      method: 'PUT',
      route: `/dashboard/api/v4/destinations/${destination.id}/model-metadata`,
      input: { publicModel: 'catalog-model', metadata: { reasoning: true } },
      source: sealed.ollamaModels,
      reason: 'Ollama catalog refresh dials the sealed models URL. An empty catalog has no row for a local reasoning metadata edit.',
      retainedCapability: 'local Pro tier, purchase date, and nonblocking monthly calibration',
    });
  }
  limit('ollama-pricing-refresh', {
    method: 'POST',
    route: '/dashboard/api/v4/providers/ollama/pricing/refresh',
    input: { expectedProviderPricingRevision: 'from GET /providers/ollama/pricing' },
    source: sealed.ollamaPricing,
    reason: 'Ollama pricing refresh dials the sealed pricing page. Local tier calibration is not that fetch.',
    retainedCapability: 'local monthly soft estimate does not disable routing',
  });
});

stage('stepfun-plan-credit-configuration-zero-does-not-disable', async () => {
  const template = await findTemplate('stepfun-plan');
  const templateUrl = templateEndpoint(template);
  const endpointUrl = templateUrl || sealed.stepfunPlan;
  if (templateUrl) assertSealedHttps(templateUrl, 'api.stepfun.com', 'StepFun plan template');
  else limit('stepfun-plan-template', {
    method: 'GET',
    route: '/dashboard/api/v4/templates',
    input: { id: 'stepfun-plan' },
    source: sealed.stepfunPlan,
    reason: 'templates did not publish stepfun-plan. The sealed source URL is used and is not replaced with loopback.',
    retainedCapability: 'local credit configuration',
  });
  assert(endpointUrl.startsWith('https://api.stepfun.com/step_plan'), `StepFun plan URL was ${endpointUrl}`);
  const secret = remember('synthetic-stepfun-plan-retained');
  const refusals = world.proxyRefusals.length;
  const created = await api('POST', `${prefix}/onboarding/commit`, {
    operationId: randomUUID(),
    mode: 'complete',
    authorizeCurrentEndpoint: false,
    connection: { kind: 'new', templateId: template?.id || 'custom', name: 'Retained StepFun plan', endpointUrl, upstreamProtocol: 'chat_completions', authKind: 'bearer' },
    authorization: { kind: 'api_key', secretInput: secret, accountLabel: 'Retained StepFun plan' },
    targets: [{ publicModel: 'retained-stepfun', upstreamModel: 'stepfun-upstream' }],
  }, { cas: true, success: false });
  if (created.code !== 0) {
    limit('stepfun-plan-onboarding', {
      method: 'POST',
      route: '/dashboard/api/v4/onboarding/commit',
      input: { templateId: template?.id || 'custom', endpointUrl, mode: 'complete' },
      source: endpointUrl,
      reason: `sealed StepFun onboarding did not complete in this hermetic profile: ${textOf(created).slice(0, 240)}. A deny-proxy refusal is not a balance.`,
      retainedCapability: 'loopback zero-credit send below',
    });
  } else {
    await ensureReady('stepfun plan');
    const account = await accountByName('Retained StepFun plan');
    await api('PUT', `${prefix}/accounts/${account.id}/billing/credits`, creditBody('stepfun-upstream', 0), { cas: true });
    const billing = (await api('GET', `${prefix}/accounts/${account.id}/billing`)).value;
    const stored = (await api('GET', `${prefix}/accounts/${account.id}`)).value;
    assert(billing.credits?.remaining === 0, 'StepFun plan credit remaining was not zero');
    assert(stored.enabled !== false, 'zero StepFun plan credits disabled the account');
    assert(!world.arrivals.some(hit => hit.model === 'stepfun-upstream'), 'StepFun plan credit configuration sent inference');
    if (world.proxyRefusals.slice(refusals).includes('api.stepfun.com')) {
      limit('stepfun-plan-proxy-refusal', {
        method: 'POST',
        route: '/dashboard/api/v4/onboarding/commit',
        input: { endpointUrl },
        source: endpointUrl,
        reason: 'the deny proxy recorded api.stepfun.com. That refusal is not a successful balance or usage fetch.',
        retainedCapability: 'local zero credit persisted and the account stayed enabled',
      });
    }
  }
  const loopSecret = rememberKey('credit', 'synthetic-credit-key-retained');
  world.modelIds.push('upstream-credit');
  const onboarded = await onboard({
    mode: 'complete',
    connection: { kind: 'new', templateId: 'custom', name: 'Retained zero credit', endpointUrl: upstreamUrl(), upstreamProtocol: 'chat_completions', authKind: 'bearer' },
    authorization: { kind: 'api_key', secretInput: loopSecret, accountLabel: 'Retained zero credit' },
    targets: [{ publicModel: 'retained-credit', upstreamModel: 'upstream-credit' }],
  });
  const creditAccount = onboarded.accountId ? { id: onboarded.accountId } : await accountByName('Retained zero credit');
  await api('PUT', `${prefix}/accounts/${creditAccount.id}/billing/credits`, creditBody('upstream-credit', 0), { cas: true });
  const billing = (await api('GET', `${prefix}/accounts/${creditAccount.id}/billing`)).value;
  const stored = (await api('GET', `${prefix}/accounts/${creditAccount.id}`)).value;
  assert(billing.credits?.remaining === 0, 'loopback zero credit was not stored');
  assert(stored.enabled !== false, 'zero estimated credits disabled routing');
  world.creditAccountId = creditAccount.id;
  const sent = await sendChat('retained-credit', 'zero-credit', await gatewayKeyFile());
  assert(sent.hits.length === 1, 'zero-credit account did not send');
  assertHit(sent.hits[0], { label: 'credit', model: 'upstream-credit', authMode: 'bearer' });
  assert((await api('GET', `${prefix}/accounts/${creditAccount.id}`)).value.enabled !== false, 'zero-credit send disabled the account');
});

stage('official-balance-and-pricing-seam', async () => {
  const accounts = (await api('GET', `${prefix}/account-records`)).value?.accounts ?? [];
  const deepseek = accounts.find(account => account.providerId === 'deepseek' || account.providerId === 'zhipu');
  if (deepseek) {
    const status = await api('GET', `${prefix}/accounts/${deepseek.id}/official-api`, undefined, { success: false });
    assert(status.code === 0, `local official status failed: ${textOf(status).slice(0, 300)}`);
    limit('official-status-is-local', {
      method: 'GET',
      route: `/dashboard/api/v4/accounts/${deepseek.id}/official-api`,
      input: {},
      source: 'local projection',
      reason: 'GET official-api does not fetch and does not establish a live balance',
      retainedCapability: 'local status read only',
    });
  }
  limit('deepseek-zhipu-balance', {
    method: 'POST',
    route: '/dashboard/api/v4/accounts/{id}/official-api/balance',
    input: { expectedRevision: 'current', processGeneration: 'current' },
    source: `${sealed.deepseek} and ${sealed.zhipu}`,
    reason: 'official balance fetch is not hermetic. This driver does not create DeepSeek or Zhipu accounts and does not call the fetch.',
    retainedCapability: 'GET official-api remains the local observation when such an account already exists',
  });
  limit('deepseek-zhipu-pricing', {
    method: 'POST',
    route: '/dashboard/api/v4/providers/{id}/official-api/pricing',
    input: { expectedRevision: 'current', processGeneration: 'current' },
    source: 'official preset endpoint required by kind_for_runtime',
    reason: 'official pricing fetch is not hermetic. Other presets return: official financial evidence is unavailable for this preset or destination.',
    retainedCapability: 'no estimated substitute is written for official balance or price',
  });
  limit('go-goat-trusted-seam', {
    method: 'POST',
    route: '/dashboard/api/v4/providers/{opencode|command-code}/pricing/refresh',
    input: {},
    source: 'OCG_CPA_TEST_ENDPOINTS is owned by scripts/cli-cpa-acceptance.mjs',
    reason: 'Go and GOAT pricing, multipliers, and quota are not reimplemented here and this process does not set the test endpoint seam.',
    retainedCapability: 'existing CPA acceptance trusted-quota stage',
  });
}, { limitOnly: true });

function newApiFixture(quota) {
  return (hit) => {
    if (hit.path === '/api/status') return { body: { success: true, data: { quota_per_unit: 500000 } } };
    if (hit.path === '/api/user/self') return { body: { success: true, data: { group: 'default', quota, used_quota: 0 } } };
    if (hit.path === '/api/user/self/groups') return { body: { success: true, data: { default: {} } } };
    if (hit.path === '/api/subscription/self') return { body: { success: true, data: { subscriptions: [] } } };
    if (hit.path === '/api/token/auto-groups') return { body: { success: true, data: { groups: [] } } };
    if (hit.path === '/api/log/self/stat') return { body: { success: true, data: { quota: 0 } } };
    if (hit.path === '/api/pricing') return { body: { success: true, data: {} } };
    if (hit.path === '/api/token/' || hit.path === '/api/token') {
      return { body: { success: true, data: { items: [{ id: 7, name: 'imported-a', status: 1 }, { id: 8, name: 'disabled-b', status: 2 }], total: 2 } } };
    }
    if (hit.path === '/api/token/7/key') {
      if (hit.method !== 'POST') return { status: 405, body: { success: false } };
      return { body: { success: true, data: { key: 'sk-platform-synthetic-7' } } };
    }
    if (hit.path === '/v1/models' || hit.path === '/models') return { body: { object: 'list', data: [{ id: 'platform-model', object: 'model' }] } };
    if (hit.path === '/api/usage/token' || hit.path === '/api/usage/token/') return { body: { code: true, data: { name: 'imported-a', total_used: 1, total_available: 4, total_granted: 5, unlimited_quota: false } } };
    if (hit.path === '/v1/chat/completions') return { body: { id: 'chatcmpl-platform', object: 'chat.completion', choices: [{ index: 0, message: { role: 'assistant', content: 'platform-ok' }, finish_reason: 'stop' }], usage: { prompt_tokens: 3, completion_tokens: 2, total_tokens: 5 } } };
    return undefined;
  };
}
function sub2Fixture() {
  return (hit) => {
    if (hit.path === '/api/v1/user/profile') return { body: { code: 0, data: { id: 8, balance: 100 } } };
    if (hit.path === '/api/v1/subscriptions/summary') return { body: { code: 0, data: { subscriptions: [] } } };
    if (hit.path === '/api/v1/groups/available') return { body: { code: 0, data: [{ id: 4, name: 'composite', platform: 'composite' }] } };
    if (hit.path === '/api/v1/model-plaza') return { body: { code: 0, data: { groups: [] } } };
    if (hit.path === '/v1/models') return { body: { data: [{ id: 'sub2-model', platform: 'openai' }] } };
    if (hit.path === '/v1/usage') return { body: { mode: 'unrestricted', balance: 20 } };
    if (hit.path === '/v1/sub2api/billing') return { body: { object: 'sub2api.key_billing', schema_version: 1, billing_scope: 'token', effective_rate_multiplier: 1 } };
    if (hit.path === '/v1/chat/completions') return { body: { id: 'chatcmpl-sub2', object: 'chat.completion', choices: [{ index: 0, message: { role: 'assistant', content: 'sub2-ok' }, finish_reason: 'stop' }], usage: { prompt_tokens: 2, completion_tokens: 2, total_tokens: 4 } } };
    return undefined;
  };
}
async function platformByName(name) {
  const listed = (await api('GET', `${prefix}/platform-accounts`)).value;
  const account = (listed.accounts ?? []).find(item => item.name === name);
  assert(account?.id, `${name} platform account was not listed`);
  return { listed, account };
}
function walletRemaining(account) {
  const quotas = account?.snapshot?.quotas ?? [];
  const wallet = quotas.find(item => item?.source === 'new_api.user_self' && item?.scopeId === 'wallet');
  return wallet?.remaining;
}

stage('platform-new-api-sub2api-isolation-link-import-unlink-guarded-delete', async () => {
  const first = await startFixture(newApiFixture(6000000));
  const second = await startFixture(newApiFixture(3000000));
  const sub2 = await startFixture(sub2Fixture());
  world.platformServers = [first.server, second.server, sub2.server];
  remember('synthetic-new-api-user');
  remember('synthetic-new-api-user-b');
  remember('synthetic-sub2-user');
  remember('sk-platform-synthetic-7');
  remember('sk-sub2-synthetic-key');
  await api('POST', `${prefix}/platform-accounts`, { kind: 'new_api', name: 'Retained New API A', baseUrl: first.origin(), userCredential: '17:synthetic-new-api-user' }, { cas: true });
  await api('POST', `${prefix}/platform-accounts`, { kind: 'new_api', name: 'Retained New API B', baseUrl: second.origin(), userCredential: '18:synthetic-new-api-user-b' }, { cas: true });
  await api('POST', `${prefix}/platform-accounts`, { kind: 'sub2api', name: 'Retained Sub2API', baseUrl: sub2.origin(), userCredential: 'synthetic-sub2-user' }, { cas: true });
  const parentA = (await platformByName('Retained New API A')).account;
  const parentB = (await platformByName('Retained New API B')).account;
  assert(parentA.baseUrl !== parentB.baseUrl, 'platform instances shared a base URL');
  const parentMark = first.hits.length;
  await api('POST', `${prefix}/platform-accounts/${parentA.id}/refresh`, {}, { cas: true });
  const parentHits = first.hits.slice(parentMark);
  assert(parentHits.some(hit => hit.path === '/api/user/self' && hit.newApiUser === '17'), 'New API parent refresh did not use the numeric user id');
  assert(!parentHits.some(hit => hit.path === '/v1/models' || hit.path.startsWith('/api/usage/token')), 'New API parent refresh read key usage');
  assert(second.hits.every(hit => hit.path !== '/api/user/self'), 'New API A refresh read instance B');
  const refreshedA = (await platformByName('Retained New API A')).account;
  const refreshedB = (await platformByName('Retained New API B')).account;
  await api('POST', `${prefix}/platform-accounts/${parentB.id}/refresh`, {}, { cas: true });
  const isolatedB = (await platformByName('Retained New API B')).account;
  assert(walletRemaining(refreshedA) === 12, `New API A wallet remaining was ${walletRemaining(refreshedA)}`);
  assert(walletRemaining(isolatedB) === 6, `New API B wallet remaining was ${walletRemaining(isolatedB)}`);
  assert(walletRemaining(refreshedB) == null || walletRemaining(refreshedB) !== walletRemaining(refreshedA), 'New API wallets were not isolated');
  await api('PUT', `${prefix}/platform-accounts/${parentB.id}`, { name: 'Retained New API B renamed' }, { cas: true });
  assert((await platformByName('Retained New API A')).account.name === 'Retained New API A', 'editing instance B renamed instance A');
  const imported = await api('POST', `${prefix}/platform-accounts/${parentA.id}/import-keys`, { page: 1 }, { cas: true });
  assert(imported.value?.imported === 1, `New API import count was ${imported.value?.imported}`);
  assert(imported.value?.skippedDisabled === 1, `New API skipped disabled count was ${imported.value?.skippedDisabled}`);
  assert(!rawCaptured(imported).includes('sk-platform-synthetic-7'), 'key import printed the remote key');
  assert(first.hits.some(hit => hit.method === 'POST' && hit.path === '/api/token/7/key'), 'New API import did not POST the full key');
  assert(!first.hits.some(hit => hit.method === 'GET' && hit.path === '/api/token/7/key' && hit.authorization.includes('sk-platform-synthetic-7')), 'New API import treated GET key as success');
  const importedAccount = await accountByName('imported-a');
  const linkMark = first.hits.length;
  const group = (refreshedA.snapshot?.groups ?? []).find(item => item?.id === 'default') ?? { id: 'default', autoGroups: [], verified: true };
  await api('PUT', `${prefix}/accounts/${importedAccount.id}/platform-link`, {
    platformAccountId: parentA.id,
    group: { id: group.id ?? 'default', platform: group.platform ?? null, subscriptionType: group.subscriptionType ?? null, autoGroups: group.autoGroups ?? [], verified: group.verified === true },
  }, { cas: true });
  assert(first.hits.length === linkMark, 'platform link fetched the site');
  const keyMark = first.hits.length;
  await api('POST', `${prefix}/platform-accounts/${parentA.id}/refresh`, { accountId: importedAccount.id }, { cas: true });
  const keyHits = first.hits.slice(keyMark);
  assert(keyHits.some(hit => hit.path === '/v1/models'), 'linked key refresh did not discover models');
  assert(keyHits.some(hit => hit.path.startsWith('/api/usage/token')), 'linked key refresh did not read key usage');
  assert(!keyHits.some(hit => hit.path === '/api/user/self'), 'linked key refresh read the site wallet');
  const chatMark = first.hits.length;
  const chat = await api('POST', '/v1/chat/completions', { model: 'platform-model', messages: [{ role: 'user', content: 'platform' }] }, { extra: ['--key-file', await gatewayKeyFile()], success: false });
  assert(chat.code === 0, `imported key send failed: ${textOf(chat).slice(0, 300)}`);
  const chatHits = first.hits.slice(chatMark).filter(hit => hit.path === '/v1/chat/completions');
  assert(chatHits.length === 1, `imported key send count was ${chatHits.length}`);
  assert(chatHits[0].authorization === 'Bearer sk-platform-synthetic-7', 'imported key send did not use the imported secret');
  assert(!chatHits[0].authorization.includes('synthetic-new-api-user'), 'imported key send used the parent user credential');
  assert(second.hits.every(hit => hit.path !== '/v1/chat/completions'), 'instance A inference reached instance B');
  const otherMark = first.hits.filter(hit => hit.path === '/v1/chat/completions').length;
  const unscoped = await api('POST', '/v1/chat/completions', { model: 'platform-unscoped', messages: [{ role: 'user', content: 'unscoped' }] }, { extra: ['--key-file', world.gatewayKeyFile], success: false });
  assert(unscoped.code !== 0, 'unscoped platform model was accepted');
  assert(first.hits.filter(hit => hit.path === '/v1/chat/completions').length === otherMark, 'unscoped platform model was sent');
  const guarded = await api('DELETE', `${prefix}/platform-accounts/${parentA.id}`, {}, { cas: true, success: false });
  assert(guarded.code !== 0 && textOf(guarded).includes('unlink Keys before deleting the platform account'), `guarded parent delete was ${textOf(guarded).slice(0, 300)}`);
  assert((await accountByName('imported-a')).id === importedAccount.id, 'guarded parent delete removed the key');
  await api('DELETE', `${prefix}/accounts/${importedAccount.id}/platform-link`, {}, { cas: true });
  await api('DELETE', `${prefix}/platform-accounts/${parentA.id}`, {}, { cas: true });
  const afterDelete = (await api('GET', `${prefix}/platform-accounts`)).value.accounts ?? [];
  assert(!afterDelete.some(item => item.id === parentA.id), 'unlinked parent was not deleted');
  assert(afterDelete.some(item => item.id === parentB.id), 'deleting parent A deleted parent B');
  assert((await accountByName('imported-a')).id === importedAccount.id, 'parent delete removed the unlinked key');
  const subParent = (await platformByName('Retained Sub2API')).account;
  const subImport = await api('POST', `${prefix}/platform-accounts/${subParent.id}/import-keys`, { page: 1 }, { cas: true, success: false });
  assert(subImport.code !== 0 && textOf(subImport).includes('key import is only available for New API'), `Sub2API import refusal was ${textOf(subImport).slice(0, 300)}`);
  const subSecret = rememberKey('sub2', 'sk-sub2-synthetic-key');
  const subOnboard = await onboard({
    mode: 'complete',
    connection: { kind: 'new', templateId: 'custom', name: 'Retained Sub2 key', endpointUrl: `${sub2.origin()}/v1`, upstreamProtocol: 'chat_completions', authKind: 'bearer' },
    authorization: { kind: 'api_key', secretInput: subSecret, accountLabel: 'Retained Sub2 key' },
    targets: [{ publicModel: 'retained-sub2', upstreamModel: 'sub2-model' }],
  });
  const subAccount = subOnboard.accountId ? { id: subOnboard.accountId } : await accountByName('Retained Sub2 key');
  const subLinkMark = sub2.hits.length;
  await api('PUT', `${prefix}/accounts/${subAccount.id}/platform-link`, {
    platformAccountId: subParent.id,
    group: { id: '4', platform: 'composite', subscriptionType: null, autoGroups: [], verified: true },
  }, { cas: true });
  assert(sub2.hits.length === subLinkMark, 'Sub2API link fetched the site');
  const subParentMark = sub2.hits.length;
  await api('POST', `${prefix}/platform-accounts/${subParent.id}/refresh`, {}, { cas: true });
  const subParentHits = sub2.hits.slice(subParentMark);
  assert(subParentHits.some(hit => hit.path === '/api/v1/user/profile'), 'Sub2API parent refresh missed the user profile');
  assert(!subParentHits.some(hit => hit.path === '/v1/usage'), 'Sub2API parent refresh read key usage');
  const subKeyMark = sub2.hits.length;
  await api('POST', `${prefix}/platform-accounts/${subParent.id}/refresh`, { accountId: subAccount.id }, { cas: true });
  const subKeyHits = sub2.hits.slice(subKeyMark);
  assert(subKeyHits.some(hit => hit.path === '/v1/usage' && hit.authorization === 'Bearer sk-sub2-synthetic-key'), 'Sub2API key refresh did not use the key bearer');
  assert(!subKeyHits.some(hit => hit.path === '/api/v1/user/profile'), 'Sub2API key refresh read the parent profile');
  const subGuarded = await api('DELETE', `${prefix}/platform-accounts/${subParent.id}`, {}, { cas: true, success: false });
  assert(subGuarded.code !== 0 && textOf(subGuarded).includes('unlink Keys before deleting the platform account'), 'linked Sub2API parent delete succeeded');
  await api('DELETE', `${prefix}/accounts/${subAccount.id}/platform-link`, {}, { cas: true });
  await api('DELETE', `${prefix}/platform-accounts/${subParent.id}`, {}, { cas: true });
  assert((await accountByName('Retained Sub2 key')).id === subAccount.id, 'Sub2API parent delete removed the key');
});

function byokPath(parts) { return join(world.root, 'home', ...parts); }
function unescapeTomlBasic(text) {
  let out = '';
  for (let index = 0; index < text.length; index += 1) {
    if (text[index] !== '\\' || index + 1 >= text.length) {
      out += text[index];
      continue;
    }
    const next = text[index + 1];
    if (next === '\\' || next === '"') out += next;
    else if (next === 'n') out += '\n';
    else if (next === 't') out += '\t';
    else out += next;
    index += 1;
  }
  return out;
}
function tomlAssignment(text, key) {
  const match = String(text).match(new RegExp(`^\\s*${key}\\s*=\\s*(.*)$`, 'm'));
  if (!match) return undefined;
  const raw = match[1].trim();
  if (raw.startsWith('"')) {
    const closed = raw.match(/^"((?:\\.|[^"\\])*)"/);
    return closed ? unescapeTomlBasic(closed[1]) : undefined;
  }
  if (raw.startsWith("'")) {
    const closed = raw.match(/^'([^']*)'/);
    return closed ? closed[1] : undefined;
  }
  const bare = raw.replace(/\s+#.*$/, '').trim();
  if (bare === 'true') return true;
  if (bare === 'false') return false;
  if (/^-?\d+$/.test(bare)) return Number(bare);
  return bare;
}
function yamlTree(text) {
  const root = {};
  const stack = [{ indent: -1, node: root }];
  for (const line of String(text).split(/\r?\n/)) {
    if (!line.trim() || /^\s*#/.test(line)) continue;
    const match = line.match(/^(\s*)([A-Za-z0-9_]+):\s*(.*)$/);
    if (!match) continue;
    const indent = match[1].length;
    const key = match[2];
    let raw = match[3].trim().replace(/\s+#.*$/, '');
    while (stack.length > 1 && indent <= stack[stack.length - 1].indent) stack.pop();
    const parent = stack[stack.length - 1].node;
    if (!raw) {
      const child = {};
      parent[key] = child;
      stack.push({ indent, node: child });
    } else {
      if ((raw.startsWith('"') && raw.endsWith('"')) || (raw.startsWith("'") && raw.endsWith("'"))) raw = raw.slice(1, -1);
      parent[key] = raw;
    }
  }
  return root;
}
function samePath(left, right) {
  if (typeof left !== 'string' || !left || typeof right !== 'string' || !right) return false;
  return resolve(left).toLowerCase() === resolve(right).toLowerCase();
}
function endpointIn(value, endpoint) {
  if (typeof value === 'string') return value.includes(endpoint);
  if (Array.isArray(value)) return value.some(item => endpointIn(item, endpoint));
  if (value && typeof value === 'object') return Object.values(value).some(item => endpointIn(item, endpoint));
  return false;
}
function zcodeSeed() {
  return `${JSON.stringify({
    schemaVersion: 1,
    extraUser: true,
    config: {
      providerOrder: ['keep'],
      providerConfigRules: { providerRules: [{ providerId: 'keep', providerName: 'Keep', enabled: true, config: { group: 'standard-personal' } }] },
      modelConfigRules: { providerModelRules: [], manualProviderModelRules: [] },
      defaultModelSelection: { providerId: 'keep', modelId: 'keep-model' },
    },
  })}\n`;
}
function assertZcodePreserved(text, phase) {
  const parsed = JSON.parse(text);
  assert(parsed.schemaVersion === 1, `zcode ${phase} schemaVersion changed`);
  assert(parsed.extraUser === true, `zcode ${phase} dropped extraUser`);
  const rules = parsed.config?.providerConfigRules?.providerRules ?? [];
  const keep = rules.find(rule => rule.providerId === 'keep');
  assert(keep?.enabled === true && keep.config?.group === 'standard-personal', `zcode ${phase} dropped the keep provider rule`);
  assert((parsed.config?.providerOrder ?? []).includes('keep'), `zcode ${phase} dropped keep from providerOrder`);
  const manual = parsed.config?.modelConfigRules?.manualProviderModelRules;
  assert(Array.isArray(manual) && manual.length === 0, `zcode ${phase} changed manualProviderModelRules`);
  const ocg = rules.find(rule => rule.providerId === 'ocg');
  const selection = parsed.config?.defaultModelSelection;
  if (phase === 'configured') {
    assert(ocg?.enabled === true, 'zcode configure did not add the ocg provider');
    assert(typeof ocg?.config?.api?.baseUrl === 'string' && ocg.config.api.baseUrl.includes(world.endpoint), 'zcode configure did not record the gateway endpoint');
    assert((parsed.config?.providerOrder ?? []).includes('ocg'), 'zcode configure did not append ocg to providerOrder');
    assert(selection?.providerId === 'ocg' && typeof selection.modelId === 'string' && selection.modelId.length > 0, 'zcode configure did not select a published model');
  }
  if (phase === 'restored') {
    assert(!ocg, 'zcode remove left the ocg provider');
    assert(!(parsed.config?.providerOrder ?? []).includes('ocg'), 'zcode remove left ocg in providerOrder');
    assert(!endpointIn(parsed, world.endpoint), 'zcode remove left the gateway endpoint');
    assert(selection?.providerId === 'keep' && selection.modelId === 'keep-model', 'zcode remove did not restore the original selection');
  }
}
async function absentFile(path) {
  try {
    await stat(path);
    return false;
  } catch (error) {
    if (error?.code === 'ENOENT') return true;
    throw error;
  }
}

stage('byok-conflict-preservation-and-restore', async () => {
  limit('byok-serialization-comments', {
    method: 'POST',
    route: '/dashboard/api/v4/applications/byok/{codex|kimi|minimax|zcode}',
    input: { expectedFingerprint: 'inspection', clientClosed: true, targetPath: 'synthetic home file' },
    source: 'Codex and Kimi encode through toml_edit DocumentMut::to_string. MiniMax encodes through serde_yaml dump. ZCode encodes through serde_json::to_vec_pretty.',
    reason: 'Comment lines such as # keep-me-comment are not preservation proof, and YAML or JSON bytes are not required to match the seed. Compared facts are parsed assignments, JSON values, the original selection, and the Codex companion catalog.',
    retainedCapability: 'count, unrelated, original managed selection, provider rules, preferences, and the Codex user catalog bytes',
  });
  const userCatalog = byokPath(['.codex', 'keep-catalog.json']);
  const userCatalogBytes = Buffer.from('{"models":[{"slug":"keep-model"}]}\n');
  const ocgCatalog = byokPath(['.codex', '.ocg-byok', 'model_catalog.json']);
  await mkdir(dirname(userCatalog), { recursive: true });
  await writeFile(userCatalog, userCatalogBytes);
  const clients = [
    ['codex', ['.codex', 'config.toml'], `# keep-me-comment\nmodel = "keep-model"\nmodel_provider = "keep"\nmodel_catalog_json = ${JSON.stringify(userCatalog)}\ncount = 7\n\n[other]\nunrelated = "yes"\n`],
    ['kimi', ['.kimi-code', 'config.toml'], '# keep-me-comment\ndefault_model = "keep/model"\ncount = 7\n\n[other]\nunrelated = "yes"\n'],
    ['minimax', ['.minimax', 'config.yaml'], 'logLevel: debug\ndefaultModel: keep-model\ndefaultModelThinking: low\nprovider:\n  minimax:\n    name: official\n'],
    ['zcode', ['.zcode', 'v2', 'provider_config.json'], zcodeSeed()],
  ];
  const unsupported = [];
  for (const [client, parts, initial] of clients) {
    const file = byokPath(parts);
    await mkdir(dirname(file), { recursive: true });
    await writeFile(file, initial);
    const view = (await api('GET', `${prefix}/applications/byok/${client}?targetPath=${encodeURIComponent(file)}`)).value;
    if (view.configureSupported !== true) {
      unsupported.push(client);
      const refused = await api('POST', `${prefix}/applications/byok/${client}`, { expectedFingerprint: view.fingerprint ?? 'absent', clientClosed: true, targetPath: file }, { cas: true, success: false });
      assert(refused.code !== 0 && textOf(refused).includes('Native application configuration is unavailable on this host'), `${client} feature-off refusal was ${textOf(refused).slice(0, 300)}`);
      assert((await readFile(file, 'utf8')) === initial, `${client} feature-off changed the synthetic file`);
      continue;
    }
    assert(view.fingerprint, `${client} inspection has no fingerprint`);
    if (client === 'codex') {
      assert(await absentFile(ocgCatalog), 'codex seed created the OCG companion catalog');
      await writeFile(file, `${initial}# stale\n`);
      const conflict = await api('POST', `${prefix}/applications/byok/${client}`, { expectedFingerprint: view.fingerprint, clientClosed: true, targetPath: file }, { cas: true, success: false });
      assert(conflict.code !== 0 && textOf(conflict).includes('Configuration changed; refresh and retry'), `stale fingerprint conflict was ${textOf(conflict).slice(0, 300)}`);
      assert((await readFile(file, 'utf8')).includes('# stale'), 'stale fingerprint conflict rewrote the file');
      await writeFile(file, initial);
    }
    const current = (await api('GET', `${prefix}/applications/byok/${client}?targetPath=${encodeURIComponent(file)}`)).value;
    const configured = await api('POST', `${prefix}/applications/byok/${client}`, { expectedFingerprint: current.fingerprint, clientClosed: true, targetPath: file }, { cas: true, success: false });
    assert(configured.code === 0 && configured.value?.status === 'configured', `${client} configure failed: ${textOf(configured).slice(0, 300)}`);
    const written = await readFile(file, 'utf8');
    if (client === 'codex') {
      assert(tomlAssignment(written, 'count') === 7, 'codex configure dropped count');
      assert(tomlAssignment(written, 'unrelated') === 'yes', 'codex configure dropped unrelated');
      assert(tomlAssignment(written, 'model_provider') === 'ocg', 'codex configure did not select ocg');
      const model = tomlAssignment(written, 'model');
      assert(typeof model === 'string' && model.length > 0 && model !== 'keep-model', 'codex configure kept the original model');
      assert(samePath(tomlAssignment(written, 'model_catalog_json'), ocgCatalog), 'codex configure did not point at the OCG companion catalog');
      const catalog = JSON.parse(await readFile(ocgCatalog, 'utf8'));
      assert(Array.isArray(catalog.models) && catalog.models.length > 0 && catalog.models.every(item => typeof item?.slug === 'string' && item.slug), 'codex companion catalog has no model slugs');
    } else if (client === 'kimi') {
      const selected = tomlAssignment(written, 'default_model');
      assert(typeof selected === 'string' && selected.startsWith('ocg/') && selected !== 'keep/model', 'kimi configure did not select an ocg model');
      assert(tomlAssignment(written, 'count') === 7, 'kimi configure dropped count');
      assert(tomlAssignment(written, 'unrelated') === 'yes', 'kimi configure dropped unrelated');
    } else if (client === 'minimax') {
      const tree = yamlTree(written);
      assert(tree.logLevel === 'debug', 'minimax configure dropped logLevel');
      assert(tree.provider?.minimax?.name === 'official', 'minimax configure dropped the official provider name');
      assert(typeof tree.defaultModel === 'string' && tree.defaultModel.startsWith('custom_provider:ocg/') && tree.defaultModel !== 'keep-model', 'minimax configure did not select the ocg custom provider');
    } else if (client === 'zcode') {
      assertZcodePreserved(written, 'configured');
    }
    assert(written.includes(world.endpoint), `${client} configure did not record the gateway endpoint`);
    const removed = await api('DELETE', `${prefix}/applications/byok/${client}`, { expectedFingerprint: configured.value.fingerprint, clientClosed: true, targetPath: file }, { cas: true, success: false });
    assert(removed.code === 0, `${client} remove failed: ${textOf(removed).slice(0, 300)}`);
    const restored = await readFile(file, 'utf8');
    if (client === 'codex') {
      assert(tomlAssignment(restored, 'model') === 'keep-model', 'codex remove did not restore model');
      assert(tomlAssignment(restored, 'model_provider') === 'keep', 'codex remove did not restore model_provider');
      assert(samePath(tomlAssignment(restored, 'model_catalog_json'), userCatalog), 'codex remove did not restore the user catalog pointer');
      assert(tomlAssignment(restored, 'count') === 7, 'codex remove dropped count');
      assert(tomlAssignment(restored, 'unrelated') === 'yes', 'codex remove dropped unrelated');
      assert(Buffer.compare(await readFile(userCatalog), userCatalogBytes) === 0, 'codex remove changed the user catalog');
      assert(await absentFile(ocgCatalog), 'codex remove left the OCG companion catalog');
    } else if (client === 'kimi') {
      assert(tomlAssignment(restored, 'default_model') === 'keep/model', 'kimi remove did not restore default_model');
      assert(tomlAssignment(restored, 'count') === 7, 'kimi remove dropped count');
      assert(tomlAssignment(restored, 'unrelated') === 'yes', 'kimi remove dropped unrelated');
    } else if (client === 'minimax') {
      const tree = yamlTree(restored);
      assert(tree.logLevel === 'debug', 'minimax remove dropped logLevel');
      assert(tree.defaultModel === 'keep-model', 'minimax remove did not restore defaultModel');
      assert(tree.defaultModelThinking === 'low', 'minimax remove did not restore defaultModelThinking');
      assert(tree.provider?.minimax?.name === 'official', 'minimax remove dropped the official provider name');
      assert(!tree.custom_provider?.ocg, 'minimax remove left the ocg provider');
    } else if (client === 'zcode') {
      assertZcodePreserved(restored, 'restored');
    }
    assert(!restored.includes(world.endpoint), `${client} remove left the gateway endpoint`);
  }
  if (unsupported.length > 0) {
    limit('byok-configure-host', {
      method: 'POST',
      route: '/dashboard/api/v4/applications/byok/{codex|kimi|minimax|zcode}',
      input: { expectedFingerprint: 'inspection', clientClosed: true, targetPath: 'synthetic home file' },
      source: 'byok application host',
      reason: `GET configureSupported is false for ${unsupported.join(', ')}. Those writes return 412 Native application configuration is unavailable on this host. Configure, conflict, and remove are claimed only for clients whose inspection says configureSupported.`,
      retainedCapability: 'feature-off refusal and untouched synthetic files for the unsupported clients',
    });
  }
});

function sha256Bytes(bytes) {
  return createHash('sha256').update(bytes).digest('hex');
}
function assertInside(parent, child, label) {
  assert(typeof child === 'string' && child.length > 0, `${label} path is absent`);
  assert(within(parent, child), `${label} resolved outside the owned fixture`);
}
async function optionalBytes(path) {
  try {
    return await readFile(path);
  } catch (error) {
    if (error?.code === 'ENOENT') return null;
    throw error;
  }
}
async function byokContentHash(path) {
  const bytes = await optionalBytes(path);
  return bytes === null ? 'absent' : sha256Bytes(bytes);
}
async function inspectCodex(targetPath) {
  return (await api('GET', `${prefix}/applications/byok/codex?targetPath=${encodeURIComponent(targetPath)}`)).value;
}
function codexStoreDir(configPath) {
  return join(world.dataDir, 'applications', 'byok', 'codex', sha256Bytes(Buffer.from(configPath, 'utf8')));
}

async function assertSeededCodexRecovery(preservedFile, preservedBytes) {
  const target = byokPath(['recovery-codex', 'config.toml']);
  const catalog = join(dirname(target), '.ocg-byok', 'model_catalog.json');
  const unrelated = join(dirname(target), 'unrelated.txt');
  const unrelatedBytes = Buffer.from('retained-recovery-unrelated\n');
  const original = Buffer.from('model = "keep-model"\nmodel_provider = "keep"\ncount = 7\n\n[other]\nunrelated = "yes"\n');
  assertInside(world.root, target, 'recovery target');
  assertInside(world.root, catalog, 'recovery catalog');
  assertInside(world.root, unrelated, 'recovery unrelated file');
  assert(resolve(target).toLowerCase() !== resolve(preservedFile).toLowerCase(), 'recovery target reused the clean Codex file');
  await mkdir(dirname(target), { recursive: true });
  await writeFile(target, original);
  await writeFile(unrelated, unrelatedBytes);
  assert(await absentFile(catalog), 'recovery seed created the OCG companion catalog');
  const seeded = await inspectCodex(target);
  assert(seeded.configureSupported === true, 'supported recovery inspect lost configureSupported');
  assert(typeof seeded.configPath === 'string' && seeded.configPath.length > 0, 'recovery inspect returned no configPath');
  assertInside(world.root, seeded.configPath, 'inspected recovery target');
  const configPath = seeded.configPath;
  const resolvedCatalog = join(dirname(configPath), '.ocg-byok', 'model_catalog.json');
  assertInside(world.root, resolvedCatalog, 'resolved recovery catalog');
  const storeDir = codexStoreDir(configPath);
  assertInside(world.dataDir, storeDir, 'recovery receipt store');
  const receiptPath = join(storeDir, 'receipt.json');
  const journalPath = join(storeDir, 'journal.json');
  assertInside(storeDir, receiptPath, 'recovery receipt');
  assertInside(storeDir, journalPath, 'recovery journal');
  assert(await absentFile(receiptPath), 'recovery seed already had a receipt');
  assert(await absentFile(journalPath), 'recovery seed already had a journal');
  const configured = await api('POST', `${prefix}/applications/byok/codex`, {
    expectedFingerprint: seeded.fingerprint, clientClosed: true, targetPath: configPath,
  }, { cas: true, success: false });
  assert(configured.code === 0 && configured.value?.status === 'configured', `recovery configure failed: ${textOf(configured).slice(0, 300)}`);
  const generatedTarget = await readFile(configPath);
  const generatedCatalog = await readFile(resolvedCatalog);
  assert(Buffer.compare(generatedTarget, original) !== 0, 'configure left the original Codex bytes');
  assert(await absentFile(journalPath), 'configure left the journal in the after-publish window');
  const receiptBytes = await readFile(receiptPath);
  const backup = (side, role) => join(storeDir, 'backup', `${side}-${role}.bin`);
  for (const path of [backup('old', 'target'), backup('new', 'target'), backup('new', 'catalog')]) {
    assertInside(storeDir, path, 'recovery backup');
  }
  assertInside(storeDir, backup('old', 'catalog'), 'recovery old catalog backup');
  const targetOld = await byokContentHash(backup('old', 'target'));
  const targetNew = await byokContentHash(backup('new', 'target'));
  const catalogOld = await byokContentHash(backup('old', 'catalog'));
  const catalogNew = await byokContentHash(backup('new', 'catalog'));
  assert(targetOld === sha256Bytes(original), 'old target backup is not the seeded original');
  assert(targetNew === sha256Bytes(generatedTarget), 'new target backup is not the configured file');
  assert(catalogOld === 'absent', 'first-adoption catalog backup was not absent');
  assert(catalogNew === sha256Bytes(generatedCatalog), 'new catalog backup is not the configured catalog');
  const newTargetBytes = await readFile(backup('new', 'target'));
  const originTarget = join(storeDir, 'origin', 'target.bin');
  const originCatalog = join(storeDir, 'origin', 'catalog.bin');
  assertInside(storeDir, originTarget, 'recovery origin target');
  assertInside(storeDir, originCatalog, 'recovery origin catalog');
  const originTargetBytes = await optionalBytes(originTarget);
  const originCatalogBytes = await optionalBytes(originCatalog);
  const journalBytes = Buffer.from(`${JSON.stringify({
    prior_receipt: null,
    kind: 'configure',
    files: [
      { role: 'target', old_hash: targetOld, new_hash: targetNew },
      { role: 'catalog', old_hash: catalogOld, new_hash: catalogNew },
    ],
  }, null, 2)}\n`);
  await writeFile(journalPath, journalBytes);
  const pending = await inspectCodex(configPath);
  assert(pending.status === 'recovery_required', `seeded journal status was ${pending.status}`);
  assert(pending.recoverySupported === true, 'seeded journal did not report recoverySupported');
  assert(pending.fingerprint && pending.fingerprint !== seeded.fingerprint, 'seeded journal did not change the inspection fingerprint');
  const third = Buffer.from('model = "user-edit"\nmodel_provider = "user"\ncount = 9\n');
  const thirdHash = sha256Bytes(third);
  assert(thirdHash !== targetOld && thirdHash !== targetNew, 'third state matched a journal hash');
  await writeFile(configPath, third);
  const fresh = await inspectCodex(configPath);
  assert(fresh.status === 'recovery_required' && fresh.recoverySupported === true, 'user-edited journal lost recovery_required');
  assert(fresh.fingerprint && fresh.fingerprint !== pending.fingerprint, 'user edit did not produce a fresh fingerprint');
  const refused = await api('POST', `${prefix}/applications/byok/codex/recover`, {
    expectedFingerprint: fresh.fingerprint, clientClosed: true, targetPath: configPath,
  }, { cas: true, success: false });
  assert(refused.code !== 0 && textOf(refused).includes('Current files changed after the interrupted write; recovery refused'), `user-edit recover was ${textOf(refused).slice(0, 300)}`);
  assert(!textOf(refused).includes('Configuration changed; refresh and retry'), 'user-edit recover failed the fingerprint check');
  assert(Buffer.compare(await readFile(configPath), third) === 0, 'refused recover changed the third-state target');
  assert(Buffer.compare(await readFile(resolvedCatalog), generatedCatalog) === 0, 'refused recover changed the catalog');
  assert(Buffer.compare(await readFile(receiptPath), receiptBytes) === 0, 'refused recover changed the receipt');
  assert(Buffer.compare(await readFile(journalPath), journalBytes) === 0, 'refused recover changed the journal');
  await writeFile(configPath, newTargetBytes);
  assert(Buffer.compare(await readFile(resolvedCatalog), generatedCatalog) === 0, 'target reset changed the catalog');
  assert(Buffer.compare(await readFile(receiptPath), receiptBytes) === 0, 'target reset changed the receipt');
  assert(Buffer.compare(await readFile(journalPath), journalBytes) === 0, 'target reset changed the journal');
  const armed = await inspectCodex(configPath);
  assert(armed.status === 'recovery_required' && armed.recoverySupported === true && armed.fingerprint, 'reset target lost recovery_required');
  const restored = await api('POST', `${prefix}/applications/byok/codex/recover`, {
    expectedFingerprint: armed.fingerprint, clientClosed: true, targetPath: configPath,
  }, { cas: true, success: false });
  assert(restored.code === 0 && restored.value?.status === 'ready', `seeded recover failed: ${textOf(restored).slice(0, 300)}`);
  assert(Buffer.compare(await readFile(configPath), original) === 0, 'recover did not restore the original target bytes');
  assert(Buffer.compare(await readFile(backup('old', 'target')), original) === 0, 'recover rewrote the old target backup');
  assert(await absentFile(resolvedCatalog), 'recover left the first-created catalog');
  assert(await absentFile(receiptPath), 'recover left the first-adoption receipt');
  assert(await absentFile(journalPath), 'recover left the journal');
  const ready = await inspectCodex(configPath);
  assert(ready.status === 'ready', `restored inspection status was ${ready.status}`);
  assert(ready.recoverySupported === false && ready.configureSupported === true && ready.detected === true, 'restored inspection did not return ready ownership');
  assert(Array.isArray(ready.configuredModelIds) && ready.configuredModelIds.length === 0, 'restored inspection kept configured models');
  assert(ready.removeSupported === false, 'restored inspection still reported an owned configuration');
  const originAfter = await optionalBytes(originTarget);
  const originCatalogAfter = await optionalBytes(originCatalog);
  assert(Buffer.compare(originTargetBytes ?? Buffer.alloc(0), originAfter ?? Buffer.alloc(0)) === 0 && (originTargetBytes === null) === (originAfter === null), 'recover changed the origin target backup');
  assert(Buffer.compare(originCatalogBytes ?? Buffer.alloc(0), originCatalogAfter ?? Buffer.alloc(0)) === 0 && (originCatalogBytes === null) === (originCatalogAfter === null), 'recover changed the origin catalog backup');
  assert(Buffer.compare(await readFile(unrelated), unrelatedBytes) === 0, 'recover changed the unrelated fixture file');
  assert(Buffer.compare(await readFile(preservedFile), preservedBytes) === 0, 'recover changed the clean Codex file');
}

stage('byok-interrupted-recovery-gap', async () => {
  const file = byokPath(['.codex', 'config.toml']);
  await mkdir(dirname(file), { recursive: true });
  const before = await readFile(file, 'utf8').catch(() => '');
  if (!before) await writeFile(file, '# keep-me-comment\n[other]\nunrelated = "yes"\ncount = 7\n');
  const bytes = await readFile(file);
  const view = await inspectCodex(file);
  const recovered = await api('POST', `${prefix}/applications/byok/codex/recover`, {
    expectedFingerprint: view.fingerprint ?? 'absent', clientClosed: true, targetPath: file,
  }, { cas: true, success: false });
  assert(Buffer.compare(await readFile(file), bytes) === 0, 'recover without a journal changed the synthetic file');
  let seeded = false;
  if (view.configureSupported !== true) {
    assert(recovered.code !== 0 && textOf(recovered).includes('Native application configuration is unavailable on this host'), `feature-off recover was ${textOf(recovered).slice(0, 300)}`);
  } else {
    assert(recovered.code !== 0 && textOf(recovered).includes('No interrupted BYOK write to recover'), `clean recover was ${textOf(recovered).slice(0, 300)}`);
    await assertSeededCodexRecovery(file, bytes);
    seeded = true;
  }
  limit('byok-crash-journal', {
    method: 'POST',
    route: '/dashboard/api/v4/applications/byok/codex/recover',
    input: { expectedFingerprint: 'current file fingerprint', clientClosed: true, targetPath: 'synthetic Codex config.toml' },
    source: 'Store::apply writes backups and the journal, publishes receipt.json, then clears journal.json. recover_journal restores prior_receipt null by deleting the first-adoption receipt and created catalog.',
    reason: seeded
      ? 'The reintroduced journal uses the host receipt and backup hashes for the Codex after-publish-before-clear window only. It is not a process kill, power loss, or a result for every client. The clean no-journal refusal above remains.'
      : 'Clean recover returned the feature-off 412. The seeded after-publish window was not exercised because configureSupported is false.',
    retainedCapability: seeded
      ? 'clean no-journal refusal, user-edit conflict with unchanged bytes, then original restoration and removal of the created catalog, receipt, and journal'
      : 'clean feature-off 412 and unchanged synthetic file',
  });
});

function scopeView(scope) {
  if (!scope || typeof scope !== 'object') return null;
  if (scope.kind === 'all') return { kind: 'all' };
  return { kind: scope.kind ?? '', models: [...(scope.models ?? [])].sort() };
}
function projectTransfer(destinations, credentials, identities, billingRows) {
  const wanted = new Set(['Retained scopes', 'Retained alias', 'Retained transfer alias', 'Retained zero credit', 'Retained discovery']);
  return destinations.filter(destination => wanted.has(destination.name)).map(destination => {
    const rows = (credentials ?? []).filter(item => item.destinationId === destination.id);
    const credentialIds = new Set(rows.map(row => row.id));
    const scopes = rows.map(row => ({ enabled: row.enabled === true, scope: scopeView(row.scope) }));
    const bindings = [];
    for (const identity of identities) {
      for (const summary of identity.credentials ?? []) {
        if (!credentialIds.has(summary.credential?.id)) continue;
        for (const binding of summary.bindings ?? []) {
          bindings.push({
            enabled: binding.enabled === true,
            modelScope: scopeView(binding.modelScope),
            allowedEndpointIds: [...(binding.allowedEndpointIds ?? [])].sort(),
            allowedOrigins: [...(binding.allowedOrigins ?? [])].sort(),
          });
        }
      }
    }
    scopes.sort((left, right) => stringifyJson(left).localeCompare(stringifyJson(right)));
    bindings.sort((left, right) => stringifyJson(left).localeCompare(stringifyJson(right)));
    const credits = [];
    for (const row of rows) {
      const billing = billingRows.find(item => item.accountId === row.legacyAccountId);
      if (!billing?.credits) continue;
      credits.push({ enabled: billing.enabled !== false, remaining: billing.credits.remaining ?? null, rates: billing.credits.configuration?.rates ?? [] });
    }
    credits.sort((left, right) => stringifyJson(left).localeCompare(stringifyJson(right)));
    return { name: destination.name, endpoint: endpointOf(destination), protocols: destination.protocols ?? [], authScheme: destination.authScheme ?? null, catalog: catalogView(destination), scopes, bindings, credits };
  }).sort((left, right) => left.name.localeCompare(right.name));
}
async function captureTransfer() {
  const destinations = (await api('GET', `${prefix}/destinations`)).value.destinations ?? [];
  const credentials = (await api('GET', `${prefix}/credentials`)).value.credentials ?? [];
  const identities = (await api('GET', `${prefix}/accounts`)).value.identities ?? [];
  const records = (await api('GET', `${prefix}/account-records`)).value.accounts ?? [];
  const billingRows = [];
  for (const account of records) {
    if (!['Retained scope A', 'Retained scope B', 'Retained zero credit', 'Retained discovery', 'Retained alias', 'Retained transfer alias'].includes(account.name) && account.id !== world.creditAccountId) continue;
    const billing = (await api('GET', `${prefix}/accounts/${account.id}/billing`, undefined, { success: false })).value;
    billingRows.push({ accountId: account.id, enabled: account.enabled, credits: billing?.credits ?? null });
  }
  return projectTransfer(destinations, credentials, identities, billingRows);
}

async function establishTransferAlias() {
  const secret = rememberKey('transfer-alias', 'synthetic-transfer-alias-key');
  const created = await onboard({
    mode: 'complete',
    connection: { kind: 'new', templateId: 'custom', name: 'Retained transfer alias', endpointUrl: upstreamUrl(), upstreamProtocol: 'chat_completions', authKind: 'bearer' },
    authorization: { kind: 'api_key', secretInput: secret, accountLabel: 'Retained transfer alias' },
    targets: [{ publicModel: 'retained-alias', upstreamModel: 'upstream-alias' }],
  });
  const destination = await destinationByName('Retained transfer alias');
  const endpointUrl = endpointOf(destination);
  assert(endpointUrl === upstreamUrl(), 'transfer alias destination left the synthetic loopback');
  const catalogRevision = appliedRevision(await readRuntime());
  await api('PATCH', `${prefix}/destinations/${destination.id}`, {
    name: destination.name,
    endpointUrl,
    upstreamProtocol: 'chat_completions',
    authScheme: destination.authScheme,
    authorizeCredentialIds: [],
    enabled: true,
    models: [
      { publicModel: 'retained-alias', upstreamModel: 'upstream-alias', enabled: true, protocols: ['chat_completions'], preferred: 'chat_completions' },
    ],
  }, { cas: true });
  await appliedAfter(catalogRevision, 'transfer alias catalog');
  const found = await identityForCredential(created.credentialId);
  const binding = found.summary.bindings?.[0];
  assert(binding?.id, 'transfer alias credential has no binding');
  const scopeRevision = appliedRevision(await readRuntime());
  await api('PATCH', `${prefix}/bindings/${binding.id}`, { modelScope: { kind: 'only', models: ['retained-alias'] }, enabled: true }, { cas: true });
  await appliedAfter(scopeRevision, 'transfer alias scope');
  const account = created.accountId ? { id: created.accountId } : await accountByName('Retained transfer alias');
  await api('PUT', `${prefix}/accounts/${account.id}/billing/credits`, creditBody('upstream-alias', 3), { cas: true });
  const billing = (await api('GET', `${prefix}/accounts/${account.id}/billing`)).value;
  assert(billing.credits?.remaining === 3, 'transfer alias credit remaining was not stored');
  assert((await api('GET', `${prefix}/accounts/${account.id}`)).value.enabled !== false, 'transfer alias credit disabled the account');
}

stage('transfer-restores-destination-protocol-alias-scope-and-credits', async () => {
  await establishTransferAlias();
  const before = await captureTransfer();
  const emptied = before.find(item => item.name === 'Retained alias');
  assert(emptied && emptied.catalog.length === 0, 'transfer dropped the emptied catalog');
  const alias = before.find(item => item.name === 'Retained transfer alias');
  assert(alias, 'transfer source has no independent alias fixture');
  assert(alias.endpoint === upstreamUrl(), 'transfer alias endpoint changed');
  assert((alias.protocols ?? []).includes('chat_completions'), 'transfer alias protocol was not chat_completions');
  assert(alias.catalog.some(model => model.publicModel === 'retained-alias' && model.upstreamModel === 'upstream-alias' && model.preferred === 'chat_completions' && model.enabled !== false), 'transfer source lost the alias mapping');
  assert(alias.bindings.some(binding => binding.modelScope?.kind === 'only' && (binding.modelScope.models ?? []).includes('retained-alias')), 'transfer source lost the alias scope');
  assert(alias.credits.some(row => row.enabled !== false && row.remaining === 3), 'transfer source lost the alias credit');
  assert(before.some(item => item.name === 'Retained zero credit'), 'transfer source has no zero-credit destination');
  assert(before.some(item => item.scopes.some(row => row.scope?.kind === 'only') || item.bindings.some(binding => binding.modelScope?.kind === 'only')), 'transfer source lost a model scope');
  const exported = (await api('POST', `${prefix}/accounts/transfer/export`, { bundlePassword: 'synthetic-bundle-password-123' })).value;
  assert(exported.bundle, 'transfer export returned no bundle');
  for (const secret of secrets) assert(!String(exported.bundle).includes(secret), 'transfer bundle contained a plaintext secret');
  const preview = (await api('POST', `${prefix}/accounts/transfer/preview`, { password: 'synthetic-bundle-password-123', bundle: exported.bundle })).value;
  assert((preview.items ?? []).length >= 1, 'transfer preview returned no items');
  const revision = await contractRevision();
  const wrong = await api('POST', `${prefix}/accounts/transfer/import`, { password: 'wrong-password-123', bundle: exported.bundle }, { cas: true, success: false });
  assert(wrong.code !== 0, 'wrong transfer password succeeded');
  assert(await contractRevision() === revision, 'wrong transfer password bumped the revision');
  const bundleFile = join(world.root, 'transfer-bundle.json');
  await writeFile(bundleFile, stringifyJson(exported));
  await stopServe();
  const importDir = join(world.root, 'import-data');
  await mkdir(importDir, { recursive: true });
  world.session = join(world.root, 'import-session.json');
  world.port = 0;
  await startServe(importDir);
  await api('POST', `${prefix}/auth/register`, { username: 'retained-import', password: 'synthetic-retained-admin-password' }, { cas: true });
  await api('POST', `${prefix}/external-integrations/cpa/runtime/start`, {}, { cas: true });
  await ensureReady('import runtime');
  const bundle = parseJson(await readFile(bundleFile, 'utf8'));
  await api('POST', `${prefix}/accounts/transfer/import`, { password: 'synthetic-bundle-password-123', bundle: bundle.bundle }, { cas: true });
  await ensureReady('imported projection');
  world.gatewayKeyFile = '';
  world.primaryKey = '';
  const keyFile = await gatewayKeyFile();
  const destinations = (await api('GET', `${prefix}/destinations`)).value.destinations ?? [];
  const credentials = (await api('GET', `${prefix}/credentials`)).value.credentials ?? [];
  const identities = (await api('GET', `${prefix}/accounts`)).value.identities ?? [];
  const records = (await api('GET', `${prefix}/account-records`)).value.accounts ?? [];
  const billingRows = [];
  for (const account of records) {
    const billing = await api('GET', `${prefix}/accounts/${account.id}/billing`, undefined, { success: false });
    if (billing.code !== 0) continue;
    billingRows.push({ accountId: account.id, enabled: account.enabled, credits: billing.value?.credits ?? null });
  }
  const after = projectTransfer(destinations, credentials, identities, billingRows);
  assert(stringifyJson(after) === stringifyJson(before), `imported projection differed from ${stringifyJson(before.map(item => item.name))}`);
  const credit = records.find(account => account.name === 'Retained zero credit');
  assert(credit?.enabled !== false, 'imported zero-credit account was disabled');
  const creditBilling = billingRows.find(item => item.accountId === credit.id);
  assert(creditBilling?.credits?.remaining === 0, 'imported zero-credit remaining changed');
  const sent = await sendChat('retained-credit', 'imported-zero', keyFile);
  assert(sent.hits.length === 1, 'imported zero-credit account did not send');
  assertHit(sent.hits[0], { label: 'credit', model: 'upstream-credit', authMode: 'bearer' });
});

async function writeRunReceipt(status) {
  await mkdir(evidenceRoot, { recursive: true });
  const payload = {
    status,
    acceptance: false,
    unrun: false,
    at: new Date().toISOString(),
    binary: options.binary,
    hostDir: options.hostDir,
    trustedSHA256: trustedSha || null,
    placeholderRejected: placeholderSHA256,
    results,
    limits,
    exclusions: [
      'stateful Responses',
      'remote CPA migration',
      'AWS AK/SK',
      'Entra refresh',
      'GUI',
      'remote browser viewer',
      'live OAuth and platform device activation',
    ],
  };
  await writeFile(join(evidenceRoot, 'run-receipt.json'), `${JSON.stringify(payload, null, 2)}\n`);
}

async function main() {
  if (options.list) {
    for (const item of stages) console.log(item.name);
    return;
  }
  await mkdir(evidenceRoot, { recursive: true });
  try {
    await acceptArtifact();
    if (options.prerequisites) {
      console.log('prerequisites accepted the production host-suite record');
      await writeRunReceipt('prerequisites');
      return;
    }
    await prepareProfile();
    world.upstream = await startUpstream();
    world.proxy = await startProxy(world.upstream.address().port);
    await startServe(world.dataDir);
    await api('POST', `${prefix}/auth/register`, { username: 'retained-controls', password: 'synthetic-retained-admin-password' }, { cas: true });
    await api('POST', `${prefix}/external-integrations/cpa/runtime/start`, {}, { cas: true });
    await ensureReady('initial runtime');
    const proxyUrl = `http://127.0.0.1:${world.proxy.address().port}`;
    const beforeProxy = appliedRevision(await readRuntime());
    await api('PUT', `${prefix}/settings`, { routingMode: 'strict-priority', proxyMode: 'manual', proxyUrl, conversationSticky: false }, { cas: true });
    await appliedAfter(beforeProxy, 'deny proxy');
    const stopAt = options.scenario ? stages.findIndex(item => item.name === options.scenario) : stages.length - 1;
    if (options.scenario && stopAt < 0) throw fail(`unknown scenario ${options.scenario}`);
    for (const item of stages.slice(0, stopAt + 1)) {
      if (halted) {
        results.push({ name: item.name, status: 'BLOCKED', detail: 'an earlier stage halted the profile' });
        console.log(`BLOCKED ${item.name}`);
        continue;
      }
      try {
        const limitCount = limits.length;
        await item.fn();
        const status = item.limitOnly ? 'LIMIT' : 'PASS';
        results.push({ name: item.name, status, limits: limits.slice(limitCount).map(row => row.name) });
        console.log(`${status} ${item.name}`);
      } catch (error) {
        const status = error.kind === 'BLOCKED' ? 'BLOCKED' : 'FAIL';
        results.push({ name: item.name, status, detail: scrub(error.message).slice(0, 900) });
        console.log(`${status} ${item.name}: ${scrub(error.message).slice(0, 500)}`);
        halted = true;
      }
    }
  } catch (error) {
    const status = error.kind === 'BLOCKED' ? 'BLOCKED' : 'FAIL';
    results.push({ name: 'startup', status, detail: scrub(error.message).slice(0, 900) });
    console.log(`${status} startup: ${scrub(error.message).slice(0, 500)}`);
  } finally {
    for (const server of world.platformServers ?? []) await closeServer(server);
  }
  const failed = results.some(result => result.status === 'FAIL');
  const blockedRun = results.some(result => result.status === 'BLOCKED');
  const status = failed ? 'fail' : blockedRun ? 'incomplete' : 'pass';
  await writeRunReceipt(status);
  console.log(`receipt ${status}; acceptance false`);
  process.exitCode = failed ? 1 : blockedRun ? 2 : 0;
}

process.on('SIGINT', () => { shutdown().finally(() => process.exit(2)); });
main().catch(error => {
  console.error(scrub(error.stack || error.message));
  process.exitCode = 1;
}).finally(shutdown);
