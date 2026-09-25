import assert from "node:assert/strict";
import test from "node:test";
import { Fragment, createRenderer, h, nextTick, ref, renderSlot } from "vue";
import { useRoutingCardLayout, type RoutingCardLayoutDraft } from "./useRoutingCardLayout.ts";
import type { MutationExpectation } from "../api/generated/dashboard-v3.ts";
function deferred() { let resolve!: () => void; let reject!: (e: Error) => void; const promise = new Promise<void>((a,b) => { resolve=a; reject=b; }); return { resolve, reject, promise }; }
function fixture() {
  const committedLayout = ref<RoutingCardLayoutDraft[]>([{id:"a",destinationId:"a",credentialIds:["a1","a2"]},{id:"empty",destinationId:"a",credentialIds:[]},{id:"b",destinationId:"b",credentialIds:["b1"]}]);
  const revision = ref<MutationExpectation|null>({expectedRevision:4,processGeneration:99});
  const draft = ref<RoutingCardLayoutDraft[]|null>(null); const busy=ref(false);
  const requests: {layout:RoutingCardLayoutDraft[];revision:MutationExpectation}[]=[];
  const pending=deferred(); let notifications=0; let refreshes=0;
  const notify=()=>{notifications++;};
  const layout=useRoutingCardLayout({committedLayout,revision,draft,busy,
    message:{success:notify,error:notify,warning:notify} as unknown as Parameters<typeof useRoutingCardLayout>[0]["message"],
    refreshConflict: async()=>{refreshes++;},
    save:async(cards,revision)=>{ requests.push({layout:cards,revision}); await pending.promise; },
  });
  return {committedLayout,revision,draft,busy,requests,pending,layout,notifications:()=>notifications,refreshes:()=>refreshes};
}
test("keyboard moves a complete card across an empty card under the captured revision", async()=>{
  const f=fixture();
  const saving=f.layout.handleCardKeydown({key:"ArrowDown",preventDefault(){}} as KeyboardEvent,"a");
  assert.deepEqual(f.requests[0].layout.map(c=>c.id),["empty","a","b"]);
  assert.deepEqual(f.requests[0].layout[1].credentialIds,["a1","a2"]);
  assert.deepEqual(f.requests[0].revision,{expectedRevision:4,processGeneration:99});
  assert.deepEqual(f.committedLayout.value.map(c=>c.id),["a","empty","b"]);
  f.pending.resolve();await saving;assert.equal(f.draft.value,null);f.layout.revertActiveArrangement();
});
test("filters and other busy operations prevent layout saves", async()=>{
  const f=fixture();f.busy.value=true;
  assert.equal(await f.layout.applyLayoutChange([...f.committedLayout.value].reverse()),false);
  assert.equal(f.requests.length,0);assert.equal(f.draft.value,null);f.layout.revertActiveArrangement();
});
test("failed saves drop previews without restoring stale committed data", async()=>{
  const f=fixture();const saving=f.layout.applyLayoutChange([...f.committedLayout.value].reverse());
  f.committedLayout.value[0].credentialIds.push("new-key");f.pending.reject(new Error("offline"));
  assert.equal(await saving,false);assert.equal(f.draft.value,null);
  assert.deepEqual(f.committedLayout.value[0].credentialIds,["a1","a2","new-key"]);f.layout.revertActiveArrangement();
});
test("unmount and logout suppress late messages and do not revive a layout", async()=>{
  for(const unmount of [true,false]){
    const f=fixture();const saving=f.layout.applyLayoutChange([...f.committedLayout.value].reverse());
    if(unmount)f.layout.revertActiveArrangement();else f.revision.value=null;
    f.pending.reject(new Error("late failure"));await saving;
    assert.equal(f.draft.value,null);assert.equal(f.notifications(),0);f.layout.revertActiveArrangement();
  }
});
function stubPreviewFrame() {
  let frames: FrameRequestCallback[] = [];
  Object.defineProperty(globalThis,"requestAnimationFrame",{configurable:true,value:(cb:FrameRequestCallback)=>{frames.push(cb);return frames.length;}});
  Object.defineProperty(globalThis,"cancelAnimationFrame",{configurable:true,value:(id:number)=>{frames=frames.filter((_,index)=>index!==id-1);}});
  return { flush:()=>{ const cb=frames.shift(); if(cb) cb(0); }, pending:()=>frames.length };
}
test("pointer preview moves rows inside their card and cancels if the saved revision changes", async()=>{
  const handlers = new Map<string, (event:PointerEvent)=>unknown>();
  Object.defineProperty(globalThis,"window",{configurable:true,value:{addEventListener:(name:string,fn:(e:PointerEvent)=>unknown)=>handlers.set(name,fn),removeEventListener:(name:string)=>handlers.delete(name)}});
  Object.defineProperty(globalThis,"document",{configurable:true,value:{elementFromPoint:()=>({closest:(selector:string)=>selector.includes("account-card")?{dataset:{accountId:"a"}}:{dataset:{credentialId:"a2"}}})}});
  const frame=stubPreviewFrame();
  const f=fixture();const handle={setPointerCapture(){},hasPointerCapture(){return true;},releasePointerCapture(){},closest(){return null;}};
  const event={isPrimary:true,pointerType:"mouse",button:0,pointerId:1,currentTarget:handle,preventDefault(){},clientX:0,clientY:0} as unknown as PointerEvent;
  f.layout.startCredentialDrag(event,"a","a1");handlers.get("pointermove")!(event);
  const beforeFlush=f.draft.value;
  assert.equal(frame.pending(),1);assert.equal(beforeFlush,null);
  frame.flush();
  assert.deepEqual(f.draft.value?.[0].credentialIds,["a2","a1"]);
  f.revision.value={expectedRevision:5,processGeneration:99};
  assert.equal(f.draft.value,null);assert.equal(handlers.size,0);assert.equal(f.requests.length,0);f.layout.revertActiveArrangement();
});

