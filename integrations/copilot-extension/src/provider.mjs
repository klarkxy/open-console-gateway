import { openAICompletionsApi } from '@earendil-works/pi-ai/api/openai-completions.lazy';
import { openAIResponsesApi } from '@earendil-works/pi-ai/api/openai-responses.lazy';
import { anthropicMessagesApi } from '@earendil-works/pi-ai/api/anthropic-messages.lazy';
import { normalizeContext } from '@earendil-works/pi-ai/utils/transcript';
import { createHash } from 'node:crypto';
import { encode as encodeRaw } from 'gpt-tokenizer/encoding/o200k_base';
import { fetchCatalog } from './catalog.mjs';
const encode=text=>encodeRaw(text,{disallowedSpecial:new Set()});
class ProviderError extends Error {}
const APIS={ 'openai-completions':openAICompletionsApi(), 'openai-responses':openAIResponsesApi(), 'anthropic-messages':anthropicMessagesApi() };
const usage=()=>({input:0,output:0,cacheRead:0,cacheWrite:0,totalTokens:0,cost:{input:0,output:0,cacheRead:0,cacheWrite:0,total:0}});
export function textPart(v) { if(typeof v?.value==='string') return v.value; throw new ProviderError('Unsupported tool result part.'); }
export function convertMessages(vscode,messages,model) {
  const out=[], systems=[]; const calls=new Map();
  for(const message of messages) {
    const system=message.role===vscode.LanguageModelChatMessageRole.System;
    const assistant=message.role===vscode.LanguageModelChatMessageRole.Assistant;
    const content=[];
    for(const p of message.content) {
      if(p instanceof vscode.LanguageModelTextPart) { if(system) systems.push(p.value); else content.push({type:'text',text:p.value}); }
      else if(p instanceof vscode.LanguageModelToolCallPart) { if(!assistant || !model.capabilities.toolCalling) throw new ProviderError('This model cannot accept tool calls.'); calls.set(p.callId,p.name); content.push({type:'toolCall',id:p.callId,name:p.name,arguments:p.input}); }
      else if(p instanceof vscode.LanguageModelToolResultPart) {
        if(assistant || system) throw new ProviderError('Unexpected tool result ordering.');
        const name=calls.get(p.callId); if(!name) throw new ProviderError('Tool result has no matching call.');
        out.push({role:'toolResult',toolCallId:p.callId,toolName:name,content:p.content.map(x=>convertResult(vscode,x,model)),isError:!!p.isError,timestamp:Date.now()});
      }
      else if(vscode.LanguageModelDataPart && p instanceof vscode.LanguageModelDataPart) {
        if(system || assistant || !model.capabilities.imageInput || !/^image\/(png|jpeg|webp|gif)$/.test(p.mimeType)) throw new ProviderError('This model cannot accept this data part.');
        content.push({type:'image',data:Buffer.from(p.data).toString('base64'),mimeType:p.mimeType});
      } else throw new ProviderError('Unsupported VS Code message part.');
    }
    if(content.length) out.push(assistant ? {role:'assistant',content,api:model.apiModel.api,provider:'ocg',model:model.id,usage:usage(),stopReason:content.some(p=>p.type==='toolCall')?'toolUse':'stop',timestamp:Date.now()} : {role:'user',content,timestamp:Date.now()});
  }
  return {systemPrompt:systems.join('\n\n') || undefined,messages:out};
}
function convertResult(v,p,model) {
  if(p instanceof v.LanguageModelTextPart) return {type:'text',text:p.value};
  if(v.LanguageModelDataPart && p instanceof v.LanguageModelDataPart && model.capabilities.imageInput && /^image\/(png|jpeg|gif|webp)$/.test(p.mimeType)) return {type:'image',data:Buffer.from(p.data).toString('base64'),mimeType:p.mimeType};
  throw new ProviderError('Unsupported tool result content.');
}
export function countTokens(value) {
  if(typeof value==='string') return encode(value).length;
  let count=8;
  for(const part of value.content ?? []) {
    if(typeof part.value==='string') count+=encode(part.value).length;
    else if(part.data) count+=Math.max(4096, Math.ceil(part.data.byteLength/512));
    else if(Array.isArray(part.content)) count+=part.content.reduce((n,p)=>n+countTokens({content:[p]}),0);
    else count+=encode(JSON.stringify({callId:part.callId,name:part.name,input:part.input})).length;
  }
  return count;
}
export class OcgProvider {
  constructor(vscode,readConnection,onCatalog=async()=>{},fetcher=fetch,readEffort=()=>undefined) {
    this.vscode=vscode;this.readConnection=readConnection;this.onCatalog=onCatalog;this.fetcher=fetcher;this.readEffort=readEffort;
    this.changed=new vscode.EventEmitter();this.onDidChangeLanguageModelChatInformation=this.changed.event;this.controllers=new Set();this.replay=new Map();this.disposed=false;
  }
  clear() { for(const c of this.controllers)c.abort(); this.replay.clear(); this.changed.fire(); }
  dispose() { this.disposed=true;this.clear();this.changed.dispose(); }
  async catalog(signal) {
    const c=await this.readConnection(); if(!c || this.disposed) return {models:[],metadataMissing:[]};
    const result=await fetchCatalog(c,signal,this.fetcher); const current=await this.readConnection(); if(current?.connectionId!==c.connectionId || current?.key!==c.key)throw new ProviderError('OCG connection changed during discovery.'); if(this.disposed)throw new ProviderError('OCG provider was disposed.'); await this.onCatalog(result,c); return result;
  }
  async provideLanguageModelChatInformation(_options,token) {
    const ctl=new AbortController();this.controllers.add(ctl);const listener=token.onCancellationRequested(()=>ctl.abort());if(token.isCancellationRequested)ctl.abort();
    try { return (await this.catalog(ctl.signal)).models.map(({apiModel,ocgMetadata,...info})=>info); }
    finally {listener.dispose();this.controllers.delete(ctl);}
  }
  async provideTokenCount(_model,text,token) { if(token.isCancellationRequested)throw new ProviderError('Request cancelled.');return countTokens(text); }
  async provideLanguageModelChatResponse(selected,messages,options,progress,token) {
    const ctl=new AbortController(); this.controllers.add(ctl);const cancellation=token.onCancellationRequested(()=>ctl.abort());if(token.isCancellationRequested)ctl.abort();
    try {
      const connection=await this.readConnection();if(!connection)throw new ProviderError('Connect OCG before selecting a model.');
      const catalog=await fetchCatalog(connection,ctl.signal,this.fetcher);
      const model=catalog.models.find(m=>m.id===selected.id);if(!model)throw new ProviderError('This OCG model is unavailable or needs metadata. Refresh models in OCG.');
      if(model.maxInputTokens!==selected.maxInputTokens || model.maxOutputTokens!==selected.maxOutputTokens || model.capabilities.imageInput!==selected.capabilities?.imageInput || model.capabilities.toolCalling!==selected.capabilities?.toolCalling) {this.changed.fire();throw new ProviderError('OCG model capabilities changed. Reselect the model.');}
      const context=convertMessages(this.vscode,messages,model);
      for(const m of context.messages) if(m.role==='assistant') { const saved=this.replay.get(replayKey(model.id,model.apiModel.api,m.content)); if(saved) m.content=saved; }
      const tools=options.tools??[]; if(tools.length && !model.capabilities.toolCalling)throw new ProviderError('This model does not advertise tool support.');
      context.tools=tools.map(t=>({name:t.name,description:t.description,parameters:t.inputSchema??{type:'object',properties:{}}}));
      if(messages.reduce((n,m)=>n+countTokens(m),0)+encode(JSON.stringify(context.tools)).length>model.maxInputTokens)throw new ProviderError('Request exceeds the OCG model input limit.');
      const reasoning=options.modelOptions?.reasoningEffort ?? this.readEffort(model);
      const request={apiKey:connection.key,headers:{Authorization:`Bearer ${connection.key}`},signal:ctl.signal,fetch:(url,init)=>{
        const u=new URL(typeof url==='string'?url:url.url??url.toString()),base=new URL(connection.gatewayV1Url);if(u.origin!==base.origin || !['/chat/completions','/responses','/messages'].some(s=>u.pathname===base.pathname+s))throw new ProviderError('OCG request endpoint is invalid.');return this.fetcher(url,{...init,redirect:'error'});
      },maxRetries:0,timeoutMs:120000,maxTokens:model.maxOutputTokens,cacheRetention:'none',transport:'sse',
        onPayload:payload=>{ if(model.apiModel.api==='openai-responses')payload.store=false;
          if(model.apiModel.api!=='anthropic-messages'){if(context.tools.length && typeof model.ocgMetadata.parallelToolCalls==='boolean')payload.parallel_tool_calls=model.ocgMetadata.parallelToolCalls;else delete payload.parallel_tool_calls;}else if(context.tools.length&&model.ocgMetadata.parallelToolCalls===false)payload.tool_choice={...(payload.tool_choice??{type:'auto'}),disable_parallel_tool_use:true};
          if(reasoning!==undefined){const efforts=model.ocgMetadata.reasoningEfforts;const effort=efforts&&Object.hasOwn(efforts,reasoning)?efforts[reasoning]:undefined;if(typeof effort!=='string'||!effort)throw new ProviderError('OCG does not advertise this reasoning effort.');if(model.apiModel.api==='anthropic-messages')throw new ProviderError('Messages reasoning effort requires a declared budget.');payload.reasoning_effort=effort;if(model.apiModel.api==='openai-responses'){delete payload.reasoning_effort;payload.reasoning={effort};}}
          return payload; }};
      if(tools.length && options.toolMode!==undefined && options.toolMode===this.vscode.LanguageModelChatToolMode?.Required) request.toolChoice=model.apiModel.api==='anthropic-messages'?'any':'required';
      const stream=APIS[model.apiModel.api].stream(model.apiModel,normalizeContext(context),request);
      for await(const event of stream) {
        if(ctl.signal.aborted || this.disposed)throw new ProviderError('Request cancelled.');
        if(event.type==='text_delta') progress.report(new this.vscode.LanguageModelTextPart(event.delta));
        else if(event.type==='toolcall_end')progress.report(new this.vscode.LanguageModelToolCallPart(event.toolCall.id,event.toolCall.name,event.toolCall.arguments));
        else if(event.type==='error')throw new ProviderError('OCG model request failed. Check OCG Logs or reconnect.');
      }
      const final=await stream.result(); if(!ctl.signal.aborted && !this.disposed && !['error','aborted'].includes(final.stopReason)) { this.replay.set(replayKey(model.id,model.apiModel.api,final.content),final.content); if(this.replay.size>128)this.replay.delete(this.replay.keys().next().value); } if(final.stopReason==='error'||final.stopReason==='aborted')throw new ProviderError('OCG model request failed or was cancelled.');
    } catch(e) { if(ctl.signal.aborted)throw new ProviderError('Request cancelled.'); if(e?.status===401||e?.status===403)throw new ProviderError('OCG authentication failed. Reconnect.');
      // SDK errors may echo arbitrary upstream bodies/headers. Only extension-authored messages cross the host boundary.
      if(e instanceof ProviderError)throw e;
      throw new ProviderError('OCG request failed. Check the connection and OCG Logs.');
    } finally {cancellation.dispose();this.controllers.delete(ctl);}
  }
}

function replayKey(model,api,content) {
  const text=content.filter(p=>p.type==='text').map(p=>p.text).join('');
  const tools=content.filter(p=>p.type==='toolCall').map(p=>({id:p.id,name:p.name,arguments:p.arguments}));
  return createHash('sha256').update(JSON.stringify({model,api,text,tools})).digest('hex');
}
