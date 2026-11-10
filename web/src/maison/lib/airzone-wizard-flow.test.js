import { test,before,after } from 'node:test';
import assert from 'node:assert/strict';
import { scriptFixture } from '../../../test-support/svelte-script-fixture.js';
let client;
const globals=new Map(['document','localStorage'].map((key)=>[key,Object.getOwnPropertyDescriptor(globalThis,key)]));
before(async()=>{
 delete globalThis.localStorage;globalThis.document={querySelector:()=>null,documentElement:{}};
 client=await scriptFixture('/src/maison/ui/AirzoneAdd.svelte',`
   export function scan(){return search();}
   export function stop(){stopSearch();}
   export function enterManually(){manual();}
   export function select(index){return choose(results.gateways[index]);}
   export function save(){return install();}
   export function goBack(){back();}
   export function change(field,value){edit(field,value);}
   export function verifyManual(){return verify();}
   export function inspect(){return {stage,results,checked,installed,pending,awaitingPin,failure,draft:{...draft}};}
 `);
});
after(()=>{for(const[k,d]of globals){if(d)Object.defineProperty(globalThis,k,d);else delete globalThis[k];}});
const gateway={host:'192.168.77.20',port:3000,tls_fingerprint:null,mac:'aabbccddeeff',gateway_type:'ws_az',api_version:null,firmware:'4.21',systems:2,zones:12,incomplete:false,read_only:true,existing_instance:null,existing_enabled:null};
const report={gateways:[gateway],scanned:254,target_count:254,incomplete:false};
const preview={...gateway,instance:'airzone-2'};
const saved={instance:'airzone-2',stored:true,read_only:true,already_installed:false};
const response=(data,status=200)=>({ok:status===200,status,json:async()=>data});
const turn=()=>new Promise(resolve=>setImmediate(resolve));
function fixture(t,handler=(url)=>response(url.endsWith('/discover')?report:url.endsWith('/probe')?preview:saved)){
 const calls=[];t.mock.method(globalThis,'fetch',async(url,options)=>{calls.push({url,body:JSON.parse(options.body),signal:options.signal});return handler(url,options);});
 client.home.held=null;const mounted=client.mount({drivers:[],connected:true});
 t.after(()=>{mounted.dispose();client.home.held=null;});return {...mounted,calls};
}

test('wizard starts without requests and choosing discovered hardware only verifies before explicit add',async(t)=>{
 const {component,calls}=fixture(t);assert.equal(component.inspect().stage,'search');assert.equal(calls.length,0);
 await component.scan();assert.equal(component.inspect().stage,'choose');assert.equal(calls.length,1);
 await component.select(0);assert.equal(calls.length,2);assert.ok(calls[1].url.endsWith('/probe'));assert.equal(component.inspect().checked.report.mac,gateway.mac);assert.equal(component.inspect().installed,null);
 await component.save();assert.equal(calls.length,3);assert.equal(calls[2].body.expected_mac,gateway.mac);assert.equal(component.inspect().installed.stored,true);
});

test('stopping a search aborts its request and discards a late response',async(t)=>{
 let resolve;const{component,calls}=fixture(t,()=>new Promise(done=>{resolve=done;}));const searching=component.scan();
 assert.equal(component.inspect().pending,'scan');component.stop();assert.equal(calls[0].signal.aborted,true);
 resolve(response(report));await searching;assert.equal(component.inspect().results,null);assert.equal(component.inspect().stage,'search');assert.equal(component.inspect().pending,null);
});

test('switching to manual input invalidates pending discovery and preserves a typed address on return',async(t)=>{
 let resolve;const{component}=fixture(t,()=>new Promise(done=>{resolve=done;}));const searching=component.scan();component.enterManually();component.change('host','192.168.77.30');
 resolve(response(report));await searching;assert.equal(component.inspect().stage,'manual');assert.equal(component.inspect().results,null);
 component.goBack();component.enterManually();assert.equal(component.inspect().draft.host,'192.168.77.30');
});

test('leaving or cancelling scan authentication cannot start a delayed search',async(t)=>{
 const{component,calls,dispose}=fixture(t,()=>response({},403));await component.scan();assert.equal(client.home.held.reason,'discovery');
 const held=client.home.held;client.home.held=null;held.onCancel();await held.custom();assert.equal(calls.length,1);assert.equal(component.inspect().failure.error,'cancelled');
 await component.scan();const stale=client.home.held.custom;dispose();await stale();assert.equal(calls.length,2);
});

test('authentication resumes only discovery and reconnecting cannot add or rescan automatically',async(t)=>{
 let permitted=false;const{component,calls,props}=fixture(t,(url)=>permitted?response(report):response({},403));
 await component.scan();permitted=true;const approve=client.home.held.custom;client.home.held=null;await approve();assert.equal(component.inspect().stage,'choose');assert.equal(calls.length,2);
 props.connected=false;client.flushSync();props.connected=true;client.flushSync();props.drivers=[{instance:'airzone-3'}];client.flushSync();await turn();assert.equal(calls.length,2);assert.equal(component.inspect().installed,null);
});

test('known and paused discoveries cannot be installed under another identity',async(t)=>{
 const{component,calls}=fixture(t,()=>response({...report,gateways:[{...gateway,existing_instance:'already-configured',existing_enabled:false}]}));
 await component.scan();await component.select(0);await component.save();assert.equal(calls.length,1);assert.equal(component.inspect().checked,null);
});

test('a different MAC at the discovered address is refused before confirmation',async(t)=>{
 const{component,calls}=fixture(t,(url)=>response(url.endsWith('/discover')?report:{...preview,mac:'aabbccddee01'}));
 await component.scan();await component.select(0);assert.equal(component.inspect().checked,null);assert.equal(component.inspect().failure.error,'identity_changed');await component.save();assert.equal(calls.length,2);
});

test('empty, incomplete and rate-limited searches keep the manual fallback actionable',async(t)=>{
 let mode='empty';const{component,calls}=fixture(t,()=>mode==='busy'?response({error:'busy'},429):response({...report,gateways:[],incomplete:mode==='partial'}));
 await component.scan();assert.equal(component.inspect().stage,'choose');assert.equal(component.inspect().results.gateways.length,0);
 mode='partial';await component.scan();assert.equal(component.inspect().results.incomplete,true);
 mode='busy';await component.scan();assert.equal(component.inspect().failure.error,'busy');component.enterManually();assert.equal(component.inspect().stage,'manual');assert.equal(calls.length,3);
});

test('going back from confirmation invalidates the prior selection and never writes',async(t)=>{
 const{component,calls}=fixture(t);await component.scan();await component.select(0);component.goBack();assert.equal(component.inspect().stage,'choose');assert.equal(component.inspect().checked,null);await component.save();assert.equal(calls.length,2);
});
