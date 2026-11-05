import { test, before, beforeEach, after } from 'node:test';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { createServer } from 'vite';

let vite, hub, source;
const original=new Map(['document','EventSource','fetch','setInterval'].map(key=>[key,Object.getOwnPropertyDescriptor(globalThis,key)]));
before(async()=>{
  globalThis.document={querySelector:()=>null,documentElement:{}};
  globalThis.EventSource=class {
    listeners=new Map();
    constructor(url) {assert.equal(url,'/api/events');source=this;}
    addEventListener(type,handler) {this.listeners.set(type,handler);}
    emit(type,data) {this.listeners.get(type)({data:JSON.stringify(data)});}
  };
  globalThis.fetch=async()=>({ok:false,json:async()=>[]});
  globalThis.setInterval=()=>0;
  vite=await createServer({root:fileURLToPath(new URL('../../',import.meta.url)),server:{middlewareMode:true,watch:null,hmr:false},logLevel:'error'});
  const module=await vite.ssrLoadModule('/src/lib/hub.svelte.js');
  hub=module.hub;module.connect();
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

test('registry revision changes on registration and reconnect snapshots but not status or samples',()=>{
  const revision=hub.driverRevision;
  assert.ok(Number.isSafeInteger(revision));
  event({type:'driver_registered',instance:'airzone-test',kind:'airzone',integration:'airzone'});
  assert.equal(hub.driverRevision,revision+1);
  event({type:'driver_status',instance:'airzone-test',status:{state:'backoff'}});
  assert.equal(hub.driverRevision,revision+1);
  source.onerror();assert.equal(hub.connected,false);
  source.emit('snapshot',{devices:[],drivers:[]});
  assert.equal(hub.connected,true);assert.equal(hub.driverRevision,revision+2);
});

test('restoration uses the persisted label even without a previous browser device, and supersedes an older label',()=>{
  const label={name:'Cached zone name',room:'Cached room'};
  event({type:'device_restored',device,states:{temperature:sample},label});
  assert.deepEqual(hub.devices[device.id].label,label);
  hub.devices[device.id].label={name:'Old browser name'};
  event({type:'device_restored',device,states:{temperature:sample},label:{}});
  assert.deepEqual(hub.devices[device.id].label,{},'an explicit empty label also remains authoritative');
});
