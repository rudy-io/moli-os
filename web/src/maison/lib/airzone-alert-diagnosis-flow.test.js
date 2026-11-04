import {test,before,after} from 'node:test';
import assert from 'node:assert/strict';
import {scriptFixture} from '../../../test-support/svelte-script-fixture.js';
let fixture;
const originals=new Map(['document','localStorage'].map(key=>[key,Object.getOwnPropertyDescriptor(globalThis,key)]));
before(async()=>{
 delete globalThis.localStorage;globalThis.document={querySelector:()=>null,documentElement:{}};
 fixture=await scriptFixture('/src/maison/ui/AirzoneAlertDiagnosis.svelte',`
  export function setup(human=false){hub.drivers=[{kind:'airzone',instance:'synthetic'}];hub.session.human=human;hub.devices={[device.id]:device};home.held=null;}
  export function run(){return verify();}
  export function inspect(){return {busy,awaitingPin,result,failure,human:hub.session.human,devices:JSON.stringify(hub.devices)};}
  export function cancel(){stop();}
 `);
});
after(()=>{for(const[key,value]of originals){if(value)Object.defineProperty(globalThis,key,value);else delete globalThis[key];}});
const device=(zone=2)=>({id:`synthetic:zone-${zone}`,instance:'synthetic',model:'Local API',online:false,points:['systemID','zoneID'].map(key=>({key,kind:{type:'numeric'},access:{read:true,write:false}})),state:{systemID:{value:1},zoneID:{value:zone}}});
const payload=(rows=[])=>({read_only:true,synthetic:false,response:{data:rows}});
const response=(data,status=200)=>({ok:status===200,status,json:async()=>data});
const mount=(human=false)=>{const mounted=fixture.mount({instance:'synthetic',device:device(),kind:'offline',connected:true});mounted.component.setup(human);fixture.flushSync();return mounted;};

test('checking asks for PIN, cancellation sends nothing, and a confirmed read never clears the alert state',async(t)=>{
 const calls=[];t.mock.method(globalThis,'fetch',async(url,options)=>{calls.push({url,...options});return response(payload([{systemID:1,zoneID:1}]));});
 const mounted=mount();try{
  const before=mounted.component.inspect().devices;assert.equal(calls.length,0);
  await mounted.component.run();assert.equal(calls.length,0);assert.equal(mounted.component.inspect().awaitingPin,true);
  const cancelled=fixture.home.held;fixture.home.held=null;cancelled.onCancel();fixture.flushSync();assert.equal(mounted.component.inspect().failure,'cancelled');assert.equal(calls.length,0);
  await mounted.component.run();const approval=fixture.home.held;fixture.home.held=null;await approval.custom();fixture.flushSync();
  assert.equal(calls.length,1);assert.equal(calls[0].url,'/api/integrations/airzone/synthetic/diagnostic');assert.deepEqual(JSON.parse(calls[0].body),{operation:'hvac',system:0,zone:0,get:false});
  assert.equal(mounted.component.inspect().result.state,'absent');assert.equal(typeof mounted.component.inspect().result.checkedAt,'number');assert.equal(mounted.component.inspect().devices,before);
 }finally{mounted.dispose();fixture.home.held=null;}
});
test('a rejected session requires another explicit PIN attempt, with no automatic read retry',async(t)=>{
 const calls=[];t.mock.method(globalThis,'fetch',async(url)=>{calls.push(url);return response({error:'private upstream message'},403);});
 const mounted=mount(true);try{await mounted.component.run();assert.equal(calls.length,1);assert.equal(mounted.component.inspect().failure,'auth');assert.equal(mounted.component.inspect().human,false);await mounted.component.run();assert.ok(fixture.home.held);assert.equal(calls.length,1);}finally{mounted.dispose();fixture.home.held=null;}
});
test('unknown format and unknown addresses never become an absent zone',async(t)=>{
 t.mock.method(globalThis,'fetch',async()=>response(payload(null)));
 const mounted=mount(true);try{await mounted.component.run();assert.equal(mounted.component.inspect().result.state,'unknown');mounted.props.device={...device(),state:{}};fixture.flushSync();await mounted.component.run();assert.equal(mounted.component.inspect().result.state,'gateway');}finally{mounted.dispose();}
});
test('timeout is explicit and a demonstration response cannot diagnose a real alert',async(t)=>{
 t.mock.method(globalThis,'fetch',async()=>response({read_only:true,synthetic:true,response:{data:[]}}));
 const mounted=mount(true);try{
  await mounted.component.run();assert.equal(mounted.component.inspect().failure,'failed');assert.equal(mounted.component.inspect().result,null);
  t.mock.method(AbortSignal,'timeout',()=>AbortSignal.abort(new DOMException('timeout','TimeoutError')));
  await mounted.component.run();assert.equal(mounted.component.inspect().failure,'timeout');assert.equal(mounted.component.inspect().result,null);
 }finally{mounted.dispose();}
});
test('cancel, retarget, disconnect and unmount abort the read and ignore a late response',async(t)=>{
 for(const action of ['cancel','retarget','disconnect','unmount']){
  let resolve,signal;t.mock.method(globalThis,'fetch',async(url,options)=>{signal=options.signal;return new Promise(done=>{resolve=done;});});
  const mounted=mount(true);let disposed=false;
  try{
   const pending=mounted.component.run();assert.equal(mounted.component.inspect().busy,true);
   if(action==='cancel')mounted.component.cancel();
   if(action==='retarget')mounted.props.device=device(3);
   if(action==='disconnect')mounted.props.connected=false;
   if(action==='unmount'){mounted.dispose();disposed=true;}
   fixture.flushSync();assert.equal(signal.aborted,true);
   resolve(response(payload([{systemID:1,zoneID:2}])));await pending;fixture.flushSync();assert.equal(mounted.component.inspect().result,null);assert.equal(mounted.component.inspect().busy,false);
  }finally{if(!disposed)mounted.dispose();}
 }
});
test('a PIN continuation cannot start a read after navigating away',async(t)=>{
 let calls=0;t.mock.method(globalThis,'fetch',async()=>{calls++;return response(payload([]));});
 const mounted=mount();await mounted.component.run();const held=fixture.home.held;mounted.dispose();assert.equal(fixture.home.held,null);await held.custom();assert.equal(calls,0);
});
