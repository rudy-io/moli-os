import { test, before, beforeEach, after } from 'node:test';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { createServer } from 'vite';

let vite, hub, source, reconcileAirzoneDevice;
const pageEvents=new Map();
const windowEvents=new Map();
const original=new Map(['document','EventSource','fetch','setInterval','addEventListener'].map(key=>[key,Object.getOwnPropertyDescriptor(globalThis,key)]));
before(async()=>{
  globalThis.document={querySelector:()=>null,documentElement:{},visibilityState:'visible',addEventListener:(name,handler)=>pageEvents.set(name,handler)};
  globalThis.addEventListener=(name,handler)=>windowEvents.set(name,handler);
  globalThis.EventSource=class {
    listeners=new Map();
    constructor(url) {assert.equal(url,'/api/events');source=this;}
    addEventListener(type,handler) {this.listeners.set(type,handler);}
    close() {this.closed=true;}
    emit(type,data) {this.listeners.get(type)({data:JSON.stringify(data)});}
  };
  globalThis.fetch=async()=>({ok:false,json:async()=>[]});
  globalThis.setInterval=()=>0;
  vite=await createServer({root:fileURLToPath(new URL('../../',import.meta.url)),server:{middlewareMode:true,watch:null,hmr:false},logLevel:'error'});
  const module=await vite.ssrLoadModule('/src/lib/hub.svelte.js');
  hub=module.hub;reconcileAirzoneDevice=module.reconcileAirzoneDevice;module.connect();
});
beforeEach(()=>{
  hub.devices={};hub.drivers=[];hub.changed={};hub.pending={};hub.connected=true;
});
after(async()=>{
  await vite?.close();
  for(const[key,descriptor]of original) {if(descriptor)Object.defineProperty(globalThis,key,descriptor);else delete globalThis[key];}
});
const device={id:'airzone-test:example',instance:'airzone-test',native_name:'Retained zone',points:[]};
const sample={value:21,ts:1700000000000};
const event=data=>source.emit('event',data);

test('restored offline samples replace state with original timestamps, preserve labels and never confirm pending commands',()=>{
  hub.devices[device.id]={...device,label:{name:'Kept name',room:'Kept room'},online:true,state:{obsolete:{value:99,ts:1700000001000}}};
  hub.pending[device.id+'/temperature']=21;
  hub.changed['other:device/value']=1700000002000;
  event({type:'device_restored',device:{...device,label:{name:'Stale label'}},states:{temperature:sample}});
  assert.equal(hub.devices[device.id].online,false);
  assert.deepEqual(hub.devices[device.id].label,{name:'Kept name',room:'Kept room'});
  assert.deepEqual(hub.devices[device.id].state,{temperature:sample});
  assert.equal(hub.pending[device.id+'/temperature'],21);
  assert.deepEqual(hub.changed,{'other:device/value':1700000002000});
});

test('unknown restored devices remain offline until availability and only fresh state events flash or confirm',()=>{
  event({type:'device_restored',device,states:{temperature:sample}});
  assert.ok(hub.devices[device.id]);assert.deepEqual(hub.devices[device.id].label,{});
  assert.equal(hub.devices[device.id].online,false);
  event({type:'device_upserted',device:{...device,native_name:'Updated zone'}});
  assert.equal(hub.devices[device.id].online,false);assert.deepEqual(hub.devices[device.id].state,{temperature:sample});
  const point=device.id+'/temperature';hub.pending[point]=22;
  event({type:'state',point,value:22,ts:1700000009000});
  assert.equal(hub.pending[point],undefined);assert.equal(hub.changed[point],1700000009000);
  assert.equal(hub.devices[device.id].online,false);
  event({type:'availability',device:device.id,online:true});assert.equal(hub.devices[device.id].online,true);
});

test('registry revision changes on registration, withdrawal and reconnect snapshots but not status or samples',()=>{
  const revision=hub.driverRevision;
  assert.ok(Number.isSafeInteger(revision));
  event({type:'driver_registered',instance:'airzone-test',kind:'airzone',integration:'airzone'});
  assert.equal(hub.driverRevision,revision+1);
  event({type:'driver_status',instance:'airzone-test',status:{state:'backoff'}});
  assert.equal(hub.driverRevision,revision+1);
  event({type:'driver_unregistered',instance:'airzone-test'});
  assert.equal(hub.driverRevision,revision+2);assert.equal(hub.drivers.length,0);
  source.onerror();assert.equal(hub.connected,false);
  source.emit('snapshot',{devices:[],drivers:[]});
  assert.equal(hub.connected,true);assert.equal(hub.driverRevision,revision+3);
});

test('restoration uses the persisted label even without a previous browser device, and supersedes an older label',()=>{
  const label={name:'Cached zone name',room:'Cached room'};
  event({type:'device_restored',device,states:{temperature:sample},label});
  assert.deepEqual(hub.devices[device.id].label,label);
  hub.devices[device.id].label={name:'Old browser name'};
  event({type:'device_restored',device,states:{temperature:sample},label:{}});
  assert.deepEqual(hub.devices[device.id].label,{},'an explicit empty label also remains authoritative');
});