type HostNode = { type:string; parent:HostNode|null; children:HostNode[]; props:Record<string,unknown>; text:string };
function hostNode(type:string, text=""):HostNode { return {type,parent:null,children:[],props:{},text}; }
function renderRows(f:ReturnType<typeof fixture>) {
  const renderer=createRenderer<HostNode,HostNode>({
    createElement:(type)=>hostNode(type),createText:(text)=>hostNode("#text",text),createComment:(text)=>hostNode("#comment",text),
    setText:(node,text)=>{node.text=text;},setElementText:(node,text)=>{node.children=[];node.text=text;},
    patchProp:(node,key,_previous,next)=>{node.props[key]=next;},
    insert:(node,parent,anchor=null)=>{
      if(node.parent){const index=node.parent.children.indexOf(node);if(index>=0)node.parent.children.splice(index,1);}
      const index=anchor?parent.children.indexOf(anchor):-1;
      parent.children.splice(index<0?parent.children.length:index,0,node);node.parent=parent;
    },
    remove:(node)=>{if(node.parent){const index=node.parent.children.indexOf(node);if(index>=0)node.parent.children.splice(index,1);node.parent=null;}},
    parentNode:(node)=>node.parent,
    nextSibling:(node)=>{if(!node.parent)return null;return node.parent.children[node.parent.children.indexOf(node)+1]??null;},
  });
  const Rows={props:["credentials"],render(this:{credentials:string[];$slots:Record<string,unknown>}){
    return h("div",{class:"destination-rows"},this.credentials.map(id=>
      h(Fragment,{key:id},[renderSlot(this.$slots as Parameters<typeof renderSlot>[0],"row",{id})])));
  }};
  const root=hostNode("root");
  renderer.createApp({render(){
    const card=(f.draft.value??f.committedLayout.value)[0];
    return h(Rows,{credentials:card.credentialIds},{row:({id}:{id:string})=>h("div",{"data-layout-row-id":id},id)});
  }}).mount(root);
  const rowIds=()=>{
    const found:string[]=[];
    const visit=(node:HostNode)=>{if(typeof node.props["data-layout-row-id"]==="string")found.push(node.props["data-layout-row-id"] as string);node.children.forEach(visit);};
    visit(root);return found;
  };
  return {rowIds};
}

