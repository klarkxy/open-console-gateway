import {mkdtemp,mkdir,cp,readFile,rm,writeFile}from'node:fs/promises';import path from'node:path';import os from'node:os';import {createHash,randomUUID}from'node:crypto';import {spawn}from'node:child_process';import {fileURLToPath}from'node:url';
const root=fileURLToPath(new URL('..',import.meta.url));const install=process.env.OCG_SMOKE_CODE_ROOT??path.join(process.env.LOCALAPPDATA??'','Programs','Microsoft VS Code Insiders');let exe=path.join(install,'Code - Insiders.exe');
const wrapper=await readFile(path.join(install,'bin/code-insiders.cmd'),'utf8');const relative=wrapper.split('"').find(p=>p.startsWith('%~dp0')&&p.endsWith('resources\\app\\out\\cli.js'));if(!relative)throw new Error('Cannot resolve trusted Insiders CLI');const cli=path.resolve(install,'bin',relative.slice('%~dp0'.length));if(!cli.startsWith(install+path.sep))throw new Error('CLI escapes installation');
const stage=await mkdtemp(path.join(os.tmpdir(),'ocg-copilot-host-'));const extension=path.join(stage,'extension');await mkdir(extension);
for(const name of ['package.json','dist','tests/host-smoke.cjs'])await cp(path.join(root,'integrations/copilot-extension',name),path.join(extension,name),{recursive:true});
if(process.argv.includes('--isolated-runtime')) {
 const selected=path.dirname(path.dirname(path.dirname(path.dirname(cli))));
 const relativeVersion=path.relative(install,selected);
 if(!relativeVersion || relativeVersion.includes('..') || relativeVersion.includes(path.sep))throw new Error('Unrecognized VS Code version directory');
 const copy=path.join(stage,'local/Programs/Microsoft VS Code Insiders');await mkdir(copy,{recursive:true});
 await mkdir(path.join(copy,'bin'));await cp(path.join(install,'bin/code-insiders.cmd'),path.join(copy,'bin/code-insiders.cmd'));
 await cp(exe,path.join(copy,'Code - Insiders.exe'));await cp(selected,path.join(copy,relativeVersion),{recursive:true});
 const copiedExe=path.join(copy,'Code - Insiders.exe');
 const hash=async p=>createHash('sha256').update(await readFile(p)).digest('hex');
 if(await hash(exe)!==await hash(copiedExe))throw new Error('Isolated executable differs');
 const main=path.join(relativeVersion,'resources/app/out/mainImpl.js');
 if(await hash(path.join(install,main))!==await hash(path.join(copy,main)))throw new Error('Isolated application code differs');
 const productPath=path.join(copy,relativeVersion,'resources/app/product.json');const product=JSON.parse(await readFile(productPath,'utf8'));
 product.win32MutexName='ocg-copilot-smoke-'+randomUUID();await writeFile(productPath,JSON.stringify(product));exe=copiedExe;
}
const user=path.join(stage,'user'),extensions=path.join(stage,'extensions'),storage=path.join(user,'User/globalStorage/open-console-gateway.copilot'),result=path.join(stage,'result.json');
await mkdir(path.join(user,'User'),{recursive:true});await writeFile(path.join(user,'User/settings.json'),JSON.stringify({'update.mode':'none','workbench.startupEditor':'none'}));
let output="";
try{await new Promise((resolve,reject)=>{const env={...process.env,OCG_COPILOT_SMOKE_STORAGE:storage,OCG_COPILOT_SMOKE_RESULT:result,OCG_COPILOT_SMOKE_NATIVE:process.env.OCG_COPILOT_SMOKE_NATIVE,OCG_COPILOT_SMOKE_NATIVE_DATA:path.join(stage,'ocg-data'),OCG_COPILOT_SMOKE_NATIVE_LOCAL:path.join(stage,'local'),OCG_COPILOT_SMOKE_NATIVE_USER:user,OCG_COPILOT_SMOKE_NATIVE_EXTENSIONS:extensions};delete env.ELECTRON_RUN_AS_NODE;delete env.VSCODE_IPC_HOOK_CLI;delete env.VSCODE_CLI;const child=spawn(exe,['--user-data-dir',user,'--extensions-dir',extensions,'--extensionDevelopmentPath',extension,'--extensionTestsPath',path.join(extension,'tests/host-smoke.cjs'),'--disable-extensions','--skip-welcome','--skip-release-notes','--disable-workspace-trust'],{env,windowsHide:true,stdio:['ignore','pipe','pipe']});for(const s of [child.stdout,child.stderr])s.on('data',d=>{output=(output+d).slice(-16000);});const timer=setTimeout(()=>{child.kill();reject(new Error('VS Code extension-host smoke timed out\n'+output));},120000);child.on('error',e=>{clearTimeout(timer);reject(e);});child.on('close',code=>{clearTimeout(timer);if(code===0)resolve();else reject(new Error('VS Code extension-host smoke failed '+code+'\n'+output));});});try{console.log(await readFile(result,'utf8'));}catch{throw new Error('VS Code exited without extension test evidence\n'+output);}}
finally{if(stage.startsWith(path.join(os.tmpdir(),'ocg-copilot-host-')))await rm(stage,{recursive:true,force:true,maxRetries:3,retryDelay:300});}