test('older Airzone state events cannot regress a reading or acknowledge a newer intention',()=>{
  hub.drivers=[{instance:'airzone-test',kind:'airzone'}];
  hub.devices[device.id]={...device,online:true,state:{temperature:{value:22,ts:2000}}};
  const point=device.id+'/temperature';hub.pending[point]=21;
  event({type:'state',point,value:21,ts:1000});
  assert.equal(hub.devices[device.id].state.temperature.value,22);
  assert.equal(hub.pending[point],21);
  assert.equal(hub.changed[point],undefined);
});

test('returning to a visible Airzone page replaces a silent SSE link and ignores the abandoned source',()=>{
  hub.drivers=[{instance:'airzone-test',kind:'airzone'}];
  const abandoned=source;
  assert.equal(typeof pageEvents.get('visibilitychange'),'function');
  document.visibilityState='hidden';pageEvents.get('visibilitychange')();assert.equal(source,abandoned);
  document.visibilityState='visible';pageEvents.get('visibilitychange')();
  assert.notEqual(source,abandoned);assert.equal(abandoned.closed,true);assert.equal(hub.connected,false);
  source.emit('snapshot',{devices:[{...device,online:true,state:{temperature:{value:23,ts:3000}}}],drivers:[{instance:'airzone-test',kind:'airzone'}]});
  abandoned.emit('event',{type:'state',point:device.id+'/temperature',value:18,ts:4000});
  abandoned.onerror();assert.equal(hub.connected,true);assert.equal(hub.devices[device.id].state.temperature.value,23);
  const active=source;windowEvents.get('online')();assert.notEqual(source,active);assert.equal(active.closed,true);
});

test('Airzone metadata removal prunes missing samples even while the gateway remains healthy',()=>{
 hub.drivers=[{instance:'airzone-test',kind:'airzone'}];
 hub.devices[device.id]={...device,online:true,state:{temperature:sample,removed:{value:4,ts:1}}};
 event({type:'device_upserted',device:{...device,points:[{key:'temperature'}]}});
 assert.deepEqual(hub.devices[device.id].state,{temperature:sample});
});
test('post-command REST reconciliation cannot replace a later SSE entity generation',()=>{
 hub.drivers=[{instance:'airzone-test',kind:'airzone'}];
 const current={...device,online:true,points:[{key:'temperature'},{key:'sync_generation'}],state:{temperature:{value:24,ts:4000},sync_generation:{value:4,ts:4000}}};
 hub.devices[device.id]=current;
 assert.equal(reconcileAirzoneDevice({...current,state:{temperature:{value:20,ts:3000},sync_generation:{value:3,ts:3000}}}),false);
 assert.equal(hub.devices[device.id].state.temperature.value,24);
 assert.equal(reconcileAirzoneDevice({...current,state:{temperature:{value:22,ts:5000},sync_generation:{value:5,ts:5000}}}),true);
 assert.equal(hub.devices[device.id].state.temperature.value,22);
 assert.equal(reconcileAirzoneDevice({...current,id:'foreign:zone',instance:'foreign'}),false);
});
test('an unchanged Airzone heartbeat refreshes browser receipt while preserving the value-change timestamp',()=>{
 hub.drivers=[{instance:'airzone-test',kind:'airzone'}];hub.airzoneReceivedAt={};
 hub.devices[device.id]={...device,online:true,state:{temperature:sample}};
 event({type:'state',point:device.id+'/sync_generation',value:2,ts:Date.now()});
 assert.ok(hub.airzoneReceivedAt['airzone-test']>0);assert.equal(hub.devices[device.id].state.temperature.ts,sample.ts);
});
test('foreground recovery does not restart unrelated integrations',()=>{
 hub.drivers=[{instance:'other',kind:'tuya'}];const previous=source;pageEvents.get('visibilitychange')();windowEvents.get('online')();assert.equal(source,previous);
});
test('a delayed REST reply cannot restore capability or availability revoked by SSE',()=>{
 hub.drivers=[{instance:'airzone-test',kind:'airzone'}];
 hub.devices[device.id]={...device,online:false,points:[{key:'on',access:{read:true,write:false}},{key:'sync_generation'}],state:{on:{value:false,ts:1},sync_generation:{value:4,ts:1}}};
 assert.equal(reconcileAirzoneDevice({...hub.devices[device.id],online:true,points:[{key:'on',access:{read:true,write:true}},{key:'removed',access:{write:true}},{key:'sync_generation'}],state:{on:{value:true,ts:2},removed:{value:20,ts:2},sync_generation:{value:4,ts:2}}}),true);
 assert.equal(hub.devices[device.id].online,false);assert.equal(hub.devices[device.id].points[0].access.write,false);assert.equal(hub.devices[device.id].state.removed,undefined);
});
