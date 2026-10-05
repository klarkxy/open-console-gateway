// Actual executable journeys against an isolated headless host and loopback upstream.
// No credentials, provider configuration, or installed client homes are consulted.
import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { existsSync } from 'node:fs';
import { copyFile, mkdir, readFile, writeFile } from 'node:fs/promises';
import { delimiter, dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createHash, randomUUID } from 'node:crypto';
import assert from 'node:assert/strict';

const binary = resolve(process.argv[2] ?? (process.platform === 'win32' ? 'target/debug/ocg.exe' : 'target/debug/ocg'));
const cpaHostDir = process.argv[4] || process.env.OCG_CPA_HOST_DIR || '';
const binarySha256 = createHash('sha256').update(await readFile(binary)).digest('hex');
const root = resolve(process.argv[3] ?? `tmp/ocg3-cli-delivery/acceptance-${Date.now()}`);
await mkdir(root, { recursive: true });
const results = [];
const arrivals = [];
let upstreamCredential = 'synthetic-upstream-key-123';
const dshMethods = [];
let dshInstalled = false;
let endpoint;
let host;
let serveLog = '';
let dataDirName = 'data';
let serial = 0;
const parseJson = text => JSON.parse(text, (_key, value, context) =>
  typeof value === 'number' && Number.isInteger(value) && !Number.isSafeInteger(value) && /^-?\d+$/.test(context?.source ?? '') ? BigInt(context.source) : value);
const stringifyJson = value => JSON.stringify(value, (_key, item) => typeof item === 'bigint' ? JSON.rawJSON(item.toString()) : item);
const environment = { ...process.env, HOME: join(root, 'home'), USERPROFILE: join(root, 'home'), OCG_MANAGER_ENCRYPTION_KEY: 'synthetic-cli-acceptance-cipher' };
for (const name of ['CODEX_HOME', 'KIMI_CODE_HOME', 'MINIMAX_DATA_DIR', 'MAVIS_DATA_DIR', 'ZCODE_PERSONAL_PROVIDER_CONFIG_FILE', 'ZCODE_DATA_BASE_DIR', 'DSH_HOME', 'OCG_BROWSER_WORKER_URL', 'OCG_BROWSER_CONTROL_TOKEN_FILE']) delete environment[name];
await mkdir(environment.HOME, { recursive: true });
await mkdir(join(environment.HOME, '.dsh'), { recursive: true });
await writeFile(join(environment.HOME, '.dsh', '.credentials.yaml'), 'version: 1\nrecords:\n  client-connection/browser-session:\n    kind: grant\n    payload:\n      version: 1\n      secret: AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\n');

