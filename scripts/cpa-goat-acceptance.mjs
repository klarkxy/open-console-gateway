// Runs a verified CPA binary against a synthetic loopback GOAT upstream.
// No installed homes, OAuth accounts, user configurations or real keys are read.
import { createServer } from 'node:http';
import { spawn } from 'node:child_process';
import { mkdir, readFile, writeFile, readdir } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { createHash, randomUUID } from 'node:crypto';
import assert from 'node:assert/strict';

const binary = resolve(process.argv[2] ?? 'tmp/cpa-goat-validation/runtime/cli-proxy-api.exe');
const root = resolve(process.argv[3] ?? `tmp/cpa-goat-validation/run-${Date.now()}`);
const filter = process.argv[4];
await mkdir(root, { recursive: true });
const binarySha256 = createHash('sha256').update(await readFile(binary)).digest('hex');
const harnessBytes = await readFile(new URL(import.meta.url));
const harnessSha256 = createHash('sha256').update(harnessBytes).digest('hex');
await writeFile(join(root, 'harness.mjs'), harnessBytes);
const MODEL = 'goat-validation';
const UPSTREAM_MODEL = 'deepseek/deepseek-v4-flash';
const OTHER_MODEL = 'goat-other';
const OTHER_UPSTREAM = 'openai/gpt-validation';
const CLIENT_KEY = 'synthetic-cpa-client-only';
const MANAGEMENT_KEY = 'synthetic-cpa-management-only';
const KEY_A = 'synthetic-goat-a';
const KEY_B = 'synthetic-goat-b';
const keyLabel = key => key === KEY_A ? 'A' : key === KEY_B ? 'B' : 'UNEXPECTED';
const results = [];
const processes = new Set();
let current;
let seq = 0;
const delay = ms => new Promise(r => setTimeout(r, ms));
const planError = (window, reset) => ({ error: { code: 'RATE_LIMITED', type: 'rate_limit_error', message: `You've reached your ${window} usage limit for your plan. Your limit resets at ${reset}. Please wait for the window to reset or upgrade your plan to continue.` } });
const success = () => ({ status: 200 });
const error = (status, body, headers = {}) => ({ status, body, headers });
const after = seconds => new Date(Date.now() + seconds * 1000).toISOString();