test("Vue keeps slot fragment rows consistent after preview, save, delete, and cancellation", async()=>{
  const handlers=new Map<string,(event:PointerEvent)=>unknown>();
  let targetRow="a2";
  Object.defineProperty(globalThis,"window",{configurable:true,value:{addEventListener:(name:string,fn:(event:PointerEvent)=>unknown)=>handlers.set(name,fn),removeEventListener:(name:string)=>handlers.delete(name)}});
  Object.defineProperty(globalThis,"document",{configurable:true,value:{elementFromPoint:()=>({closest:(selector:string)=>selector.includes("account-card")?{dataset:{accountId:"a"}}:{dataset:{credentialId:targetRow}}})}});
  const frame=stubPreviewFrame();
  const f=fixture();const rendered=renderRows(f);
  const handle={setPointerCapture(){},hasPointerCapture(){return true;},releasePointerCapture(){}};
  const event={isPrimary:true,pointerType:"mouse",button:0,pointerId:1,currentTarget:handle,preventDefault(){},clientX:0,clientY:0} as unknown as PointerEvent;
  assert.deepEqual(rendered.rowIds(),["a1","a2"]);
  f.layout.startCredentialDrag(event,"a","a1");handlers.get("pointermove")!(event);frame.flush();await nextTick();
  assert.deepEqual(rendered.rowIds(),["a2","a1"]);
  const finishing=handlers.get("pointerup")!(event) as Promise<unknown>;
  f.committedLayout.value=f.requests[0].layout;
  f.pending.resolve();await finishing;await nextTick();
  assert.deepEqual(rendered.rowIds(),["a2","a1"]);
  f.committedLayout.value=f.committedLayout.value.map(card=>card.id==="a"?{...card,credentialIds:["a2"]}:card);
  await nextTick();assert.deepEqual(rendered.rowIds(),["a2"]);
  f.committedLayout.value=f.committedLayout.value.map(card=>card.id==="a"?{...card,credentialIds:["a2","a3"]}:card);
  await nextTick();
  targetRow="a3";
  f.layout.startCredentialDrag(event,"a","a2");handlers.get("pointermove")!(event);frame.flush();await nextTick();
  assert.deepEqual(rendered.rowIds(),["a3","a2"]);
  handlers.get("pointercancel")!(event);await nextTick();
  assert.deepEqual(rendered.rowIds(),["a2","a3"]);
  f.committedLayout.value=f.committedLayout.value.map(card=>card.id==="a"?{...card,credentialIds:["a3"]}:card);
  await nextTick();assert.deepEqual(rendered.rowIds(),["a3"]);
  f.layout.revertActiveArrangement();
});
test("card pointer preview reorders the reactive draft and cancellation restores the committed order",()=>{
  const handlers=new Map<string,(event:PointerEvent)=>unknown>();
  Object.defineProperty(globalThis,"window",{configurable:true,value:{addEventListener:(name:string,fn:(event:PointerEvent)=>unknown)=>handlers.set(name,fn),removeEventListener:(name:string)=>handlers.delete(name)}});
  Object.defineProperty(globalThis,"document",{configurable:true,value:{elementFromPoint:()=>({closest:()=>({dataset:{accountId:"b"}})})}});
  const frame=stubPreviewFrame();
  const f=fixture();
  const handle={setPointerCapture(){},hasPointerCapture(){return true;},releasePointerCapture(){}};
  const event={isPrimary:true,pointerType:"mouse",button:0,pointerId:1,currentTarget:handle,preventDefault(){},clientX:0,clientY:0} as unknown as PointerEvent;
  f.layout.startCardDrag(event,"a");handlers.get("pointermove")!(event);
  assert.equal(f.draft.value,null);
  frame.flush();
  const preview=f.draft.value as RoutingCardLayoutDraft[] | null;
  assert.deepEqual(preview?.map(card=>card.id),["empty","b","a"]);
  handlers.get("pointercancel")!(event);
  assert.equal(f.draft.value,null);
  assert.deepEqual(f.committedLayout.value.map(card=>card.id),["a","empty","b"]);
  assert.equal(f.requests.length,0);
  f.layout.revertActiveArrangement();
});
test("finishing a drag applies a pending pointer preview before saving", async()=>{
  const handlers = new Map<string, (event:PointerEvent)=>unknown>();
  Object.defineProperty(globalThis,"window",{configurable:true,value:{addEventListener:(name:string,fn:(e:PointerEvent)=>unknown)=>handlers.set(name,fn),removeEventListener:(name:string)=>handlers.delete(name)}});
  Object.defineProperty(globalThis,"document",{configurable:true,value:{elementFromPoint:()=>({closest:(selector:string)=>selector.includes("account-card")?{dataset:{accountId:"a"}}:{dataset:{credentialId:"a2"}}})}});
  const frame=stubPreviewFrame();
  const f=fixture();const handle={setPointerCapture(){},hasPointerCapture(){return true;},releasePointerCapture(){},closest(){return null;}};
  const event={isPrimary:true,pointerType:"mouse",button:0,pointerId:1,currentTarget:handle,preventDefault(){},clientX:0,clientY:0} as unknown as PointerEvent;
  f.layout.startCredentialDrag(event,"a","a1");handlers.get("pointermove")!(event);
  assert.equal(frame.pending(),1);
  const finishing=handlers.get("pointerup")!(event) as Promise<unknown>;
  assert.equal(frame.pending(),0);
  assert.deepEqual(f.requests[0].layout[0].credentialIds,["a2","a1"]);
  f.pending.resolve();await finishing;f.layout.revertActiveArrangement();
});
