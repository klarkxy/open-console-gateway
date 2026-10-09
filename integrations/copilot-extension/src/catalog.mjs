import { isIP } from 'node:net';
export const PROTOCOLS = { chat_completions: "openai-completions", responses: "openai-responses", messages: "anthropic-messages" };
export function gatewayUrl(value) {
  const u = new URL(value);
  if (!['http:', 'https:'].includes(u.protocol) || u.username || u.password || u.search || u.hash || !u.pathname.replace(/\/$/, '').endsWith('/v1')) throw new Error('Use an OCG /v1 URL without credentials, query, or fragment.');
  const host = u.hostname.replace(/^\[|\]$/g, '');
  const privateHost=host==='localhost' || (isIP(host)===4 && (/^127\./.test(host)||/^10\./.test(host)||/^192\.168\./.test(host)||/^172\.(1[6-9]|2\d|3[01])\./.test(host))) || (isIP(host)===6 && (host==='::1'||/^f[cd]/i.test(host)||/^fe[89ab]/i.test(host)));
  if (u.protocol === 'http:' && !privateHost) throw new Error('Remote OCG connections require HTTPS.');
  return u.toString().replace(/\/$/, '');
}
export function projectCatalog(payload, baseUrl) {
  if (!Array.isArray(payload?.data) || payload.data.length > 10000) throw new Error('OCG returned an invalid model directory.');
  const models = [], metadataMissing = [], seen = new Set();
  for (const row of payload.data) {
    const m = row?.ocg, id = row?.id;
    if (typeof id !== 'string' || !id.trim() || id.length > 512 || /[\u0000-\u001f\u007f]/.test(id) || seen.has(id)) throw new Error('OCG returned invalid or duplicate model identities.');
    seen.add(id);
    if (m?.schemaVersion !== 2 || !PROTOCOLS[m.protocols?.preferred] || !Array.isArray(m.protocols.supported) || !m.protocols.supported.includes(m.protocols.preferred) || !m.protocols.supported.every(p=>PROTOCOLS[p]) || new Set(m.protocols.supported).size!==m.protocols.supported.length) throw new Error('OCG model protocol metadata is incompatible.');
    const context = m.contextWindow, output = m.maxOutputTokens;
    if (![context, output].every(v => Number.isSafeInteger(v) && v > 0) || output >= context) { metadataMissing.push(id); continue; }
    if (Array.isArray(m.outputModalities) && !m.outputModalities.includes('text')) continue;
    if(m.inputModalities!=null&&!Array.isArray(m.inputModalities))throw new Error('OCG model modality metadata is incompatible.');
    // Public aliases are the client-facing identity; upstream display names can collide.
    const name = id;
    models.push({ id, name, family: id, version: '1', maxInputTokens: context - output, maxOutputTokens: output,
      tooltip: 'Limits supplied by Open Console Gateway', detail: 'OCG',
      capabilities: { toolCalling: m.toolCalling === true, imageInput: m.inputModalities?.includes('image') === true },
      apiModel: { id, name, provider: 'ocg', api: PROTOCOLS[m.protocols.preferred], baseUrl: m.protocols.preferred === 'messages' ? baseUrl.replace(/\/v1$/, '') : baseUrl,
        contextWindow: context, maxTokens: output, reasoning: m.reasoning === true, input: m.inputModalities?.includes('image') ? ['text','image'] : ['text'],
        cost: { input:0, output:0, cacheRead:0, cacheWrite:0 },
        compat: { supportsDeveloperRole: false, supportsStore: false, supportsUsageInStreaming: true, supportsReasoningEffort: !!m.reasoningEfforts,
          maxTokensField: 'max_tokens', thinkingFormat: 'openai' } }, ocgMetadata: m });
  }
  return {models, metadataMissing};
}
export async function fetchCatalog(connection, signal, fetcher = fetch) {
  const url = gatewayUrl(connection.gatewayV1Url);
  const response = await fetcher(`${url}/models`, { headers: { Authorization: `Bearer ${connection.key}` }, signal: AbortSignal.any([signal ?? new AbortController().signal, AbortSignal.timeout(15000)]), redirect: 'error' });
  if (!response.ok) { const e = new Error(response.status === 401 || response.status === 403 ? 'OCG authentication failed. Reconnect with an enabled Key.' : 'OCG model directory is unavailable.'); e.status = response.status; throw e; }
  const reader = response.body?.getReader(); if (!reader) throw new Error('OCG returned an empty model directory.');
  const chunks = []; let size = 0;
  try { while (true) { const {value, done} = await reader.read(); if (done) break; size += value.length; if (size > 8 * 1024 * 1024) throw new Error('OCG model directory is too large.'); chunks.push(value); } }
  finally { await reader.cancel().catch(() => {}); }
  try { return projectCatalog(JSON.parse(Buffer.concat(chunks).toString('utf8')), url); } catch (e) { if (e instanceof SyntaxError) throw new Error('OCG returned an invalid model directory.'); throw e; }
}
