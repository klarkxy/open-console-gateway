// Runs a verified CPA binary against a synthetic loopback GOAT upstream.
// No installed homes, OAuth accounts, user configurations or real keys are read.
import { createServer } from 'node:http';
import { spawn } from 'node:child_process';
import { mkdir, readFile, writeFile, readdir, copyFile } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { createHash, randomUUID } from 'node:crypto';
import assert from 'node:assert/strict';

const binary = resolve(process.argv[2] ?? 'tmp/cpa-goat-validation/runtime/cli-proxy-api.exe');
const root = resolve(process.argv[3] ?? `tmp/cpa-goat-validation/run-${Date.now()}`);
const pluginBinary = resolve(process.argv[4]);
const pluginSha256 = createHash("sha256").update(await readFile(pluginBinary)).digest("hex");
const filter = process.argv[5];
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
  await mkdir(join(dir, "plugins"), { recursive: true });
  await copyFile(pluginBinary, join(dir, "plugins", "ocg-probe.dll"));
  await writeFile(join(dir, "probe-control.json"), JSON.stringify(options.control ?? {}));
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
    plugins: { enabled: true, dir: join(dir, "plugins"), configs: { "ocg-probe": { enabled: true, priority: 100 } } },
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
      await delay(200);
      entry.pluginEvents = await events(cpa);
      entry.pluginState = await readFile(join(cpa.dir, "probe-state.json"), "utf8").then(JSON.parse).catch(() => ({}));
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
function report() { return { binary, binarySha256, harnessSha256, version: 'v8.0.10', commit: '6fecc6e5', createdAt: new Date().toISOString(), upstream, inferenceUpstreamsLoopbackOnly: true, remoteInferenceCalls: { value: 0, basis: 'All configured inference destinations are the loopback simulator; this is not packet-capture evidence.' }, pluginBinary, pluginSha256, expectationBasis: 'Assertions verify stated plugin capabilities AND limitations; PASS may prove an unsafe race, not production readiness.', results, cleanup: { activeOwnedCpaProcesses: [...processes].filter(p => p.process.exitCode === null && p.process.signalCode === null).length } }; }

async function events(cpa) { return (await readFile(join(cpa.dir, "probe-events.jsonl"), "utf8").catch(() => "")).trim().split("\n").filter(Boolean).map(JSON.parse); }
async function waitEvent(cpa, predicate, timeout = 3000) { const end = Date.now() + timeout; do { const all = await events(cpa); const hit = all.find(predicate); if (hit) return hit; await delay(20); } while (Date.now() < end); throw Error("Expected plugin event not received"); }
const method = (e, m) => e.filter(x => x.method === m);

