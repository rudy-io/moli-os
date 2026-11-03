import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { scriptFixture } from '../../../test-support/svelte-script-fixture.js';

let client;
const globals=new Map(['document','localStorage'].map((key)=>[key,Object.getOwnPropertyDescriptor(globalThis,key)]));
before(async()=>{
  delete globalThis.localStorage;
  globalThis.document={querySelector:()=>null,documentElement:{}};
  client=await scriptFixture('/src/maison/ui/AirzoneAdd.svelte',`
    export function change(field,value) {if(typeof edit==='function')edit(field,value);}
    export function runProbe() {return typeof verify==='function'?verify():undefined;}
    export function runInstall() {return typeof install==='function'?install():undefined;}
    export function again() {if(typeof restart==='function')restart();}
    export function intent() {
      if(typeof verify!=='function')return {implemented:false};
      return {implemented:true,draft:{...draft},pending,awaitingPin,checked,installed,failure,uncertain};
    }
  `);
});
after(()=>{for(const[key,descriptor]of globals){if(descriptor)Object.defineProperty(globalThis,key,descriptor);else delete globalThis[key];}});
const preview={instance:'airzone-2',mac:'aabbccddeeff',gateway_type:'ws_az',api_version:null,firmware:'4.21',systems:2,zones:12,incomplete:false,read_only:true};
const saved={instance:'airzone-2',stored:true,read_only:true,already_installed:false};
const response=(data,status=200)=>({ok:status>=200&&status<300,status,json:async()=>data});
function fixture(t,handler=(url)=>response(url.endsWith('/probe')?preview:saved),props={}) {
  const calls=[];
  t.mock.method(globalThis,'fetch',async(url,options)=>{calls.push({url,body:JSON.parse(options.body)});return handler(url,options);});
  client.home.held=null;
  const mounted=client.mount({drivers:[],connected:true,...props});
  t.after(()=>{mounted.dispose();client.home.held=null;});
  assert.equal(mounted.component.intent().implemented,true,'direct gateway workflow must exist');
  mounted.component.change('host','127.0.0.1');
  return {...mounted,calls};
}

test('checking only previews; a separate explicit action installs the checked identity',async(t)=>{
  const{component,calls}=fixture(t);
  await component.runInstall();assert.equal(calls.length,0);
  await component.runProbe();
  assert.equal(calls.length,1);assert.equal(component.intent().checked.report.zones,12);assert.equal(component.intent().installed,null);
  await component.runInstall();
  assert.equal(calls.length,2);assert.equal(calls[1].body.expected_mac,'aabbccddeeff');
  assert.deepEqual(component.intent().installed,saved);assert.equal(component.intent().checked,null);
  component.again();assert.equal(component.intent().draft.instance,'airzone-3');assert.equal(component.intent().draft.host,'');
});

test('editing any connection setting invalidates a checked gateway',async(t)=>{
  const{component,calls}=fixture(t);
  await component.runProbe();component.change('port','3001');
  assert.equal(component.intent().checked,null);
  await component.runInstall();assert.equal(calls.length,1);
});

test('double taps do not send a second probe or install and pending inputs cannot change target',async(t)=>{
  let settle;
  const{component,calls}=fixture(t,()=>new Promise((resolve)=>{settle=resolve;}));
  const checking=component.runProbe();
  await component.runProbe();component.change('host','127.0.0.2');
  assert.equal(calls.length,1);assert.equal(component.intent().draft.host,'127.0.0.1');
  settle(response(preview));await checking;
  const saving=component.runInstall();await component.runInstall();
  assert.equal(calls.length,2);settle(response(saved));await saving;
});

test('403 asks for a human and retries only the same user action after explicit validation',async(t)=>{
  let allowed=false;
  const{component,calls}=fixture(t,(url)=>allowed?response(url.endsWith('/probe')?preview:saved):response({},403));
  await component.runProbe();
  assert.equal(calls.length,1);assert.equal(component.intent().awaitingPin,true);assert.equal(component.intent().checked,null);
  const approve=client.home.held.custom;client.home.held=null;allowed=true;await approve();
  assert.equal(calls.length,2);assert.ok(component.intent().checked);assert.equal(component.intent().installed,null);
  allowed=false;await component.runInstall();
  const installApproval=client.home.held.custom;client.home.held=null;allowed=true;await installApproval();
  assert.equal(calls.length,4);assert.deepEqual(calls[2],calls[3]);assert.equal(component.intent().installed.stored,true);
});

