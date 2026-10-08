import * as vscode from 'vscode';
import { readFile } from 'node:fs/promises';
import { watch } from 'node:fs';
import { createHash,randomUUID } from 'node:crypto';
import { OcgProvider } from './provider.mjs';
import { consumeHandoff,prepareStorage,SECRET_NAME,writeAcknowledgement } from './handoff.mjs';
import { gatewayUrl } from './catalog.mjs';
export async function activate(context) {
  const root=context.globalStorageUri.fsPath;
  const digest=createHash('sha256').update(await readFile(context.extensionPath+'/dist/extension.cjs')).digest('hex');
  await prepareStorage(root);
  let disposed=false,epoch=0,queue=Promise.resolve();
  const readConnection=async()=>{const value=await context.secrets.get(SECRET_NAME);if(!value)return null;try{const c=JSON.parse(value);if(typeof c.key!=='string'||!c.key)return null;gatewayUrl(c.gatewayV1Url);return c;}catch{return null;}};
  const status=async(catalog,c)=>{if(!disposed)await writeAcknowledgement(root,{connectionId:c.connectionId,runtimeDigest:digest,status:catalog.metadataMissing.length?'metadata_required':'connected',modelCount:catalog.models.length,metadataMissing:catalog.metadataMissing});};
  const readEffort=model=>{const saved=context.globalState.get('ocg.reasoningEfforts',{})[model.id];return model.apiModel.api!=='anthropic-messages'&&model.ocgMetadata.reasoning!==false&&Object.hasOwn(model.ocgMetadata.reasoningEfforts??{},saved)?saved:undefined;};
  const provider=new OcgProvider(vscode,readConnection,status,fetch,readEffort);
  const refresh=async()=>{const generation=epoch;try{const catalog=await provider.catalog();if(generation!==epoch||disposed)return;provider.changed.fire();return catalog;}catch(e){const c=await readConnection();if(c&&generation===epoch&&!disposed)await writeAcknowledgement(root,{connectionId:c.connectionId,runtimeDigest:digest,status:e.status===401||e.status===403?'authentication_failed':'unavailable',modelCount:0,metadataMissing:[]});throw new Error('OCG connection failed. Check the URL and Key in OCG.');}};
  const enqueue=work=>{queue=queue.catch(()=>{}).then(async()=>{if(!disposed)return work();});return queue;};
  const drain=()=>enqueue(async()=>{if(disposed)return;const action=await consumeHandoff(root,digest,context.secrets,()=>disposed);if(action){epoch++;provider.clear();if(action==='connect')await refresh();}});
  await drain().catch(()=>vscode.window.showWarningMessage('OCG connection is pending. Reload this Profile or reconnect from OCG.'));
  const watcher=watch(root,(_event,name)=>{if(String(name)==='credential-handoff.json')void drain().catch(()=>vscode.window.showWarningMessage('OCG connection could not be imported. Reconnect from OCG.'));});
  context.subscriptions.push({dispose(){disposed=true;epoch++;watcher.close();provider.dispose();}},vscode.lm.registerLanguageModelChatProvider('open-console-gateway',provider),
    vscode.window.registerUriHandler({async handleUri(uri){if(uri.path==='/connect')await drain();}}),
    vscode.commands.registerCommand('ocg.refresh',async()=>{const c=await refresh();if(c?.metadataMissing.length)await vscode.window.showWarningMessage('Some OCG models need context/output metadata: '+c.metadataMissing.join(', ')+'. Set it once in OCG Aliases → model capability.');}),
    vscode.commands.registerCommand('ocg.disconnect',async()=>enqueue(async()=>{epoch++;provider.clear();const c=await readConnection();await context.secrets.delete(SECRET_NAME);if(c)await writeAcknowledgement(root,{connectionId:c.connectionId,runtimeDigest:digest,status:'disconnected',modelCount:0,metadataMissing:[]});})),
    vscode.commands.registerCommand('ocg.reasoning',async()=>{
      const catalog=await provider.catalog();const models=catalog.models.filter(m=>m.apiModel.api!=='anthropic-messages'&&m.ocgMetadata.reasoning!==false&&Object.keys(m.ocgMetadata.reasoningEfforts??{}).length);
      const model=await vscode.window.showQuickPick(models.map(m=>({label:m.name,description:m.id,model:m})),{title:'OCG reasoning effort — select model'});if(!model)return;
      const choice=await vscode.window.showQuickPick([{label:'Model default',selector:null},...Object.entries(model.model.ocgMetadata.reasoningEfforts).filter(([,wire])=>typeof wire==='string').map(([selector,wire])=>({label:selector,description:wire,selector}))],{title:'OCG reasoning effort'});if(!choice)return;
      const preferences={...context.globalState.get('ocg.reasoningEfforts',{})};if(choice.selector===null)delete preferences[model.model.id];else preferences[model.model.id]=choice.selector;await context.globalState.update('ocg.reasoningEfforts',preferences);
    }),
    vscode.commands.registerCommand('ocg.connect',async()=>{
      const c=await readConnection();const url=await vscode.window.showInputBox({title:'Connect Open Console Gateway',prompt:'OCG /v1 URL',value:c?.gatewayV1Url??'http://127.0.0.1:9042/v1',ignoreFocusOut:true,validateInput:v=>{try{gatewayUrl(v);return null;}catch(e){return e.message;}}});if(!url)return;
      const key=await vscode.window.showInputBox({title:'OCG Key',password:true,ignoreFocusOut:true,prompt:'Stored only in this VS Code Profile SecretStorage'});if(!key)return;
      await enqueue(async()=>{epoch++;provider.clear();const connection={connectionId:randomUUID(),gatewayV1Url:gatewayUrl(url),key};await context.secrets.store(SECRET_NAME,JSON.stringify(connection));await refresh();});
    }));
  // No credential-bearing provider object is exported to other extensions.
}
