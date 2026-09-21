import assert from "node:assert/strict";
import test from "node:test";
import { ref } from "vue";
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
test("pointer preview moves rows inside their card and cancels if the saved revision changes", async()=>{
  const handlers = new Map<string, (event:PointerEvent)=>unknown>();
  Object.defineProperty(globalThis,"window",{configurable:true,value:{addEventListener:(name:string,fn:(e:PointerEvent)=>unknown)=>handlers.set(name,fn),removeEventListener:(name:string)=>handlers.delete(name)}});
  Object.defineProperty(globalThis,"document",{configurable:true,value:{elementFromPoint:()=>({closest:(selector:string)=>selector.includes("account-card")?{dataset:{accountId:"a"}}:{dataset:{credentialId:"a2"}}})}});
  const f=fixture();const handle={setPointerCapture(){},hasPointerCapture(){return true;},releasePointerCapture(){}};
  const event={isPrimary:true,pointerType:"mouse",button:0,pointerId:1,currentTarget:handle,preventDefault(){},clientX:0,clientY:0} as unknown as PointerEvent;
  f.layout.startCredentialDrag(event,"a","a1");handlers.get("pointermove")!(event);
  assert.deepEqual(f.draft.value?.[0].credentialIds,["a2","a1"]);
  f.revision.value={expectedRevision:5,processGeneration:99};
  assert.equal(f.draft.value,null);assert.equal(handlers.size,0);assert.equal(f.requests.length,0);f.layout.revertActiveArrangement();
});
