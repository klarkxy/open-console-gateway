import {build} from 'esbuild';
import {readFile,writeFile,mkdir,readdir} from 'node:fs/promises';
import {fileURLToPath} from 'node:url';
import path from 'node:path';
const root=path.dirname(fileURLToPath(import.meta.url));
const result=await build({absWorkingDir:root,entryPoints:['src/extension.mjs'],bundle:true,write:false,platform:'node',target:'node22',format:'cjs',external:['vscode'],minify:true,legalComments:'eof',logLevel:'warning',metafile:true,define:{'import.meta.url':'__filename'}});
const file=path.join(root,'dist/extension.cjs');await mkdir(path.dirname(file),{recursive:true});
const bytes=result.outputFiles[0].contents;
if(process.argv.includes('--check')){if(!Buffer.from(await readFile(file)).equals(Buffer.from(bytes)))throw new Error('Copilot embedded runtime is stale. Run its build.');}
else await writeFile(file,bytes);
console.log(`Copilot runtime ${bytes.length} bytes`);

const packages=new Map();
for(const input of Object.keys(result.metafile.inputs)) {
  if(!input.includes('node_modules'))continue;
  let directory=path.dirname(path.resolve(root,input));
  while(directory!==path.dirname(directory)) {
    try { const manifest=JSON.parse(await readFile(path.join(directory,'package.json'),'utf8')); if(manifest.name&&manifest.version){packages.set(manifest.name,{directory,manifest});break;} } catch{}
    directory=path.dirname(directory);
  }
}
let notices='Bundled third-party software. Exact versions are pinned by pnpm-lock.yaml.\n';
for(const [name,{directory,manifest}] of [...packages].sort(([a],[b])=>a < b ? -1 : a > b ? 1 : 0)) {
  notices+=`\n--- ${name} ${manifest.version} (${manifest.license??'see license'}) ---\n`;
  const names=(await readdir(directory)).filter(n=>/^(licen[sc]e|notice|copying)([.-]|$)/i.test(n)).sort();
  for(const name of names)try{notices+=await readFile(path.join(directory,name),'utf8')+'\n';}catch{}
}
notices=notices.replace(/\r\n/g,'\n');
const noticeFile=path.join(root,'THIRD_PARTY_NOTICES');
if(process.argv.includes('--check')){if(await readFile(noticeFile,'utf8')!==notices)throw new Error('Copilot license notices are stale. Run its build.');}
else await writeFile(noticeFile,notices);