function runCaptured(command, args, env) {
  return new Promise((done, reject) => {
    const child = spawn(command, args, { env, windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
    let stdout = '', stderr = '';
    child.stdout.on('data', chunk => { stdout += chunk; });
    child.stderr.on('data', chunk => { stderr += chunk; });
    child.on('error', reject);
    child.on('exit', code => done({ code, stdout, stderr }));
  });
}

function pidAlive(pid) {
  try { process.kill(pid, 0); return true; } catch { return false; }
}

async function installSyntheticBrowser() {
  const isolation = join(root, 'browser-isolation');
  const programFiles = join(isolation, 'ProgramFiles');
  const programFilesX86 = join(isolation, 'ProgramFilesX86');
  const localAppData = join(isolation, 'LocalAppData');
  const pathBin = join(isolation, 'bin');
  const edgeDir = join(programFilesX86, 'Microsoft', 'Edge', 'Application');
  await mkdir(programFiles, { recursive: true });
  await mkdir(edgeDir, { recursive: true });
  await mkdir(localAppData, { recursive: true });
  await mkdir(pathBin, { recursive: true });
  const scriptDir = dirname(fileURLToPath(import.meta.url));
  const workspace = resolve(scriptDir, '..');
  const source = join(workspace, 'scripts', 'acceptance-fixtures', 'fake-browser.rs');
  const outDir = join(workspace, 'target', 'ocg3-cli-tests');
  await mkdir(outDir, { recursive: true });
  const compiled = join(outDir, process.platform === 'win32' ? 'ocg-fake-msedge.exe' : 'ocg-fake-msedge');
  const rustcArgs = ['--edition', '2021', '-C', 'debuginfo=0', '-C', 'opt-level=1', '--crate-type', 'bin', '-o', compiled, source];
  const built = await runCaptured('rustc', rustcArgs, process.env);
  if (built.code !== 0 || !existsSync(compiled)) {
    throw new Error(`synthetic browser fixture failed to compile: ${(built.stderr || built.stdout).slice(0, 1500)}`);
  }
  const fakeName = process.platform === 'win32' ? 'msedge.exe' : 'msedge';
  const fakeEdge = join(edgeDir, fakeName);
  await copyFile(compiled, fakeEdge);
  await copyFile(compiled, join(pathBin, fakeName));
  environment.ProgramFiles = programFiles;
  environment['ProgramFiles(x86)'] = programFilesX86;
  environment.LOCALAPPDATA = localAppData;
  const systemRoot = environment.SystemRoot || environment.SYSTEMROOT || (process.platform === 'win32' ? 'C:\\Windows' : '/usr');
  environment.PATH = [pathBin, join(systemRoot, process.platform === 'win32' ? 'System32' : 'bin'), systemRoot].join(delimiter);
  environment.Path = environment.PATH;
  return { capturePath: join(edgeDir, 'ocg-browser-capture.json'), fakeEdge, isolation };
}

const syntheticBrowser = await installSyntheticBrowser();

function invoke(args, input, timeoutMs = 45000) {
  return new Promise((done, reject) => {
    const child = spawn(binary, args, { env: environment, windowsHide: true, stdio: ['pipe', 'pipe', 'pipe'] });
    let stdout = '', stderr = '';
    child.stdout.on('data', chunk => { stdout += chunk; });
    child.stderr.on('data', chunk => { stderr += chunk; });
    child.on('error', reject);
    const timer = setTimeout(() => child.kill(), timeoutMs);
    child.on('exit', code => { clearTimeout(timer); done({ code, stdout, stderr }); });
    child.stdin.end(input);
  });
}

async function api(method, path, body, { cas = false, success = true, stdin = false, extra = [] } = {}) {
  const index = ++serial;
  const output = join(root, `response-${index}.json`);
  const args = ['--endpoint', endpoint, 'api', method, path, '--output', output, ...extra];
  let input;
  if (body !== undefined) {
    if (stdin) { args.push('--input', '-'); input = stringifyJson(body); }
    else {
      const file = join(root, `request-${index}.json`);
      await writeFile(file, stringifyJson(body));
      args.push('--input', file);
    }
  }
  if (cas) args.push('--cas-current');
  const run = await invoke(args, input);
  if (success) assert.equal(run.code, 0, `${method} ${path}: ${run.stderr.slice(0, 1000)}`);
  else assert.notEqual(run.code, 0, `${method} ${path} unexpectedly succeeded`);
  let text = '';
  try { text = await readFile(output, 'utf8'); } catch {}
  let value;
  try { value = parseJson(text); } catch { value = text; }
  return { ...run, value };
}

function nonzeroGeneration(value) {
  if (typeof value === 'bigint') return value > 0n;
  if (typeof value === 'number') return Number.isFinite(value) && value > 0;
  return false;
}

function restoredOwnedReady(runtime) {
  if (!runtime || typeof runtime !== 'object') return false;
  return runtime.installed === true
    && runtime.running === true
    && runtime.owned === true
    && runtime.desiredRunning === true
    && runtime.phase === 'idle'
    && runtime.policyReady === true
    && runtime.applyStatus === 'applied'
    && runtime.executionUnavailable === false
    && nonzeroGeneration(runtime.childProcessGeneration)
    && runtime.desiredRevision != null
    && runtime.appliedRevision != null
    && runtime.desiredRevision === runtime.appliedRevision
    && typeof runtime.desiredDigest === 'string'
    && runtime.desiredDigest.length === 64
    && runtime.desiredDigest === runtime.appliedDigest;
}

async function check(name, fn) {
  try { await fn(); results.push({ name, status: 'PASS' }); console.log(`PASS ${name}`); }
  catch (error) { results.push({ name, status: 'FAIL', detail: error.message }); console.log(`FAIL ${name}: ${error.message.slice(0, 500)}`); throw error; }
}

const upstream = createServer(async (req, res) => {
  let text = '';
  for await (const chunk of req) text += chunk;
  let body = {};
  try { body = JSON.parse(text); } catch {}
  if (typeof body.method === 'string' && typeof body.rpcId === 'string') {
    dshMethods.push(body.method);
    res.setHeader('content-type', 'application/json');
    if (!req.headers.cookie?.includes('dsh-auth-')) { res.writeHead(403); res.end('{}'); return; }
    const name = '@open-console-gateway/dsh-plugin';
    let value;
    switch (body.method) {
      case 'pluginManager/listBundles': value = dshInstalled ? [{ name, installed: true, enabled: true, removable: true, version: '0.1.0' }] : []; break;
      case 'pluginManager/listPlugins': value = dshInstalled ? [{ moduleName: name, enabled: true, fiberPhase: 'active' }] : []; break;
      case 'pluginManager/inspect': value = { status: 'accepted', kind: 'path', name, bundle: true }; break;
      case 'pluginManager/installBundle':
      case 'pluginManager/waitForInstall': dshInstalled = true; value = { changed: true, application: 'applied', stage: 'install', target: name, bundle: name }; break;
      case 'pluginManager/removeBundle': dshInstalled = false; value = { changed: true, application: 'applied', stage: 'remove', target: name, bundle: name }; break;
      default: value = null;
    }
    res.end(JSON.stringify({ type: 'server-response', rpcId: body.rpcId, result: { ok: true, value } })); return;
  }
  if (req.url?.startsWith('/dashboard/api/v4/')) {
    const authenticated = req.headers.cookie?.includes('ocg_dashboard_session=synthetic-session') === true;
    const status = { revision: 7, processGeneration: 17, local: false, initialized: true, authenticated };
    res.setHeader('content-type', 'application/json');
    if (req.url.endsWith('/auth/status')) { res.end(JSON.stringify(status)); return; }
    if (req.url.endsWith('/auth/login')) {
      if (body.expectedRevision !== 7 || body.processGeneration !== 17) { res.writeHead(409); res.end(JSON.stringify({ code: 'revisionConflict', message: 'stale expectations' })); return; }
      res.setHeader('set-cookie', 'ocg_dashboard_session=synthetic-session; Path=/dashboard/api; HttpOnly');
      res.end(JSON.stringify({ ...status, authenticated: true })); return;
    }
    if (!authenticated) { res.writeHead(401); res.end(JSON.stringify({ code: 'unauthorized', message: 'login required' })); return; }
    if (req.url.endsWith('/auth/logout')) {
      res.setHeader('set-cookie', 'ocg_dashboard_session=; Path=/dashboard/api; Max-Age=0; HttpOnly');
      res.end(JSON.stringify({ ...status, authenticated: false })); return;
    }
    res.end(JSON.stringify({ ...status, pricingRevision: 'synthetic-global-pricing' })); return;
  }
  arrivals.push({ path: req.url, model: body.model, stream: body.stream === true });
  if (req.headers.authorization !== `Bearer ${upstreamCredential}`) {
    res.writeHead(401, { 'content-type': 'application/json' });
    res.end(JSON.stringify({ error: { message: 'fixture credential rejected' } })); return;
  }
  if (req.url?.endsWith('/models')) {
    res.setHeader('content-type', 'application/json');
    res.end(JSON.stringify({ object: 'list', data: [{ id: 'vendor-cli-model', object: 'model', owned_by: 'loopback' }] }));
    return;
  }
  if (body.stream) {
    res.setHeader('content-type', 'text/event-stream');
    const chunk = { id: 'chatcmpl-cli', object: 'chat.completion.chunk', model: body.model, choices: [{ index: 0, delta: { content: 'cli-loopback-ok' }, finish_reason: null }] };
    res.write(`data: ${JSON.stringify(chunk)}\n\n`);
    res.write(`data: ${JSON.stringify({ ...chunk, choices: [{ index: 0, delta: {}, finish_reason: 'stop' }], usage: { prompt_tokens: 2, completion_tokens: 3, total_tokens: 5 } })}\n\n`);
    res.end('data: [DONE]\n\n');
    return;
  }
  res.setHeader('content-type', 'application/json');
  res.end(JSON.stringify({ id: 'chatcmpl-cli', object: 'chat.completion', created: 1, model: body.model, choices: [{ index: 0, message: { role: 'assistant', content: 'cli-loopback-ok' }, finish_reason: 'stop' }], usage: { prompt_tokens: 2, completion_tokens: 3, total_tokens: 5 } }));
});
await new Promise(done => upstream.listen(0, '127.0.0.1', done));
const upstreamUrl = `http://127.0.0.1:${upstream.address().port}/v1/chat/completions`;

async function freePort() {
  const listener = createServer();
  await new Promise(done => listener.listen(0, '127.0.0.1', done));
  const port = listener.address().port;
  await new Promise(done => listener.close(done));
  return port;
}

async function start(port, data = 'data') {
  dataDirName = data;
  endpoint = `http://127.0.0.1:${port}`;
  serveLog = '';
  const args = ['--data-dir', join(root, data), 'serve', '--port', String(port)];
  if (cpaHostDir) args.push('--cpa-host-dir', cpaHostDir);
  host = spawn(binary, args, { env: environment, windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
  let error = '';
  host.stderr.on('data', chunk => { error += chunk; serveLog += chunk; });
  host.stdout.resume();
  for (let attempt = 0; attempt < 100; attempt++) {
    if (host.exitCode !== null) throw new Error(`serve exited ${host.exitCode}: ${error.slice(0, 1000)}`);
    try { const response = await fetch(`${endpoint}/dashboard/api/v4/auth/status`); if (response.ok) return; } catch {}
    await new Promise(done => setTimeout(done, 100));
  }
  throw new Error(`serve never became ready: ${error.slice(0, 1000)}`);
}

async function stop() {
  if (!host || host.exitCode !== null) return;
  const child = host;
  await new Promise(done => { child.once('exit', done); child.kill('SIGINT'); });
  host = undefined;
}

const prefix = '/dashboard/api/v4';
let accountId, credentialId, destinationId, bindingId, transfer, gatewayKeyFile;
let stale;
try {
  const port = await freePort();
  await start(port);
  await check('fresh host and V4 authority', async () => {
    const { value } = await api('GET', `${prefix}/auth/status`);
    assert.equal(value.local, true); assert.equal(value.authenticated, true); assert.equal(value.initialized, false);
  });
  await check('schema help without database', async () => {
    const run = await invoke(['schema', 'v4']); assert.equal(run.code, 0, run.stderr); assert.ok(JSON.parse(run.stdout).$defs);
  });
  await check('second writer rejected before opening the database', async () => {
    const run = await invoke(['--data-dir', join(root, 'data'), 'serve', '--port', String(await freePort())]);
    assert.notEqual(run.code, 0);
    assert.ok((await api('GET', `${prefix}/auth/status`)).value.local);
  });
  for (const family of ['templates', 'connections', 'accounts', 'account-records', 'destinations', 'credentials', 'settings', 'provider-contracts', 'providers', 'providers/opencode/pricing', 'platform-accounts', 'routing/cards', 'alias-publication', 'routing/temporary-unavailability', 'gateway/status', 'dashboard/summary', 'dashboard/daily-tokens-by-model', 'logs/gateway', 'logs/forward', 'logs/forward/models', 'logs/forward/keys', 'browser/capabilities', 'external-integrations/cpa', 'external-integrations/cpa/runtime', 'applications/dsh', 'applications/byok/codex', 'applications/byok/kimi', 'applications/byok/minimax', 'applications/byok/zcode', 'settings/update-status']) {
    await check(`read ${family}`, async () => {
      const query = family === 'applications/dsh' ? `?runtimeUrl=${encodeURIComponent(`http://127.0.0.1:${upstream.address().port}`)}` : '';
      const run = await api('GET', `${prefix}/${family}${query}`); assert.ok(run.value && typeof run.value === 'object');
    });
  }
  await check('owned CPA host is registered', async () => {
    assert.equal((await api('GET', `${prefix}/external-integrations/cpa/runtime`)).value.supported, true);
  });
  await check('auth register from stdin and persisted session', async () => {
    const result = await api('POST', `${prefix}/auth/register`, { username: 'cli-acceptance', password: 'synthetic-admin-password-123' }, { cas: true, stdin: true, extra: ['--session-file', join(root, 'session.json')] });
    assert.ok(!result.stdout.includes('synthetic-admin-password-123'));
    assert.equal((await api('GET', `${prefix}/auth/status`)).value.initialized, true);
  });
  await check('custom onboarding through executable', async () => {
    const { value } = await api('POST', `${prefix}/onboarding/commit`, { operationId: randomUUID(), mode: 'complete', authorizeCurrentEndpoint: true, connection: { kind: 'new', templateId: 'custom', name: 'CLI acceptance upstream', endpointUrl: upstreamUrl, upstreamProtocol: 'chat_completions', authKind: 'bearer' }, authorization: { kind: 'api_key', secretInput: 'synthetic-upstream-key-123', accountLabel: 'CLI upstream' }, targets: [{ publicModel: 'cli-test-model', upstreamModel: 'vendor-cli-model' }] }, { cas: true });
    accountId = value.accountId;
    credentialId = value.credentialId;
    if (!accountId) { const records = (await api('GET', `${prefix}/account-records`)).value; accountId = records.accounts.find(row => row.name === 'CLI acceptance upstream')?.id; }
    assert.ok(accountId);
    const destinations = (await api('GET', `${prefix}/destinations`)).value.destinations;
    const destination = destinations.find(row => row.name === 'CLI acceptance upstream');
    assert.ok(destination); destinationId = destination.id;
    const credentials = (await api('GET', `${prefix}/credentials`)).value.credentials;
    credentialId ??= credentials.find(row => row.legacyAccountId === accountId || row.accountId === accountId)?.id;
  });
  await check('CAS preserves stale expectations and rejects writes', async () => {
    const contract = (await api('GET', `${prefix}/contract`)).value;
    stale = { expectedRevision: contract.revision, processGeneration: contract.processGeneration };
    await api('PATCH', `${prefix}/accounts/${accountId}`, { name: 'CLI renamed' }, { cas: true });
    await api('PATCH', `${prefix}/accounts/${accountId}`, { ...stale, name: 'must-not-commit' }, { cas: true, success: false });
    assert.equal((await api('GET', `${prefix}/accounts/${accountId}`)).value.name, 'CLI renamed');
  });
  await check('missing CAS rejected', async () => { await api('PUT', `${prefix}/settings`, { conversationSticky: false }, { success: false }); });
  await check('credential rotation and binding enablement', async () => {
    assert.ok(credentialId);
    const before = (await api('GET', `${prefix}/accounts`)).value.identities.flatMap(row => row.credentials ?? []).find(row => row.credential.id === credentialId)?.credential.version;
    assert.ok(before !== undefined);
    const rotation = (await api('POST', `${prefix}/credentials/${credentialId}/rotate`, { secretInput: 'synthetic-rotated-key-456' }, { cas: true })).value;
    upstreamCredential = 'synthetic-rotated-key-456';
    assert.equal(rotation.credentialId, credentialId);
    const identities = (await api('GET', `${prefix}/accounts`)).value.identities;
    const identity = identities.find(row => row.credentials?.some(summary => summary.credential.id === credentialId));
    const bindings = identity?.credentials?.find(row => row.credential.id === credentialId)?.bindings;
    bindingId = bindings?.[0]?.id;
    assert.ok(bindingId);
    await api('PATCH', `${prefix}/bindings/${bindingId}`, { enabled: true }, { cas: true });
    assert.ok(rotation.version > before);
  });
  await check('Gateway Key CRUD without changing primary Key', async () => {
    const created = (await api('POST', `${prefix}/keys`, { name: 'CLI acceptance subkey' }, { cas: true })).value;
    const listed = (await api('GET', `${prefix}/connection`)).value;
    const key = listed.subKeys.find(row => row.name === 'CLI acceptance subkey');
    assert.ok(key);
    await api('POST', `${prefix}/keys/${key.id}/regenerate`, {}, { cas: true });
    assert.notEqual((await api('GET', `${prefix}/connection`)).value.subKeys.find(row => row.id === key.id).value, key.value);
    await api('PATCH', `${prefix}/keys/${key.id}`, { enabled: false }, { cas: true });
    await api('DELETE', `${prefix}/keys/${key.id}`, {}, { cas: true });
    assert.ok(!(await api('GET', `${prefix}/connection`)).value.subKeys.some(row => row.id === key.id));
  });
  await check('routing card replacement persists the current layout', async () => {
    const before = (await api('GET', `${prefix}/routing/cards`)).value;
    const after = (await api('PUT', `${prefix}/routing/cards`, { cards: before.cards }, { cas: true })).value;
    assert.deepEqual(after.cards, before.cards);
  });
  await check('destination catalog and model metadata edits', async () => {
    const refreshed = (await api('POST', `${prefix}/destinations/${destinationId}/catalog/refresh`, {}, { cas: true })).value;
    assert.equal(refreshed.destination.id, destinationId);
    assert.equal(refreshed.revision.revision, (await api('GET', `${prefix}/auth/status`)).value.revision);
    await api('PUT', `${prefix}/destinations/${destinationId}/catalog`, { updates: [{ publicModel: 'cli-test-model', enabled: true }], removeModels: [] }, { cas: true });
    await api('PUT', `${prefix}/destinations/${destinationId}/model-metadata`, { publicModel: 'cli-test-model', metadata: { name: 'CLI acceptance model', contextWindow: 8192, maxOutputTokens: 512, inputModalities: ['text'] } }, { cas: true });
    const metadata = (await api('GET', `${prefix}/destinations/${destinationId}/model-metadata`)).value;
    assert.equal(metadata.models.find(row => row.publicModel === 'cli-test-model').metadata.contextWindow, 8192);
  });
  await check('billing configuration grants and calibration', async () => {
    const configuration = { name: 'CLI credits', currency: 'USD', creditsPerCurrency: 1, rates: [{ model: 'vendor-cli-model', inputPerMillion: 1, outputPerMillion: 1, cacheReadPerMillion: null, cacheWritePerMillion: null }], monthly: null, sourceUrl: null };
    const bucket = { id: 'cli-initial-bucket', kind: 'manual', label: 'CLI initial', granted: 100, remaining: 100, startsAt: new Date().toISOString(), expiresAt: null };
    await api('PUT', `${prefix}/accounts/${accountId}/billing/credits`, { configuration, initialBuckets: [bucket] }, { cas: true });
    const granted = (await api('POST', `${prefix}/accounts/${accountId}/billing/credits/grants`, { label: 'CLI grant', amount: 10, expiresAt: null }, { cas: true })).value;
    assert.equal(granted.credits.remaining, 110);
    const balances = granted.credits.buckets.map(row => ({ bucketId: row.id, remaining: row.id === bucket.id ? 50 : 10 }));
    const calibrated = (await api('POST', `${prefix}/accounts/${accountId}/billing/credits/calibrate`, { balances }, { cas: true })).value;
    assert.equal(calibrated.credits.remaining, 60);
  });
  for (const [client, parts, initial] of [
    ['codex', ['.codex', 'config.toml'], '# CLI acceptance\n'],
    ['kimi', ['.kimi-code', 'config.toml'], '# CLI acceptance\n'],
    ['minimax', ['.minimax', 'config.yaml'], '{}\n'],
    ['zcode', ['.zcode', 'v2', 'provider_config.json'], '{"schemaVersion":1,"config":{"providerOrder":[],"providerConfigRules":{"providerRules":[]},"modelConfigRules":{"providerModelRules":[],"manualProviderModelRules":[]}}}\n'],
  ]) await check(`isolated BYOK ${client} configure and remove`, async () => {
    const file = join(environment.HOME, ...parts);
    await mkdir(resolve(file, '..'), { recursive: true }); await writeFile(file, initial);
    const view = (await api('GET', `${prefix}/applications/byok/${client}?targetPath=${encodeURIComponent(file)}`)).value;
    assert.equal(view.configureSupported, true); assert.ok(view.fingerprint);
    const configured = (await api('POST', `${prefix}/applications/byok/${client}`, { expectedFingerprint: view.fingerprint, clientClosed: true }, { cas: true })).value;
    assert.equal(configured.status, 'configured');
    assert.ok(configured.configuredModelIds.includes('cli-test-model'));
    assert.ok((await readFile(file, 'utf8')).includes(endpoint));
    const removed = (await api('DELETE', `${prefix}/applications/byok/${client}`, { expectedFingerprint: configured.fingerprint, clientClosed: true }, { cas: true })).value;
    assert.equal(removed.status, 'ready');
  });
  await check('secret-safe connection output and private gateway key', async () => {
    const { value } = await api('GET', `${prefix}/connection`);
    assert.ok(value.primaryKey && value.primaryKey !== '[redacted]');
    gatewayKeyFile = join(root, 'gateway-key.txt'); await writeFile(gatewayKeyFile, value.primaryKey);
    const publicRun = await invoke(['--endpoint', endpoint, 'api', 'GET', `${prefix}/connection`]);
    assert.equal(publicRun.code, 0, publicRun.stderr); assert.ok(!publicRun.stdout.includes(value.primaryKey));
  });
  await check('local model publication', async () => {
    const { value } = await api('GET', '/v1/models', undefined, { extra: ['--key-file', gatewayKeyFile] });
    assert.ok(value.data.some(row => row.id === 'cli-test-model'));
    await api('PATCH', `${prefix}/alias-publication`, { publicModel: 'cli-test-model', published: false }, { cas: true });
    assert.ok(!(await api('GET', '/v1/models', undefined, { extra: ['--key-file', gatewayKeyFile] })).value.data.some(row => row.id === 'cli-test-model'));
    await api('PATCH', `${prefix}/alias-publication`, { publicModel: 'cli-test-model', published: true }, { cas: true });
    assert.ok((await api('GET', '/v1/models', undefined, { extra: ['--key-file', gatewayKeyFile] })).value.data.some(row => row.id === 'cli-test-model'));
  });
  await check('isolated DSH HTTP runtime install and uninstall', async () => {
    const runtimeUrl = `http://127.0.0.1:${upstream.address().port}`;
    const query = `${prefix}/applications/dsh?runtimeUrl=${encodeURIComponent(runtimeUrl)}`;
    const before = (await api('GET', query)).value;
    assert.equal(before.installSupported, true); assert.ok(before.fingerprint);
    const installed = (await api('POST', `${prefix}/applications/dsh`, { expectedFingerprint: before.fingerprint, runtimeUrl }, { cas: true })).value;
    assert.equal(installed.installed, true); assert.equal(installed.enabled, true);
    const current = (await api('GET', query)).value;
    const removed = (await api('DELETE', `${prefix}/applications/dsh`, { expectedFingerprint: current.fingerprint, runtimeUrl }, { cas: true })).value;
    assert.equal(removed.installed, false);
    assert.ok(dshMethods.includes('pluginManager/installBundle')); assert.ok(dshMethods.includes('pluginManager/removeBundle'));
  });
  await check('owned runtime applies before inference', async () => {
    assert.ok(cpaHostDir, 'pinned CPA host directory was not provided');
    const started = (await api('POST', `${prefix}/external-integrations/cpa/runtime/start`, {}, { cas: true })).value;
    assert.equal(started.applyStatus, 'applied', JSON.stringify(started));
    assert.equal(started.policyReady, true);
    assert.equal(started.desiredRunning, true);
  });
  const formats = [
    ['/v1/chat/completions', { model: 'cli-test-model', messages: [{ role: 'user', content: 'test' }] }, value => value.choices?.[0]?.message?.content],
    ['/v1/responses', { model: 'cli-test-model', input: 'test', store: false }, value => JSON.stringify(value.output)],
    ['/v1/messages', { model: 'cli-test-model', messages: [{ role: 'user', content: 'test' }], max_tokens: 32 }, value => JSON.stringify(value.content)],
    ['/v1beta/models/cli-test-model:generateContent', { contents: [{ role: 'user', parts: [{ text: 'test' }] }] }, value => JSON.stringify(value.candidates)],
    ['/v1/models/cli-test-model:generateContent', { contents: [{ role: 'user', parts: [{ text: 'test' }] }] }, value => JSON.stringify(value.candidates)],
  ];
  for (const [path, body, content] of formats) await check(`inference ${path}`, async () => {
    const { value } = await api('POST', path, body, { extra: ['--key-file', gatewayKeyFile] });
    assert.ok(content(value)?.includes('cli-loopback-ok'));
    assert.equal(arrivals.at(-1).model, 'vendor-cli-model');
  });
  await check('streamed chat without buffering or JSON rewriting', async () => {
    const { value } = await api('POST', '/v1/chat/completions', { model: 'cli-test-model', stream: true, messages: [{ role: 'user', content: 'test' }] }, { extra: ['--key-file', gatewayKeyFile] });
    assert.equal(typeof value, 'string'); assert.ok(value.includes('data: ')); assert.ok(value.includes('[DONE]'));
  });
  for (const [path, body] of formats.slice(1)) await check(`stream conversion ${path}`, async () => {
    const route = path.endsWith(':generateContent') ? path.replace(':generateContent', ':streamGenerateContent') : path;
    const { value } = await api('POST', route, { ...body, stream: true }, { extra: ['--key-file', gatewayKeyFile] });
    assert.equal(typeof value, 'string'); assert.ok(value.includes('cli-loopback-ok'));
  });
  await check('unsupported stateful Responses rejected before upstream', async () => {
    const before = arrivals.length;
    await api('POST', '/v1/responses', { model: 'cli-test-model', input: 'test', store: true }, { success: false, extra: ['--key-file', gatewayKeyFile] });
    assert.equal(arrivals.length, before);
  });
  await check('credit settlement completes after JSON and stream inference', async () => {
    const billing = (await api('GET', `${prefix}/accounts/${accountId}/billing`)).value;
    assert.equal(billing.credits.pendingRequests, 0);
    assert.equal(billing.credits.unpricedRequests, 0);
    assert.ok(billing.credits.remaining < 60 && billing.credits.remaining > 59);
  });
  await check('encrypted transfer export and preview', async () => {
    transfer = (await api('POST', `${prefix}/accounts/transfer/export`, { bundlePassword: 'synthetic-bundle-password-123' })).value;
    assert.ok(transfer.bundle); assert.ok(!transfer.bundle.includes('vendor-cli-model'));
    const preview = (await api('POST', `${prefix}/accounts/transfer/preview`, { password: 'synthetic-bundle-password-123', bundle: transfer.bundle })).value;
    assert.ok(preview.items.length >= 1);
  });
  await check('wrong transfer password preserves state', async () => {
    const before = (await api('GET', `${prefix}/contract`)).value.revision;
    await api('POST', `${prefix}/accounts/transfer/import`, { password: 'wrong-password-123', bundle: transfer.bundle }, { cas: true, success: false });
    assert.equal((await api('GET', `${prefix}/contract`)).value.revision, before);
  });
  await check('encrypted transfer import through CLI', async () => {
    await api('POST', `${prefix}/accounts/transfer/import`, { password: 'synthetic-bundle-password-123', bundle: transfer.bundle }, { cas: true });
  });
  await check('settings mutations and port rebind', async () => {
    await api('PUT', `${prefix}/settings`, { routingMode: 'round-robin', conversationSticky: false }, { cas: true });
    const nextPort = await freePort();
    await api('PUT', `${prefix}/settings`, { gatewayPort: nextPort }, { cas: true });
    endpoint = `http://127.0.0.1:${nextPort}`;
    assert.equal((await api('GET', `${prefix}/settings`)).value.gatewayPort, nextPort);
  });
  await check('restart persists state and rejects prior generation', async () => {
    const revision = (await api('GET', `${prefix}/contract`)).value;
    const restartedPort = Number(new URL(endpoint).port);
    await stop(); await start(restartedPort);
    assert.equal((await api('GET', `${prefix}/accounts/${accountId}`)).value.name, 'CLI renamed');
    await api('PATCH', `${prefix}/accounts/${accountId}`, { expectedRevision: revision.revision, processGeneration: revision.processGeneration, name: 'wrong-generation' }, { success: false });
  });
  await check('stopped whole-directory backup restores configuration and credentials', async () => {
    const restorePort = Number(new URL(endpoint).port);
    await stop();
    const snapshot = join(root, 'profile.snapshot');
    const created = await invoke(['--data-dir', join(root, 'data'), 'backup', 'create', '--output', snapshot], undefined, 180000);
    assert.equal(created.code, 0, created.stderr);
    const receipt = parseJson(created.stdout);
    assert.equal(receipt.state, 'created');
    assert.ok(receipt.count > 0);
    const restoredDir = join(root, 'restored-data');
    const restoredRun = await invoke(['--data-dir', restoredDir, 'backup', 'restore', '--input', snapshot], undefined, 180000);
    assert.equal(restoredRun.code, 0, restoredRun.stderr);
    assert.equal(parseJson(restoredRun.stdout).state, 'restored');
    await start(restorePort, 'restored-data');
    const deadline = Date.now() + 120000;
    let runtimeText = '';
    let ready = false;
    while (Date.now() < deadline) {
      const output = join(root, 'runtime-barrier.json');
      const run = await invoke(['--endpoint', endpoint, 'api', 'GET', `${prefix}/external-integrations/cpa/runtime`, '--output', output]);
      try { runtimeText = await readFile(output, 'utf8'); } catch { runtimeText = run.stderr; }
      if (run.code === 0 && runtimeText) {
        try {
          if (restoredOwnedReady(parseJson(runtimeText))) {
            ready = true;
            break;
          }
        } catch {}
      }
      await new Promise(done => setTimeout(done, 200));
    }
    if (!ready) throw new Error(`restored CPA was not ready: ${runtimeText.slice(0, 4000)} ${serveLog.slice(0, 4000)}`);
    const restoredConfig = (await readFile(join(restoredDir, 'cpa', 'config.yaml'), 'utf8')).replaceAll('\\', '/');
    assert.match(restoredConfig, /restored-data\/cpa\/auth/);
    assert.equal((await api('GET', `${prefix}/accounts/${accountId}`)).value.name, 'CLI renamed');
    assert.equal((await api('GET', `${prefix}/settings`)).value.routingMode, 'round-robin');
    const restored = await api('POST', '/v1/chat/completions', { model: 'cli-test-model', messages: [{ role: 'user', content: 'restore test' }] }, { extra: ['--key-file', gatewayKeyFile] });
    assert.equal(restored.value?.choices?.[0]?.message?.content, 'cli-loopback-ok', `restored inference failed ${JSON.stringify(restored.value).slice(0, 800)} ${runtimeText.slice(0, 800)} ${serveLog.slice(0, 800)}`);
  });
  await check('invalid method and path escape fail locally', async () => {
    for (const path of ['http://example.com/', '//example.com/', '/dashboard/api/v3/accounts', '/dashboard/api/v4/../api/auth/status']) {
      const run = await invoke(['--endpoint', endpoint, 'api', 'GET', path]); assert.notEqual(run.code, 0, path);
    }
  });
  await check('unknown V4 path returns failure', async () => { await api('GET', `${prefix}/does-not-exist`, undefined, { success: false }); });
  await check('executable login bootstrap cookie and logout against session fixture', async () => {
    const actualHost = endpoint;
    endpoint = `http://127.0.0.1:${upstream.address().port}`;
    const session = join(root, 'auth-fixture-session.json');
    try {
      await api('GET', `${prefix}/settings`, undefined, { success: false });
      await api('POST', `${prefix}/auth/login`, { username: 'fixture-admin', password: 'synthetic-fixture-password' }, { cas: true, extra: ['--session-file', session] });
      assert.equal((await api('GET', `${prefix}/settings`, undefined, { extra: ['--session-file', session] })).value.authenticated, true);
      await api('POST', `${prefix}/auth/logout`, {}, { cas: true, extra: ['--session-file', session] });
      await api('GET', `${prefix}/settings`, undefined, { success: false, extra: ['--session-file', session] });
    } finally { endpoint = actualHost; }
  });
  await check('native browser open profile reset and managed-account cleanup', async () => {
    assert.equal((await api('GET', `${prefix}/browser/capabilities`)).value.mode, 'native');
    const created = (await api('POST', `${prefix}/accounts/managed`, { name: 'CLI browser acceptance' }, { cas: true })).value;
    const id = created.account.id;
    const dataRoot = resolve(join(root, dataDirName)).replaceAll('\\', '/');
    try {
      const opened = (await api('POST', `${prefix}/accounts/${id}/browser`, { target: 'console' }, { cas: true })).value;
      assert.equal(opened.mode, 'native'); assert.equal(opened.sessionToken, null);
      const capture = parseJson(await readFile(syntheticBrowser.capturePath, 'utf8'));
      assert.equal(capture.url, 'https://opencode.ai/auth', `captured URL ${capture.url}`);
      assert.ok(Array.isArray(capture.argv) && capture.argv.some(arg => String(arg).startsWith('--user-data-dir=')), `argv ${JSON.stringify(capture.argv)}`);
      const profileDir = String(capture.userDataDir || '').replaceAll('\\', '/');
      assert.ok(dataDirName === 'restored-data', `browser ran against ${dataDirName}, not restored-data`);
      assert.ok(profileDir.startsWith(dataRoot), `user-data-dir ${profileDir} is outside active ${dataRoot}`);
      assert.ok(profileDir.includes('/restored-data/') && profileDir.includes(`/browser-profiles/${id}`), `user-data-dir ${profileDir} is not the active restored profile`);
      assert.ok(!profileDir.includes('/data/browser-profiles/'), `user-data-dir ${profileDir} used the stopped pre-restore data directory`);
      assert.ok(Number.isInteger(capture.pid) && capture.pid > 0, 'synthetic browser pid missing');
      assert.ok(pidAlive(capture.pid), 'synthetic browser exited before profile delete');
      assert.ok(existsSync(capture.userDataDir), `active profile ${capture.userDataDir} was not created`);
      await api('DELETE', `${prefix}/accounts/${id}/browser-profile`, {}, { cas: true });
      const deadline = Date.now() + 8000;
      let stopped = !pidAlive(capture.pid);
      let deleted = !existsSync(capture.userDataDir);
      while ((!stopped || !deleted) && Date.now() < deadline) {
        await new Promise(done => setTimeout(done, 50));
        stopped = !pidAlive(capture.pid);
        deleted = !existsSync(capture.userDataDir);
      }
      assert.ok(stopped, `owned synthetic browser pid ${capture.pid} was not stopped`);
      assert.ok(deleted, `active restored profile ${capture.userDataDir} was not deleted`);
    } finally {
      await api('DELETE', `${prefix}/accounts/${id}`, {}, { cas: true });
    }
  });
} catch (error) {
  if (!results.some(row => row.status === 'FAIL')) results.push({ name: 'setup', status: 'FAIL', detail: error.message });
  process.exitCode = 1;
} finally {
  await stop();
  await new Promise(done => upstream.close(done));
  await writeFile(join(root, 'report.json'), JSON.stringify({ binary, binarySha256, inferenceUpstreamsLoopbackOnly: true, remoteInferenceCalls: 0, sessionFixture: 'CLI public-login bootstrap is tested against a loopback HTTP session fixture; backend auth is separately covered by Rust integration tests.', shutdownEvidence: 'Process termination and restart; graceful host cleanup is checked separately by Rust lifecycle tests.', results, arrivals }, null, 2));
  console.log(`Report: ${join(root, 'report.json')}`);
}