try {
  await scenario('native-hooks-429-fallback-observation', {}, { A: [error(429, () => planError('weekly', after(3600)))], B: [success()] }, async (cpa,e) => {
    assert.equal((await send(cpa)).status,200);assert.deepEqual(keys(e),['A','B']);
    const usage = await waitEvent(cpa,x => x.method === 'usage.handle' && x.request.Failed);
    assert.equal(usage.request.Failure.StatusCode,429);assert.ok(usage.request.Failure.Body.includes('Your limit resets at'));
    assert.ok(usage.request.AuthID);e.observations.failedUsage = usage.request;
    const all=await events(cpa);assert.equal(method(all,'request.intercept_after').length,2);
    assert.ok(method(all,'response.intercept_after').every(x => x.request.StatusCode===200));
    e.observations.responseHooksDoNotSee429=true;
  });
  await scenario('usage-learned-exact-deadline-and-expiry', { control: { deadlineScheduling:true,learnDeadline:true }, persist:true }, { A: [error(429, () => planError('5-hour', after(5))),success()], B: [success(),success(),success()] }, async (cpa,e) => {
    assert.equal((await send(cpa)).status,200);
    await waitEvent(cpa,x=>x.method==='usage.handle'&&x.request.Failed);
    const until=Date.parse(e.arrivals[0].declaredReset);assert.ok(until>Date.now());
    await delay(1400);assert.ok(Date.now()+300<until);assert.equal((await send(cpa)).status,200);
    await delay(Math.max(0,until-Date.now()+150));assert.equal((await send(cpa)).status,200);
    assert.deepEqual(keys(e),['A','B','B','A']);
  });
  await scenario('usage-learned-deadline-across-models', { control: { deadlineScheduling:true,learnDeadline:true } }, { A: [error(429, () => planError('monthly', after(3600))),success()], B: [success(),success()] }, async (cpa,e) => {
    assert.equal((await send(cpa)).status,200);await waitEvent(cpa,x=>x.method==='usage.handle'&&x.request.Failed);
    assert.equal((await send(cpa,{ body:{model:OTHER_MODEL,messages:[{role:'user',content:'synthetic'}]} })).status,200);
    assert.deepEqual(keys(e),['A','B','B']);
  });
  await scenario('usage-learned-deadline-survives-restart', { control: { deadlineScheduling:true,learnDeadline:true },persist:true }, { A: [error(429, () => planError('weekly', after(3600))),success()], B: [success(),success()] }, async (cpa,e) => {
    assert.equal((await send(cpa)).status,200);await waitEvent(cpa,x=>x.method==='usage.handle'&&x.request.Failed);
    await stop(cpa);const restarted=await start(e.name,{control:{deadlineScheduling:true,learnDeadline:true},persist:true},cpa.dir);
    try { await delay(1400);assert.equal((await send(restarted)).status,200);assert.deepEqual(keys(e),['A','B','B']); }
    finally { await stop(restarted);e.observations.restartCleanup=restarted.cleanup;await writeFile(join(cpa.dir,'restart-process.log'),restarted.output); }
  });
  await scenario('delayed-usage-proves-deadline-race', { control: { deadlineScheduling:true,learnDeadline:true,usageDelayMs:2500 } }, { A: [error(429, () => planError('monthly', after(3600))),success()], B: [success(),success()] }, async (cpa,e) => {
    assert.equal((await send(cpa)).status,200);
    assert.equal((await send(cpa,{body:{model:OTHER_MODEL,messages:[{role:'user',content:'synthetic'}]}})).status,200);
    assert.deepEqual(keys(e),['A','B','A'],'Asynchronous observer must demonstrate cross-model early reuse before deadline is recorded');
    const usage=await waitEvent(cpa,x=>x.method==='usage.handle'&&x.request.Failed,5000);
    e.observations.reuseBeforeDeadlinePublication=e.arrivals[2].time;e.observations.deadlinePublishedAtMs=usage.atMs;
    assert.ok(Date.parse(e.arrivals[2].time)<usage.atMs);
  });
  for(const mode of ['disconnect','truncated-body','empty-stream']) {
    await scenario(`after-auth-stops-second-attempt-${mode}`, {control:{stopSecondAttempt:true}}, {A:[{mode}],B:[success()]},async(cpa,e)=>{
      const response=await send(cpa,{stream:mode==='empty-stream'});
      assert.deepEqual(keys(e),['A']);
      if(mode==='empty-stream') { assert.equal(response.status,500);assert.ok(response.text.includes('empty_stream')); }
      else assert.ok(response.text.includes('probe_no_replay')||response.status===503);
      const all=await events(cpa);const afterHooks=method(all,'request.intercept_after');assert.equal(afterHooks.length,2);
      assert.equal(afterHooks[1].response.result.Terminate,true);
    });
  }
  await scenario('coarse-no-replay-also-disables-429-fallback', {control:{stopSecondAttempt:true}}, {A:[error(429,()=>planError('weekly',after(3600)))],B:[success()]},async(cpa,e)=>{
    const response=await send(cpa);assert.equal(response.status,503);assert.deepEqual(keys(e),['A']);
  });
  await scenario('scheduler-explicit-rejection-zero-upstream', {}, {},async(cpa,e)=>{
    await writeFile(join(cpa.dir,'probe-control.json'),JSON.stringify({rejectAll:true}));
    const r=await send(cpa);assert.notEqual(r.status,200);assert.deepEqual(keys(e),[]);
  });
  await scenario('scheduler-error-stops-selection', {control:{schedulerError:true}}, {},async(cpa,e)=>{
    assert.notEqual((await send(cpa)).status,200);assert.deepEqual(keys(e),[]);
    assert.ok(method(await events(cpa),'scheduler.pick').length);
  });
  await scenario('invalid-scheduler-decision-falls-back', {control:{invalidScheduler:true}}, {A:[success()]},async(cpa,e)=>{
    assert.equal((await send(cpa)).status,200);assert.deepEqual(keys(e),['A']);
    assert.ok(method(await events(cpa),'scheduler.pick').some(x=>x.response.result.AuthID==='absent-auth'));
  });
  await scenario('interceptor-error-is-fail-open', {control:{interceptorError:true}}, {A:[{mode:'disconnect'}],B:[success()]},async(cpa,e)=>{
    assert.equal((await send(cpa)).status,200);assert.deepEqual(keys(e),['A','B']);
    assert.ok(method(await events(cpa),'request.intercept_after').every(x=>x.response.ok===false));
  });
  await scenario('ordinary-429-does-not-create-plan-deadline', {control:{deadlineScheduling:true,learnDeadline:true}}, {A:[error(429,{error:{message:'Too many requests'}}),success()],B:[success()]},async(cpa,e)=>{
    assert.equal((await send(cpa)).status,200);await waitEvent(cpa,x=>x.method==='usage.handle'&&x.request.Failed);
    const state=await readFile(join(cpa.dir,'probe-state.json'),'utf8').then(JSON.parse).catch(()=>({}));assert.deepEqual(state,{});
    await delay(1400);assert.equal((await send(cpa)).status,200);assert.deepEqual(keys(e),['A','B','A']);
  });
  for(const mode of ['http-500','disconnect','truncated-body','empty-stream','explicit-429']) {
    await scenario(`native-stop-rule-${mode}`, {rules:[{status:500,'match-regexr':['(?s).*'],action:'stop'}]}, {A:[mode==='http-500'?error(500,{error:{message:'accepted but response failed'}}):mode==='explicit-429'?error(429,()=>planError('weekly',after(3600))):{mode}],B:[success()]},async(cpa,e)=>{
      const response=await send(cpa,{stream:mode==='empty-stream'});
      e.observations.client=response;
      if(mode==='http-500') { assert.deepEqual(keys(e),['A']);assert.equal(response.status,500); }
      else { assert.deepEqual(keys(e),['A','B']);assert.equal(response.status,200); }
    });
  }
} finally {
  for(const cpa of [...processes])await stop(cpa);
  server.closeAllConnections();await new Promise(r=>server.close(r));
  const final=report();final.cleanup.mockServerClosed=!server.listening;await writeFile(join(root,'report.json'),JSON.stringify(final,null,2));
  console.log(JSON.stringify({total:results.length,pass:results.filter(x=>x.compatibility==='PASS').length,fail:results.filter(x=>x.compatibility==='FAIL').length,report:join(root,'report.json')}));
}
