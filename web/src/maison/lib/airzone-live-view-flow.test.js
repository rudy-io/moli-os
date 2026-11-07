import {test,before,after} from 'node:test';
import assert from 'node:assert/strict';
import {scriptFixture} from '../../../test-support/svelte-script-fixture.js';
let view,diagnostic;
const originals=new Map(['document','localStorage'].map(key=>[key,Object.getOwnPropertyDescriptor(globalThis,key)]));
before(async()=>{
 delete globalThis.localStorage;globalThis.document={querySelector:()=>null,documentElement:{}};
 view=await scriptFixture('/src/maison/pages/Airzone.svelte',`
  export function install(devices,drivers,connected=true){hub.devices=devices;hub.drivers=drivers;hub.connected=connected;}
  export function emitValue(id,key,value,ts){hub.devices[id].state[key]={value,ts};}
  export function connected(value){hub.connected=value;}
  export function availability(id,value){hub.devices[id].online=value;}
  export function advance(time){now=time;}
  export function inspect(){const device=inventory.zones[0];return {zone:device,temperature:device?readingOf(device,'temperature'):null,sync:device?syncFor(device):null,count:inventory.zones.length};}
 `);
 diagnostic=await scriptFixture('/src/maison/ui/AirzoneDiagnostic.svelte',`
  export function configure(values){operation=values.operation??operation;system=values.system??system;zone=values.zone??zone;sensor=values.sensor??sensor;get=values.get??get;}
  export function session(human){hub.session.human=human;}
  export function run(){return inspect();}
  export function inspectState(){return {valid,busy,result,error};}
 `);
});
after(()=>{for(const[key,value]of originals){if(value)Object.defineProperty(globalThis,key,value);else delete globalThis[key];}});
const turn=()=>new Promise(resolve=>setImmediate(resolve));
const entity=(id,model,values)=>({id,instance:'synthetic',model,native_name:id,online:true,points:Object.entries(values).map(([key,value])=>({key,kind:{type:typeof value==='boolean'?'binary':'numeric'},access:{read:true,write:false}})),state:Object.fromEntries(Object.entries(values).map(([key,value])=>[key,{value,ts:1000}]))});
test('client runes react to repeated samples, unchanged receipt heartbeats, disconnect and replacement snapshot',()=>{
 const mounted=view.mount({route:'airzone'});try{
 const zone=entity('synthetic:zone','Local API',{systemID:1,zoneID:1,temperature:20,sync_last_received:10000,sync_generation:1});
 const gateway=entity('synthetic:gateway','Airzone Webserver',{sync_poll_interval:30,sync_last_received:10000});
 mounted.component.install({[zone.id]:zone,[gateway.id]:gateway},[{instance:'synthetic',kind:'airzone'}]);mounted.component.advance(11000);view.flushSync();assert.equal(mounted.component.inspect().temperature.value,20);assert.equal(mounted.component.inspect().sync.state,'current');
 mounted.component.emitValue(zone.id,'temperature',21,12000);mounted.component.emitValue(zone.id,'sync_last_received',12000,12000);view.flushSync();assert.equal(mounted.component.inspect().temperature.value,21);
 mounted.component.emitValue(zone.id,'sync_last_received',20000,20000);mounted.component.advance(21000);view.flushSync();assert.equal(mounted.component.inspect().temperature.ts,12000);assert.equal(mounted.component.inspect().sync.received,20000);
 mounted.component.connected(false);view.flushSync();assert.equal(mounted.component.inspect().sync.state,'disconnected');
 mounted.component.install({[zone.id]:entity(zone.id,'Local API',{systemID:1,zoneID:1,temperature:22,sync_last_received:30000,sync_generation:4}),[gateway.id]:gateway},[{instance:'synthetic',kind:'airzone'}]);mounted.component.advance(31000);view.flushSync();assert.equal(mounted.component.inspect().temperature.value,22);assert.equal(mounted.component.inspect().count,1);
 mounted.component.availability(zone.id,false);view.flushSync();assert.equal(mounted.component.inspect().sync.state,'offline');assert.equal(mounted.component.inspect().temperature.value,22);
 }finally{mounted.dispose();}
});
test('diagnostic has no automatic requests, asks PIN, bounds addresses and never calls command API',async(t)=>{
 const calls=[];t.mock.method(globalThis,'fetch',async(url,options)=>{calls.push({url,...options});return {ok:true,status:200,json:async()=>({read_only:true,synthetic:false,response:{systems:[]}})};});
 const mounted=diagnostic.mount({instance:'synthetic',connected:true});diagnostic.home.held=null;try{
 mounted.component.session(false);await turn();assert.equal(calls.length,0);mounted.component.configure({operation:'hvac',system:33,zone:''});diagnostic.flushSync();assert.equal(mounted.component.inspectState().valid,false);await mounted.component.run();assert.equal(calls.length,0);
 mounted.component.configure({operation:'hvac',system:127,zone:''});diagnostic.flushSync();assert.equal(mounted.component.inspectState().valid,true);await mounted.component.run();assert.equal(calls.length,0);assert.ok(diagnostic.home.held);
 const held=diagnostic.home.held;diagnostic.home.held=null;await held.custom();assert.equal(calls.length,1);assert.equal(calls[0].url,'/api/integrations/airzone/synthetic/diagnostic');assert.deepEqual(JSON.parse(calls[0].body),{operation:'hvac',system:127,get:false});assert.equal(mounted.component.inspectState().result.read_only,true);
 mounted.component.session(true);mounted.component.configure({operation:'iaq',system:1,sensor:3,get:true});await mounted.component.run();assert.deepEqual(JSON.parse(calls[1].body),{operation:'iaq',system:1,sensor:3,get:true});assert.ok(calls.every(call=>!call.url.includes('/api/command')));
 }finally{mounted.dispose();diagnostic.home.held=null;}
});
test('diagnostic handles retryable errors and marks demo as synthetic without importing it into state',async(t)=>{
 let response={read_only:true,synthetic:true,response:{temperature:999}},status=200;
 t.mock.method(globalThis,'fetch',async()=>({ok:status===200,status,json:async()=>response}));
 const mounted=diagnostic.mount({instance:'synthetic',connected:true});try{
 mounted.component.session(true);mounted.component.configure({operation:'demo'});await mounted.component.run();assert.equal(mounted.component.inspectState().result.synthetic,true);
 status=429;response={error:'busy'};await mounted.component.run();assert.equal(mounted.component.inspectState().error,'busy');assert.equal(mounted.component.inspectState().result,null);assert.equal(mounted.component.inspectState().busy,false);
 }finally{mounted.dispose();}
});