test('cancelling a PIN request or leaving the form cannot later install a gateway',async(t)=>{
  const{component,calls,dispose}=fixture(t,()=>response({},403));
  await component.runProbe();const held=client.home.held;
  client.home.held=null;held.onCancel();await held.custom();
  assert.equal(calls.length,1);assert.equal(component.intent().failure.error,'cancelled');
  await component.runProbe();const stale=client.home.held.custom;dispose();await stale();
  assert.equal(calls.length,2);assert.equal(client.home.held,null);
});

test('a late probe response after leaving the page is discarded',async(t)=>{
  let settle;const{component,dispose}=fixture(t,()=>new Promise((resolve)=>{settle=resolve;}));
  const checking=component.runProbe();dispose();settle(response(preview));await checking;
  assert.equal(component.intent().checked,null);
});

test('a lost install response preserves checked identity for an explicit idempotent retry',async(t)=>{
  let installs=0;
  const{component,calls,props}=fixture(t,(url)=>{
    if(url.endsWith('/probe'))return response(preview);
    installs++;if(installs===1)throw new Error('response lost');
    return response({...saved,already_installed:true});
  });
  await component.runProbe();await component.runInstall();
  assert.equal(component.intent().uncertain,true);assert.equal(component.intent().installed,null);
  props.drivers=[{instance:'airzone-2',kind:'airzone'}];client.flushSync();
  assert.equal(calls.length,2);
  await component.runInstall();
  assert.deepEqual(calls[1],calls[2]);assert.equal(component.intent().installed.already_installed,true);
});

test('disconnects, identity changes and unrelated approvals cannot authorize an installation',async(t)=>{
  let mode='good';const{component,calls,props}=fixture(t,(url)=>mode==='identity'?response({error:'identity_changed'},409):mode==='auth'?response({},403):response(url.endsWith('/probe')?preview:saved));
  await component.runProbe();props.connected=false;client.flushSync();await component.runInstall();assert.equal(calls.length,1);
  props.connected=true;client.flushSync();mode='identity';await component.runInstall();
  assert.equal(component.intent().checked,null);assert.equal(component.intent().failure.error,'identity_changed');
  mode='auth';const unrelated={custom:()=>{}};client.home.held=unrelated;await component.runProbe();
  assert.equal(client.home.held.custom,unrelated.custom);assert.equal(component.intent().failure.error,'auth_busy');
});

test('editing and resuming a gateway preserves its stable instance and uses the lifecycle endpoints',async(t)=>{
  const gateway={instance:'airzone-2',host:'127.0.0.1',port:3001,tls_fingerprint:null,mac:preview.mac,enabled:false,read_only:false};
  const{component,calls}=fixture(t,undefined,{gateway});
  component.change('instance','different');assert.equal(component.intent().draft.instance,gateway.instance);
  assert.equal(component.intent().draft.port,'3001');
  await component.runProbe();assert.equal(calls[0].url,'/api/integrations/airzone/airzone-2/probe');
  await component.runInstall();assert.equal(calls[1].url,'/api/integrations/airzone/airzone-2');assert.equal(calls[1].body.expected_mac,preview.mac);
});

test('editing refuses a different hardware identity even if a malformed server approves its probe',async(t)=>{
  const gateway={instance:'airzone-2',host:'127.0.0.1',port:3000,tls_fingerprint:null,mac:'112233445566',enabled:true,read_only:true};
  const{component,calls}=fixture(t,undefined,{gateway});
  await component.runProbe();assert.equal(component.intent().checked,null);assert.equal(component.intent().failure.error,'identity_changed');
  await component.runInstall();assert.equal(calls.length,1);
});

test('editing a legacy configured ID preserves whitespace and punctuation in both body and path',async(t)=>{
  const instance=' Airzone étage.v1 ';
  const gateway={instance,host:'127.0.0.1',port:3000,tls_fingerprint:null,mac:preview.mac,enabled:true,read_only:true};
  const{component,calls}=fixture(t,(url)=>response(url.endsWith('/probe')?{...preview,instance}:{...saved,instance}),{gateway});
  await component.runProbe();await component.runInstall();assert.equal(calls.length,2);
  assert.equal(calls[0].body.instance,instance);assert.equal(calls[1].body.instance,instance);
  assert.equal(calls[1].url,'/api/integrations/airzone/'+encodeURIComponent(instance));
});
