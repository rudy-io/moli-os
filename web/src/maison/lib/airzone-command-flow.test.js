import {test,before,after,beforeEach} from 'node:test';
import assert from 'node:assert/strict';
import {createServer} from 'vite';
import {fileURLToPath} from 'node:url';
let vite,hub,home,client,reconcileAirzoneDevice,render,Controls,Detail,Page,counter=0;
const originals=new Map(['document','localStorage','fetch'].map(key=>[key,Object.getOwnPropertyDescriptor(globalThis,key)]));
before(async()=>{
 delete globalThis.localStorage;globalThis.document={querySelector:()=>null,documentElement:{}};
 vite=await createServer({root:fileURLToPath(new URL('../../../',import.meta.url)),server:{middlewareMode:true,watch:null,hmr:false},logLevel:'error'});
 ({hub,reconcileAirzoneDevice}=await vite.ssrLoadModule('/src/lib/hub.svelte.js'));
 ({home}=await vite.ssrLoadModule('/src/maison/lib/home.svelte.js'));client=await vite.ssrLoadModule('/src/maison/lib/airzone-command.svelte.js');
 ({render}=await vite.ssrLoadModule('svelte/server'));Controls=(await vite.ssrLoadModule('/src/maison/ui/AirzoneControls.svelte')).default;Detail=(await vite.ssrLoadModule('/src/maison/ui/AirzoneZoneDetail.svelte')).default;Page=(await vite.ssrLoadModule('/src/maison/pages/Airzone.svelte')).default;await (await vite.ssrLoadModule('/src/lib/i18n.svelte.js')).setLanguage('fr');
});
beforeEach(()=>{hub.devices={};hub.drivers=[{instance:'test-airzone',kind:'airzone'}];hub.connected=true;hub.session={human:true};home.held=null;home.config={};});
after(async()=>{await vite?.close();for(const[key,value]of originals){if(value)Object.defineProperty(globalThis,key,value);else delete globalThis[key];}});
const waitFor=async(test)=>{for(let i=0;i<100;i++){if(test())return;await new Promise(resolve=>setTimeout(resolve,10));}assert.fail('expected state not reached');};
const response=(data,status=200)=>({ok:status>=200&&status<300,status,json:async()=>data});
function entity(id,values,extra={}){
 return {id,instance:'test-airzone',online:true,native_name:'Synthetic zone',model:'Local API',points:Object.entries(values).map(([key,value])=>({key,label:key,kind:typeof value==='boolean'?{type:'binary'}:{type:'numeric',min:0,max:10000000000000000,step:1},access:{read:true,write:['on','target_temperature'].includes(key)}})),state:Object.fromEntries(Object.entries(values).map(([key,value])=>[key,{value,ts:Date.now()}])),...extra};
}
function setup(){
 const zone=entity(`test-airzone:zone-${++counter}`,{on:false,target_temperature:20,sync_generation:2,sync_last_received:Date.now(),sync_last_command_generation:0});
 zone.points.find(point=>point.key==='target_temperature').kind={type:'numeric',min:15,max:30,step:0.5};
 const gateway=entity('test-airzone:gateway',{sync_poll_interval:30},{model:'Airzone Webserver'});
 hub.devices={[zone.id]:zone,[gateway.id]:gateway};return zone;
}
const observed=(zone,value,generation=4)=>({...zone,state:{...zone.state,on:{value,ts:Date.now()},sync_generation:{value:generation,ts:Date.now()},sync_last_command_generation:{value:3,ts:Date.now()}}});
test('ordinary family command uses one POST and one read-only barrier read; ACK alone is not confirmed',async()=>{
 const zone=setup(),point=zone.id+'/on',calls=[];hub.session.human=false;
 globalThis.fetch=async(url,options)=>{calls.push({url,...options});return response(url==='/api/command'?{ok:true}:observed(zone,false,3));};
 assert.equal(client.requestAirzone(point,true),true);await waitFor(()=>client.airzoneCommands[point]?.phase==='awaiting');
 assert.equal(home.held,null);assert.equal(calls.length,2);assert.equal(calls[0].method,'POST');assert.equal(calls[1].method,undefined);assert.deepEqual(JSON.parse(calls[0].body),{point,value:true});
 reconcileAirzoneDevice(observed(zone,true,4));assert.equal(client.airzoneCommands[point].phase,'confirmed');
});
test('guarded order cancellation denied by expired session remains explicit and can be refused with a fresh code',async()=>{
 const zone=setup(),point=zone.id+'/on',calls=[];let human=false;
 globalThis.fetch=async(url,options)=>{calls.push({url,...options});return url==='/api/command'?response({pending:7,reason:'protected'},202):response({ok:human},human?200:403);};
 client.requestAirzone(point,true);await waitFor(()=>home.held?.id===7);const held=home.held;home.held=null;await held.onCancel();await waitFor(()=>client.airzoneCommands[point]?.error==='approval_pending');
 assert.equal(calls.filter(call=>call.url==='/api/command').length,1);
 assert.deepEqual(JSON.parse(calls[1].body),{approve:false,point});
 assert.equal(client.denyPendingAirzone(point),true);human=true;const denial=home.held;home.held=null;await denial.custom();assert.equal(client.airzoneCommands[point].error,'cancelled');
 assert.equal(calls.filter(call=>call.url==='/api/command').length,1);assert.equal(calls.length,3);
});
test('guard approval retains the causal barrier and sends no duplicate command',async()=>{
 const zone=setup(),point=zone.id+'/on',calls=[];
 globalThis.fetch=async(url,options)=>{calls.push({url,...options});return url==='/api/command'?response({pending:8,reason:'protected'},202):response(url.startsWith('/api/approvals/')?{ok:true}:observed(zone,true,4));};
 client.requestAirzone(point,true);await waitFor(()=>home.held?.id===8);const held=home.held;home.held=null;await held.custom();await waitFor(()=>client.airzoneCommands[point]?.phase==='confirmed');
 assert.equal(calls.filter(call=>call.url==='/api/command').length,1);assert.deepEqual(JSON.parse(calls[1].body),{approve:true,point});
});
test('read-only, stale, unknown and no-longer-advertised values send nothing',()=>{
 const zone=setup();let calls=0;globalThis.fetch=async()=>{calls++;throw Error('unexpected');};
 zone.points.find(point=>point.key==='on').access.write=false;assert.equal(client.requestAirzone(zone.id+'/on',true),false);
 assert.equal(client.requestAirzone(zone.id+'/target_temperature',null),false);assert.equal(client.requestAirzone(zone.id+'/target_temperature',80),false);
 zone.state.sync_last_received.value=1;assert.equal(client.requestAirzone(zone.id+'/target_temperature',22),false);assert.equal(calls,0);
});
test('compound turn-on confirms only a newer reading containing both on and target',async()=>{
 const zone=setup(),point=zone.id+'/target_temperature_on';zone.points.push({...zone.points.find(spec=>spec.key==='target_temperature'),key:'target_temperature_on',access:{read:false,write:true}});
 const answer=observed(zone,false,4);answer.state.target_temperature={value:22,ts:Date.now()};
 globalThis.fetch=async(url)=>response(url==='/api/command'?{ok:true}:answer);
 client.requestAirzone(point,22);await waitFor(()=>client.airzoneCommands[point]?.phase==='awaiting');
 reconcileAirzoneDevice({...answer,state:{...answer.state,on:{value:true,ts:Date.now()},sync_generation:{value:5,ts:Date.now()}}});assert.equal(client.airzoneCommands[point].phase,'confirmed');
});
test('an unresolved guard blocks another request on the same point and an offline cancellation keeps its explicit refusal action',async()=>{
 const zone=setup(),point=zone.id+'/on',calls=[];
 globalThis.fetch=async(url,options)=>{calls.push({url,...options});return url==='/api/command'?response({pending:9,reason:'protected'},202):response({error:'human_only'},403);};
 client.requestAirzone(point,true);await waitFor(()=>home.held?.id===9);
 reconcileAirzoneDevice({...zone,state:{...zone.state,sync_generation:{value:3,ts:Date.now()},sync_last_received:{value:1,ts:Date.now()}}});
 const held=home.held;home.held=null;await held.onCancel();await waitFor(()=>client.airzoneCommands[point]?.error==='approval_pending');
 reconcileAirzoneDevice({...zone,state:{...zone.state,sync_generation:{value:4,ts:Date.now()},sync_last_received:{value:Date.now(),ts:Date.now()}}});
 assert.equal(client.hasPendingAirzone(point),true);assert.equal(client.requestAirzone(point,false),false);assert.equal(calls.filter(call=>call.url==='/api/command').length,1);
 globalThis.fetch=async()=>response({ok:true});client.denyPendingAirzone(point);const denial=home.held;home.held=null;await denial.custom();assert.equal(client.hasPendingAirzone(point),false);
});