const server = createServer(async (req, res) => {
  let raw = '';
  for await (const chunk of req) raw += chunk;
  let body = {};
  try { body = JSON.parse(raw); } catch {}
  const key = req.headers.authorization?.replace(/^Bearer /i, '') ?? req.headers['x-api-key'];
  const label = keyLabel(key);
  const entry = { seq: ++seq, time: new Date().toISOString(), elapsedMs: Date.now() - current.started, key: label, method: req.method, path: req.url, model: body.model, stream: body.stream === true, requestId: req.headers['x-request-id'] ?? null };
  current.arrivals.push(entry);
  if (!req.url?.startsWith('/provider/v1/')) { entry.reply = 'unexpected_path'; res.writeHead(404).end(); return; }
  if (label === 'UNEXPECTED') { entry.reply = 'unexpected_credential'; res.writeHead(401).end(); return; }
  if (req.url?.endsWith('/models')) { entry.reply = 'models'; res.setHeader('content-type', 'application/json'); res.end(JSON.stringify({ object: 'list', data: [{ id: UPSTREAM_MODEL, object: 'model', owned_by: 'command-code' }] })); return; }
  const script = current.replies.get(label) ?? [];
  const reply = script.shift() ?? success();
  if (typeof reply.body === 'function') reply.body = reply.body();
  entry.reply = reply.mode ?? reply.status;
  entry.declaredReset = reply.body?.error?.message?.match(/resets at ([^.]+(?:\.\d+)?Z)/)?.[1] ?? null;
  if (reply.mode === 'disconnect') { entry.acceptedBeforeDisconnect = true; req.socket.destroy(); return; }
  if (reply.mode === 'truncated-body') {
    entry.acceptedBeforeDisconnect = true;
    res.writeHead(200, { 'content-type': 'application/json', 'content-length': '4096' });
    res.write('{"id":"accepted-but-body-lost"');
    setTimeout(() => res.socket?.destroy(), 50); return;
  }
  if (reply.mode === 'hang') { entry.acceptedWithoutResponse = true; return; }
  if (reply.status !== 200 && reply.status !== undefined) {
    res.writeHead(reply.status, { 'content-type': 'application/json', ...reply.headers });
    res.end(typeof reply.body === 'string' ? reply.body : JSON.stringify(reply.body)); return;
  }
  const completion = { id: `chatcmpl-${entry.seq}`, object: 'chat.completion', created: Math.floor(Date.now() / 1000), model: body.model, choices: [{ index: 0, message: { role: 'assistant', content: `goat-${label}-ok` }, finish_reason: 'stop' }], usage: { prompt_tokens: 7, completion_tokens: 3, total_tokens: 10 } };
  if (body.stream || reply.mode?.includes('stream')) {
    res.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' });
    res.flushHeaders();
    if (reply.mode === 'empty-stream') { res.end(); return; }
    const chunk = { id: completion.id, object: 'chat.completion.chunk', created: completion.created, model: body.model, choices: [{ index: 0, delta: { content: `goat-${label}-visible` }, finish_reason: null }] };
    res.write(`data: ${JSON.stringify(chunk)}\n\n`);
    if (reply.mode === 'broken-stream') { setTimeout(() => res.socket?.destroy(), 75); return; }
    res.write(`data: ${JSON.stringify({ ...chunk, choices: [{ index: 0, delta: {}, finish_reason: 'stop' }], usage: completion.usage })}\n\n`);
    res.end('data: [DONE]\n\n'); return;
  }
  res.setHeader('content-type', 'application/json'); res.end(JSON.stringify(completion));
});
await new Promise(r => server.listen(0, '127.0.0.1', r));
const upstream = `http://127.0.0.1:${server.address().port}/provider/v1`;

async function freePort() {
  const probe = createServer();
  await new Promise(r => probe.listen(0, '127.0.0.1', r));
  const port = probe.address().port;
  await new Promise(r => probe.close(r)); return port;
}

async function stop(cpa) {
  if (!cpa) return;
  if (cpa.process.exitCode !== null || cpa.process.signalCode !== null) { processes.delete(cpa); return; }
  cpa.process.kill();
  await new Promise((done, reject) => {
    const timer = setTimeout(() => reject(Error('CPA process did not stop')), 5000);
    cpa.process.once('exit', () => { clearTimeout(timer); done(); });
    if (cpa.process.exitCode !== null || cpa.process.signalCode !== null) { clearTimeout(timer); done(); }
  });
  processes.delete(cpa);
  cpa.cleanup = { processId: cpa.process.pid, exitCode: cpa.process.exitCode, signalCode: cpa.process.signalCode, endpointClosed: false };
  try { await fetch(`${cpa.endpoint}/v1/models`, { signal: AbortSignal.timeout(300) }); }
  catch { cpa.cleanup.endpointClosed = true; }
  if (!cpa.cleanup.endpointClosed) throw Error('CPA endpoint still responds after its process exited');
}

