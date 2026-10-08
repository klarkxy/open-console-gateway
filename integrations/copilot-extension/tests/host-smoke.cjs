const vscode=require('vscode');const assert=require('node:assert/strict');const http=require('node:http');const fs=require('node:fs/promises');const path=require('node:path');const crypto=require('node:crypto');const {execFile}=require('node:child_process');const {promisify}=require('node:util');const exec=promisify(execFile);
async function waitForModels(count){const start=Date.now();while(true){const models=await vscode.lm.selectChatModels({vendor:'open-console-gateway'});if(models.length===count)return models;if(Date.now()-start>15000)throw new Error(`Native model registry expected ${count}, received ${models.length}`);await new Promise(r=>setTimeout(r,50));}}
exports.run=async()=>{
 const extension=vscode.extensions.getExtension('open-console-gateway.copilot');assert.ok(extension,'extension discovered');
 const exported=await extension.activate();assert.equal(exported,undefined,'no credential-bearing activation exports');
 const root=process.env.OCG_COPILOT_SMOKE_STORAGE;assert.ok(root,'isolated storage supplied');
 let rows=['chat_completions','responses','messages'].map(p=>({id:'smoke-'+p,ocg:{schemaVersion:2,contextWindow:32000,maxOutputTokens:4000,toolCalling:true,inputModalities:['text'],protocols:{preferred:p,supported:[p]}}}));const requests=[];
 const server=http.createServer(async(req,res)=>{if(req.url==='/v1/models'){assert.equal(req.headers.authorization,'Bearer synthetic-host-key');res.writeHead(200,{'content-type':'application/json'}).end(JSON.stringify({data:rows}));return;}
  for await(const _ of req){}requests.push(req.url);res.writeHead(200,{'content-type':'text/event-stream'});
  const send=e=>res.write(`${e.type?'event: '+e.type+'\n':''}data: ${JSON.stringify(e)}\n\n`);
  if(req.url.startsWith('/v1/chat/completions')){send({choices:[{delta:{role:'assistant',content:'HOST-OK'},index:0}]});send({choices:[{delta:{},finish_reason:'stop',index:0}]});res.write('data: [DONE]\n\n');}
  else if(req.url.startsWith('/v1/responses')){send({type:'response.created',response:{id:'r',output:[],status:'in_progress'}});send({type:'response.output_item.added',output_index:0,item:{type:'message',id:'m',role:'assistant',content:[],status:'in_progress'}});send({type:'response.output_text.delta',item_id:'m',output_index:0,content_index:0,delta:'HOST-OK'});send({type:'response.completed',response:{id:'r',status:'completed',output:[],usage:{input_tokens:1,output_tokens:1,total_tokens:2}}});}
  else {send({type:'message_start',message:{id:'m',type:'message',role:'assistant',model:'smoke',content:[],stop_reason:null,usage:{input_tokens:1,output_tokens:0}}});send({type:'content_block_start',index:0,content_block:{type:'text',text:''}});send({type:'content_block_delta',index:0,delta:{type:'text_delta',text:'HOST-OK'}});send({type:'content_block_stop',index:0});send({type:'message_delta',delta:{stop_reason:'end_turn'},usage:{output_tokens:1}});send({type:'message_stop'});}
  res.end();});await new Promise(r=>server.listen(0,'127.0.0.1',r));
 const digest=crypto.createHash('sha256').update(await fs.readFile(path.join(extension.extensionPath,'dist/extension.cjs'))).digest('hex');const cid=crypto.randomUUID();
 try {
  const modelUrl=`http://127.0.0.1:${server.address().port}/v1`;
  const nativeArgs=[process.env.OCG_COPILOT_SMOKE_NATIVE_DATA,process.env.OCG_COPILOT_SMOKE_NATIVE_USER,process.env.OCG_COPILOT_SMOKE_NATIVE_EXTENSIONS,modelUrl];
  const nativeOptions={env:{...process.env,LOCALAPPDATA:process.env.OCG_COPILOT_SMOKE_NATIVE_LOCAL},windowsHide:true,timeout:90000,maxBuffer:65536};
  if(process.env.OCG_COPILOT_SMOKE_NATIVE){try{await exec(process.env.OCG_COPILOT_SMOKE_NATIVE,nativeArgs,nativeOptions);}catch(error){for(const name of ['activation.json','credential-handoff.claimed.json','credential-handoff.json']){try{const entry=JSON.parse(await fs.readFile(path.join(root,name),'utf8'));delete entry.key;console.error(name,JSON.stringify(entry));}catch{}}for(const receipt of await fs.readdir(path.join(nativeArgs[0],'applications/copilot/receipts')).catch(()=>[])){console.error('receipt',await fs.readFile(path.join(nativeArgs[0],'applications/copilot/receipts',receipt),'utf8'));}console.error('runtime',digest);throw error;}}
  else {
  await fs.writeFile(path.join(root,'credential-handoff.json'),JSON.stringify({schemaVersion:1,operation:'connect',connectionId:cid,storagePath:root,runtimeDigest:digest,gatewayV1Url:`http://127.0.0.1:${server.address().port}/v1`,key:'synthetic-host-key'}),{mode:0o600});await waitForModels(3);
  }
  const token=new vscode.CancellationTokenSource();
  const registered=await waitForModels(3);assert.equal(registered[0].maxInputTokens,28000,'authoritative native input capacity');
  for(const model of registered){let text='';const response=await model.sendRequest([vscode.LanguageModelChatMessage.User('ping')],{},token.token);for await(const part of response.stream){if(part instanceof vscode.LanguageModelTextPart)text+=part.value;}assert.equal(text,'HOST-OK');}
  rows=rows.slice(1);await vscode.commands.executeCommand('ocg.refresh');await waitForModels(2);
  await vscode.commands.executeCommand('ocg.disconnect');await waitForModels(0);
  const ack=JSON.parse(await fs.readFile(path.join(root,'activation.json'),'utf8'));assert.equal(ack.status,'disconnected');assert.ok(!JSON.stringify(ack).includes('synthetic-host-key'));
  if(process.env.OCG_COPILOT_SMOKE_NATIVE)await exec(process.env.OCG_COPILOT_SMOKE_NATIVE,['uninstall',...nativeArgs],nativeOptions);
  token.dispose();await fs.writeFile(process.env.OCG_COPILOT_SMOKE_RESULT,JSON.stringify({nativeInstallUninstall:!!process.env.OCG_COPILOT_SMOKE_NATIVE,extensionHost:true,registeredModels:registered.length,protocolRequests:requests.length,dynamicCatalog:true,secretStorageDisconnect:true}));
 } finally{server.closeAllConnections();await new Promise(r=>server.close(r));}
};