test('a guarded target stays visible and refusable after presentation and permission changes',async()=>{
 const zone=setup(),key='target_temperature',point=zone.id+'/'+key,calls=[];
 globalThis.fetch=async(url,options)=>{calls.push({url,...options});return url==='/api/command'?response({pending:17,reason:'protected'},202):response({error:'human_only'},403);};
 assert.equal(client.requestAirzone(point,22),true);await waitFor(()=>home.held?.id===17);
 const held=home.held;home.held=null;await held.onCancel();await waitFor(()=>client.airzoneCommands[point]?.error==='approval_pending');
 zone.points.push({...zone.points.find(p=>p.key===key),key:'heatsetpoint'},{...zone.points.find(p=>p.key===key),key:'coolsetpoint'},{key:'double_sp',kind:{type:'binary'},access:{read:true}});zone.state.double_sp={value:true};
 zone.points.forEach(p=>p.access.write=false);
 for(const sync of [{state:'current'},{state:'offline'}]){
  const html=render(Controls,{props:{device:zone,sync}}).body;
  assert.match(html,/data-retained-command="target_temperature"/);assert.match(html,/Refuser la demande avec mon code/);assert.doesNotMatch(html,/<input|<select|role="switch"/);
 }
 zone.points=zone.points.filter(p=>p.key!==key);
 const removed=render(Controls,{props:{device:zone,sync:{state:'offline'}}}).body;assert.match(removed,/Refuser la demande avec mon code/);
 zone.points=zone.points.filter(p=>!['on','mode','heatsetpoint','coolsetpoint'].includes(p.key));
 const detail=render(Detail,{props:{zone,sync:{state:'offline'}}}).body;
 assert.match(detail,/data-retained-command="target_temperature"/);assert.match(detail,/Refuser la demande avec mon code/);
 for(const hidden of [[],[zone.id]]){
  home.config={hidden};const overview=render(Page,{props:{route:'airzone'}}).body;
  assert.match(overview,/Une demande attend encore/);assert.match(overview,/Refuser la demande avec mon code/);assert.doesNotMatch(overview,/class="control-row/);
  if(hidden.length){
   const hiddenDetail=render(Page,{props:{route:`airzone/${encodeURIComponent(zone.id)}`}}).body;
   assert.match(hiddenDetail,/Une demande attend encore/);assert.match(hiddenDetail,/Refuser la demande avec mon code/);
  }
 }
 home.config={};delete hub.devices[zone.id];
 const removedDetail=render(Page,{props:{route:`airzone/${encodeURIComponent(zone.id)}`}}).body;
 assert.match(removedDetail,/Une demande attend encore/);assert.match(removedDetail,/Refuser la demande avec mon code/);
 assert.equal(calls.filter(call=>call.url==='/api/command').length,1);
 globalThis.fetch=async()=>response({ok:true});assert.equal(client.denyPendingAirzone(point),true);const denial=home.held;home.held=null;await denial.custom();assert.equal(client.hasPendingAirzone(point),false);
});