async function start(name, options = {}, reuse) {
  const dir = reuse ?? join(root, name);
  await mkdir(join(dir, 'home'), { recursive: true });
  await mkdir(join(dir, 'auth'), { recursive: true });
  const port = await freePort();
  const config = {
    'config-version': 8,
    server: { host: '127.0.0.1', port },
    management: { 'allow-remote': false, 'secret-key': MANAGEMENT_KEY, 'disable-control-panel': true, 'disable-auto-update-panel': true },
    access: { 'api-keys': [CLIENT_KEY] },
    oauth: { 'auth-dir': join(dir, 'auth') },
    routing: { strategy: options.strategy ?? 'fill-first', 'session-affinity': options.affinity ?? false, retry: { 'request-retry': options.retry ?? 0, 'max-retry-credentials': options.maxCredentials ?? 0, 'max-retry-interval': 0 }, cooldown: { 'save-cooldown-status': options.persist ?? false } },
    requests: { 'passthrough-headers': true, streaming: { 'bootstrap-retries': options.bootstrap ?? 0 } },
    'api-keys': { 'openai-compatibility': (options.keys ?? [KEY_A, KEY_B]).map((key, i) => ({ name: `command-goat-${keyLabel(key).toLowerCase()}`, priority: 100 - i * 100, 'base-url': upstream, 'request-scoped-errors': options.rules ?? [], keys: [{ 'api-key': key, 'proxy-url': 'direct' }], models: [{ name: UPSTREAM_MODEL, alias: MODEL }, { name: OTHER_UPSTREAM, alias: OTHER_MODEL }] })) },
    observability: { logs: { debug: true, 'logging-to-file': false, 'request-log': options.requestLogs ?? false }, usage: { 'usage-statistics-enabled': true, 'redis-usage-queue-retention-seconds': 3600 } },
    plugins: { enabled: false },
  };
  // The legacy field supplies an explicit isolated auth root as well, for versions
  // that preserve the old spelling while reading the v8 layout.
  config['auth-dir'] = join(dir, 'auth');
  await writeFile(join(dir, 'config.yaml'), JSON.stringify(config, null, 2));
  const env = {};
  for (const key of ['PATH', 'SystemRoot', 'SYSTEMROOT', 'WINDIR', 'COMSPEC']) if (process.env[key]) env[key] = process.env[key];
  Object.assign(env, { HOME: join(dir, 'home'), USERPROFILE: join(dir, 'home'), APPDATA: join(dir, 'home'), LOCALAPPDATA: join(dir, 'home'), TEMP: dir, TMP: dir });
  const processHandle = spawn(binary, ['-config', join(dir, 'config.yaml')], { cwd: dir, env, windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
  const cpa = { process: processHandle, dir, endpoint: `http://127.0.0.1:${port}`, output: '' };
  processes.add(cpa);
  processHandle.stdout.on('data', c => { cpa.output += c; });
  processHandle.stderr.on('data', c => { cpa.output += c; });
  let spawnError;
  processHandle.on('error', e => { spawnError = e; });
  for (let attempt = 0; attempt < 100; attempt++) {
    if (spawnError) throw spawnError;
    if (processHandle.exitCode !== null) throw Error(`CPA exited ${processHandle.exitCode}: ${cpa.output.slice(-1500)}`);
    try {
      const r = await fetch(`${cpa.endpoint}/v1/models`, { headers: { Authorization: `Bearer ${CLIENT_KEY}` }, signal: AbortSignal.timeout(300) });
      if (r.ok) {
        cpa.models = await r.json();
        if (!cpa.output.includes('CLIProxyAPI Version: 8.0.10, Commit: 6fecc6e5')) throw Error('CPA runtime identity does not match the pinned release');
        if (!cpa.models.data?.some(m => m.id === MODEL)) throw Error(`Missing configured model: ${JSON.stringify(cpa.models)}`);
        return cpa;
      }
    } catch (e) { if (e.message.startsWith('Missing configured') || e.message.startsWith('CPA runtime identity')) throw e; }
    await delay(50);
  }
  throw Error(`CPA startup timed out: ${cpa.output.slice(-1500)}`);
}

async function send(cpa, { stream = false, path = '/v1/chat/completions', timeout = 5000, headers = {}, body } = {}) {
  const requestId = randomUUID();
  const started = Date.now();
  const sent = body ?? (path === '/v1/messages' ? { model: MODEL, max_tokens: 32, messages: [{ role: 'user', content: 'synthetic-ping' }], stream } : path === '/v1/responses' ? { model: MODEL, input: 'synthetic-ping', stream } : { model: MODEL, messages: [{ role: 'user', content: 'synthetic-ping' }], stream });
  const result = { requestId, path, stream, status: null, text: '', error: null };
  try {
    const response = await fetch(cpa.endpoint + path, { method: 'POST', headers: { 'content-type': 'application/json', Authorization: `Bearer ${CLIENT_KEY}`, 'X-Request-Id': requestId, ...headers }, body: JSON.stringify(sent), signal: AbortSignal.timeout(timeout) });
    result.status = response.status;
    result.headers = Object.fromEntries(response.headers);
    const reader = response.body.getReader();
    const decoder = new TextDecoder();
    try { for (;;) { const part = await reader.read(); if (part.done) break; result.text += decoder.decode(part.value, { stream: true }); } }
    catch (e) { result.error = e.message; }
  } catch (e) { result.error = e.message; }
  result.elapsedMs = Date.now() - started;
  current.responses.push(result); return result;
}

async function management(cpa, path = '/auth-files') {
  const response = await fetch(`${cpa.endpoint}/v0/management${path}`, { headers: { Authorization: `Bearer ${MANAGEMENT_KEY}` }, signal: AbortSignal.timeout(3000) });
  const data = await response.json();
  if (!response.ok) return { status: response.status, data };
  if (path === '/auth-files') return { status: response.status, data: { files: (data.files ?? []).map(({ id, auth_index, name, provider, status, status_message, unavailable, disabled, runtime_only }) => ({ id, auth_index, name, provider, status, status_message, unavailable, disabled, runtime_only })) } };
  if (Array.isArray(data)) return { status: response.status, data: data.map(item => ({ ...item, api_key: item.api_key ? '[synthetic-redacted]' : undefined })) };
  return { status: response.status, data };
}

async function files(dir) {
  const found = [];
  for (const item of await readdir(dir, { withFileTypes: true }).catch(() => [])) {
    const path = join(dir, item.name);
    if (item.isDirectory()) found.push(...await files(path));
    else if (item.name.endsWith('.cds')) found.push({ path, text: await readFile(path, 'utf8') });
  }
  return found;
}

async function scenario(name, options, replies, test) {
  if (filter && !name.includes(filter)) return;
  current = { name, started: Date.now(), arrivals: [], replies: new Map(Object.entries(replies)), responses: [], observations: {} };
  const entry = current;
  let cpa;
  try {
    cpa = await start(name, options);
    entry.models = cpa.models;
    await test(cpa, entry);
    entry.compatibility = 'PASS';
  } catch (e) {
    entry.compatibility = 'FAIL'; entry.failure = e.message;
  } finally {
    if (cpa) {
      entry.authState = await management(cpa).catch(e => ({ error: e.message }));
      entry.usageQueue = await management(cpa, '/usage-queue?count=100').catch(e => ({ error: e.message }));
      await stop(cpa);
      entry.cleanup = cpa.cleanup;
      entry.cooldownFiles = await files(cpa.dir);
      await writeFile(join(cpa.dir, 'process.log'), cpa.output);
    }
    entry.elapsedMs = Date.now() - entry.started;
    delete entry.replies;
    results.push(entry);
    await writeFile(join(root, 'report.json'), JSON.stringify(report(), null, 2));
    console.log(`${entry.compatibility} ${name}${entry.failure ? `: ${entry.failure}` : ''}`);
  }
}

function keys(entry) { return entry.arrivals.filter(a => !a.path.endsWith('/models')).map(a => a.key); }
function report() { return { binary, binarySha256, harnessSha256, version: 'v8.0.10', commit: '6fecc6e5', createdAt: new Date().toISOString(), upstream, inferenceUpstreamsLoopbackOnly: true, remoteInferenceCalls: { value: 0, basis: 'All configured inference destinations are the loopback simulator; this is not packet-capture evidence.' }, expectationBasis: 'Existing OCG no-replay/protocol contracts plus desired new GOAT plan-aware limits referenced by ocg-manager/goat_plan_cooldowns; FAIL means behavior differs from that expectation, not necessarily a CPA bug.', results, cleanup: { activeOwnedCpaProcesses: [...processes].filter(p => p.process.exitCode === null && p.process.signalCode === null).length } }; }

try {
  await scenario('priority-and-model-mapping', {}, { A: [success(), success()], B: [success()] }, async (cpa, e) => {
    const normalized = await management(cpa, '/openai-compatibility');
    e.observations.normalizedCompatibility = normalized;
    assert.equal((await send(cpa)).status, 200); assert.equal((await send(cpa)).status, 200);
    assert.deepEqual(keys(e), ['A', 'A']); assert.ok(e.arrivals.every(a => a.model === UPSTREAM_MODEL));
  });
  await scenario('exact-plan-short-reset', { persist: true }, { A: [error(429, () => planError('5-hour', after(5))), success(), success()], B: [success(), success(), success()] }, async (cpa, e) => {
    assert.equal((await send(cpa)).status, 200);
    e.observations.immediateAuth = await management(cpa);
    assert.equal((await send(cpa)).status, 200);
    await delay(1400);
    assert.ok(Date.now() + 300 < Date.parse(e.arrivals[0].declaredReset), 'HARNESS: early check no longer precedes provider reset');
    assert.equal((await send(cpa)).status, 200);
    e.observations.persistedAfterEarlyRequest = await files(cpa.dir);
    await delay(4000); assert.equal((await send(cpa)).status, 200);
    assert.deepEqual(keys(e), ['A', 'B', 'B', 'B', 'A'], 'A must stay blocked before the 5s GOAT deadline and return after expiry');
  });
  await scenario('exact-plan-long-reset-with-short-retry-after', { persist: true }, { A: [error(429, () => planError('weekly', after(3600)), { 'retry-after': '1' }), success()], B: [success(), success()] }, async (cpa, e) => {
    assert.equal((await send(cpa)).status, 200);
    await delay(150); e.observations.persistedWhileBlocked = await files(cpa.dir);
    await delay(10900);
    assert.equal((await send(cpa)).status, 200);
    assert.deepEqual(keys(e), ['A', 'B', 'B'], 'A must remain blocked until declared weekly reset, despite shorter Retry-After');
  });
  await scenario('plan-limit-applies-across-models-of-key', {}, { A: [error(429, () => planError('monthly', after(3600)), { 'retry-after': '120' }), success()], B: [success(), success()] }, async (cpa, e) => {
    assert.equal((await send(cpa)).status, 200);
    assert.equal((await send(cpa, { body: { model: OTHER_MODEL, messages: [{ role: 'user', content: 'synthetic-ping' }] } })).status, 200);
    assert.deepEqual(keys(e), ['A', 'B', 'B'], 'GOAT plan exhaustion belongs to the Key, not only the model that received 429');
  });
  await scenario('restart-preserves-declared-plan-deadline', { persist: true }, { A: [error(429, () => planError('weekly', after(3600))), success()], B: [success(), success()] }, async (cpa, e) => {
    assert.equal((await send(cpa)).status, 200);
    await delay(150); e.observations.beforeRestartFiles = await files(cpa.dir);
    await stop(cpa);
    const restarted = await start('restart-preserves-declared-plan-deadline', { persist: true }, cpa.dir);
    try {
      await delay(1500); assert.equal((await send(restarted)).status, 200);
      e.observations.afterRestartFiles = await files(restarted.dir);
      assert.deepEqual(keys(e), ['A', 'B', 'B'], 'Restart must preserve the 1h plan deadline');
    } finally {
      e.observations.afterRestartUsage = await management(restarted, '/usage-queue?count=100').catch(err => ({ error: err.message }));
      await stop(restarted); e.observations.restartedCleanup = restarted.cleanup; await writeFile(join(cpa.dir, 'restarted-process.log'), restarted.output);
    }
  });
  await scenario('generic-retry-after-key-isolation', {}, { A: [error(429, { error: { type: 'rate_limit_error', message: 'transient' } }, { 'retry-after': '2' }), success()], B: [success(), success()] }, async (cpa, e) => {
    assert.equal((await send(cpa)).status, 200); assert.equal((await send(cpa)).status, 200);
    assert.deepEqual(keys(e), ['A', 'B', 'B']);
  });
  await scenario('retry-after-two-seconds-deadline-difference', {}, { A: [error(429, { error: { type: 'rate_limit_error', message: 'transient' } }, { 'retry-after': '2' }), success()], B: [success(), success()] }, async (cpa, e) => {
    assert.equal((await send(cpa)).status, 200); assert.equal((await send(cpa)).status, 200);
    await delay(2400); assert.equal((await send(cpa)).status, 200);
    assert.deepEqual(keys(e), ['A', 'B', 'B', 'A']);
  });
  await scenario('insufficient-credit-400-fallback', {}, { A: [error(400, { error: { code: 'BAD_REQUEST', type: 'invalid_request_error', message: 'You have insufficient credits to make this request. Please purchase more credits to continue using the service.' } })], B: [success()] }, async (cpa, e) => {
    assert.equal((await send(cpa)).status, 200); assert.deepEqual(keys(e), ['A', 'B']);
  });
  await scenario('insufficient-credit-explicit-fallback-rule', { rules: [{ status: 400, match: ['You have insufficient credits to make this request. Please purchase more credits to continue using the service.'], action: 'continue-and-cooldown' }] }, { A: [error(400, { error: { code: 'BAD_REQUEST', type: 'invalid_request_error', message: 'You have insufficient credits to make this request. Please purchase more credits to continue using the service.' } })], B: [success()] }, async (cpa, e) => {
    assert.equal((await send(cpa)).status, 200); assert.deepEqual(keys(e), ['A', 'B']);
  });
  await scenario('ordinary-400-no-fallback', {}, { A: [error(400, { error: { message: 'maximum context length exceeded' } })], B: [success()] }, async (cpa, e) => {
    assert.equal((await send(cpa)).status, 400); assert.deepEqual(keys(e), ['A']);
  });
  await scenario('http-500-no-replay', {}, { A: [error(500, { error: { message: 'accepted but response failed' } })], B: [success()] }, async (cpa, e) => {
    assert.equal((await send(cpa)).status, 500); assert.deepEqual(keys(e), ['A']);
  });
  await scenario('http-500-explicit-stop-rule', { rules: [{ status: 500, match: ['accepted but response failed'], action: 'stop' }] }, { A: [error(500, { error: { message: 'accepted but response failed' } })], B: [success()] }, async (cpa, e) => {
    assert.equal((await send(cpa)).status, 500); assert.deepEqual(keys(e), ['A']);
  });
  await scenario('connection-lost-after-upstream-accept-no-replay', {}, { A: [{ mode: 'disconnect' }], B: [success()] }, async (cpa, e) => {
    const response = await send(cpa); e.observations.response = response;
    assert.deepEqual(keys(e), ['A'], 'A connection failure after upstream acceptance must not replay on B');
    assert.notEqual(response.status, 200);
  });
  await scenario('one-credential-cap-prevents-uncertain-replay', { maxCredentials: 1 }, { A: [{ mode: 'disconnect' }], B: [success()] }, async (cpa, e) => {
    const response = await send(cpa); assert.notEqual(response.status, 200);
    assert.deepEqual(keys(e), ['A']);
  });
  await scenario('one-credential-cap-retains-429-fallback', { maxCredentials: 1 }, { A: [error(429, { error: { message: 'transient' } }, { 'retry-after': '120' })], B: [success()] }, async (cpa, e) => {
    assert.equal((await send(cpa)).status, 200); assert.deepEqual(keys(e), ['A', 'B']);
  });
  await scenario('response-body-lost-after-upstream-accept-no-replay', {}, { A: [{ mode: 'truncated-body' }], B: [success()] }, async (cpa, e) => {
    const response = await send(cpa); e.observations.response = response;
    assert.deepEqual(keys(e), ['A'], 'A response-body failure after upstream acceptance must not replay on B');
    assert.notEqual(response.status, 200);
  });
  await scenario('usage-attempt-correlation', {}, { A: [error(429, { error: { message: 'synthetic-transient-limit' } }, { 'retry-after': '120' })], B: [success()] }, async (cpa, e) => {
    const response = await send(cpa); assert.equal(response.status, 200);
    await delay(50);
    const queue = await management(cpa, '/usage-queue?count=100');
    e.observations.attemptQueue = queue;
    assert.equal(queue.status, 200); assert.equal(queue.data.length, 2);
    const [failed, succeeded] = queue.data;
    assert.equal(failed.failed, true); assert.equal(failed.fail.status_code, 429);
    assert.equal(succeeded.failed, false); assert.equal(succeeded.tokens.total_tokens, 10);
    assert.equal(failed.request_id, succeeded.request_id);
    assert.notEqual(failed.auth_index, succeeded.auth_index);
    assert.notEqual(failed.execution_id, succeeded.execution_id);
    assert.ok(response.headers['x-cpa-trace-id'].endsWith(succeeded.request_id));
    assert.deepEqual(keys(e), ['A', 'B']);
    e.observations.secondPop = await management(cpa, '/usage-queue?count=100');
    assert.deepEqual(e.observations.secondPop.data, []);
  });
  await scenario('complete-stream-pass-through', {}, { A: [success()] }, async (cpa, e) => {
    const response = await send(cpa, { stream: true });
    assert.equal(response.status, 200); assert.ok(response.text.includes('goat-A-visible')); assert.ok(response.text.includes('[DONE]'));
    assert.deepEqual(keys(e), ['A']);
  });
  await scenario('stream-after-visible-output-no-replay', {}, { A: [{ mode: 'broken-stream' }], B: [success()] }, async (cpa, e) => {
    const response = await send(cpa, { stream: true });
    assert.ok(response.text.includes('goat-A-visible')); assert.deepEqual(keys(e), ['A']);
    assert.ok(!response.text.includes('goat-B'));
  });
  await scenario('empty-stream-does-not-switch-credential', {}, { A: [{ mode: 'empty-stream' }], B: [success()] }, async (cpa, e) => {
    const response = await send(cpa, { stream: true });
    assert.ok(keys(e).every(key => key === 'A'), 'An incomplete pre-output stream must not replay on a different credential');
    assert.ok(!response.text.includes('goat-B'));
  });
  await scenario('messages-protocol-conversion', {}, { A: [success()] }, async (cpa, e) => {
    const response = await send(cpa, { path: '/v1/messages' });
    assert.equal(response.status, 200); const value = JSON.parse(response.text);
    assert.equal(value.type, 'message'); assert.ok(value.content.some(c => c.text?.includes('goat-A-ok'))); assert.deepEqual(keys(e), ['A']);
  });
  await scenario('responses-protocol-conversion', {}, { A: [success()] }, async (cpa, e) => {
    const response = await send(cpa, { path: '/v1/responses' });
    assert.equal(response.status, 200); const value = JSON.parse(response.text);
    assert.equal(value.object, 'response'); assert.ok(response.text.includes('goat-A-ok')); assert.deepEqual(keys(e), ['A']);
  });
} finally {
  for (const cpa of [...processes]) await stop(cpa).catch(e => console.error(e.message));
  await new Promise(r => server.close(r));
  const final = report(); final.cleanup.mockServerClosed = !server.listening;
  await writeFile(join(root, 'report.json'), JSON.stringify(final, null, 2));
  console.log(JSON.stringify({ report: join(root, 'report.json'), cases: results.length, compatible: results.filter(r => r.compatibility === 'PASS').length, incompatible: results.filter(r => r.compatibility === 'FAIL').length, cleanup: final.cleanup }));
}
