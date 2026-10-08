import { open, lstat, realpath, mkdir, rename, unlink, writeFile } from 'node:fs/promises';
import { constants } from 'node:fs';
import path from 'node:path';
import { randomUUID } from 'node:crypto';
import { gatewayUrl } from './catalog.mjs';
export const SECRET_NAME = 'ocg.connection.v1';
export async function samePath(a,b) {
  // Filesystem identity preserves Windows case-sensitive directory semantics and namespace paths.
  await safeDirectory(a);await safeDirectory(b);
  return (await realpath(a)) === (await realpath(b));
}

async function safeDirectory(root) {
  for (let p=path.resolve(root); ; p=path.dirname(p)) {
    const s=await lstat(p); if (s.isSymbolicLink() || !s.isDirectory()) throw new Error('OCG storage cannot contain links.');
    if (path.dirname(p)===p) break;
  }
}
export async function readPrivate(file) {
  const before=await lstat(file); if (!before.isFile() || before.isSymbolicLink() || before.size > 65536) throw new Error('OCG handoff must be a bounded regular file.');
  if (process.platform !== 'win32' && (before.mode & 0o077)) throw new Error('OCG handoff must be private.');
  const handle=await open(file, constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0));
  try { const s=await handle.stat(); if (s.dev!==before.dev || s.ino!==before.ino || s.size > 65536) throw new Error('OCG handoff changed while opening.'); const bytes=await handle.readFile(); if(bytes.length>65536) throw new Error('OCG handoff is too large.'); return JSON.parse(bytes.toString('utf8')); }
  finally { await handle.close(); }
}
export async function writeAcknowledgement(root, value) {
  await safeDirectory(root);
  const temporary=path.join(root, `activation-${randomUUID()}.tmp`), dest=path.join(root,'activation.json');
  try { const s=await lstat(dest); if(!s.isFile() || s.isSymbolicLink()) throw new Error('OCG acknowledgment target is unsafe.'); } catch(e) { if(e.code!=='ENOENT') throw e; }
  try { await writeFile(temporary, JSON.stringify({schemaVersion:1,...value}), {flag:'wx',mode:0o600}); await rename(temporary,dest); } finally { await unlink(temporary).catch(()=>{}); }
}
export async function consumeHandoff(root, digest, secrets, isDisposed=()=>false) {
  await safeDirectory(root);
  const live=path.join(root,'credential-handoff.json');
  // Claim atomically; a later host reconnect stays at the live name and is never removed here.
  const claimed=path.join(root,'credential-handoff.claimed.json');
  let input;
  try { input=await readPrivate(claimed); }
  catch(e) {
    if(e.code!=='ENOENT') throw e;
    try { input=await readPrivate(live); } catch(e) { if(e.code==='ENOENT') return null; throw e; }
    // Rename only after host serializes updates with this ownership filename.
    await rename(live,claimed); input=await readPrivate(claimed);
  }
  if(input.schemaVersion!==1 || !/^[a-f0-9-]{36}$/i.test(input.connectionId ?? '') || !(await samePath(input.storagePath ?? '',root)) || input.runtimeDigest!==digest || !['connect','disconnect'].includes(input.operation)) throw new Error('OCG handoff does not match this Profile or extension. Reload VS Code and reconnect from OCG.');
  if(isDisposed()) return null;
  if(input.operation==='disconnect') await secrets.delete(SECRET_NAME);
  else {
    if(typeof input.key!=='string' || !input.key || input.key.length>32768 || /[\r\n\u0000]/.test(input.key)) throw new Error('OCG handoff Key is invalid.');
    await secrets.store(SECRET_NAME, JSON.stringify({connectionId:input.connectionId,gatewayV1Url:gatewayUrl(input.gatewayV1Url),key:input.key}));
  }
  await unlink(claimed);
  await writeAcknowledgement(root,{connectionId:input.connectionId,runtimeDigest:digest,status:input.operation==='connect'?'connected':'disconnected',modelCount:0,metadataMissing:[]});
  return input.operation;
}
export async function prepareStorage(root) { await mkdir(root,{recursive:true}); await safeDirectory(root); }
