import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { existsSync } from 'node:fs';
import { scriptFixture } from '../../../test-support/svelte-script-fixture.js';

let client;
const globals=new Map(['document','localStorage'].map((key)=>[key,Object.getOwnPropertyDescriptor(globalThis,key)]));
before(async()=>{
  delete globalThis.localStorage;globalThis.document={querySelector:()=>null,documentElement:{}};
  if (!existsSync(new URL('../ui/AirzoneGateways.svelte',import.meta.url))) return;
  client=await scriptFixture('/src/maison/ui/AirzoneGateways.svelte',`
    import { confirmHeld } from '../lib/home.svelte.js';
    export function submitCode(pin) {return confirmHeld(pin);}
    export function refresh() {return load();}
    export function close() {return closeEditor();}
    export function select(instance,kind) {choose(instance,kind);}
    export function confirm() {return confirmWithdrawal();}
    export function confirmModeChange() {return confirmMode();}
    export function cancel() {cancelSelection();}
    export function addReplacement() {startReplacement();}
    export function inspect() {return {gateways,pending,awaitingPin,failure,editing,confirmation,replacement,uncertain,creating,modeResult};}
  `);
});
after(()=>{for(const[key,descriptor]of globals){if(descriptor)Object.defineProperty(globalThis,key,descriptor);else delete globalThis[key];}});
const turn=()=>new Promise(resolve=>setImmediate(resolve));
const gateway={instance:'airzone-old',host:'172.31.1.1',port:3000,tls_fingerprint:null,mac:'aabbccddeeff',enabled:true,source:'configuration',read_only:false,running:true};
const response=(data,status=200)=>({ok:status>=200&&status<300,status,json:async()=>data});
function fixture(t,handler=(url)=>response(url.endsWith('/withdraw')?{instance:gateway.instance,stored:true,enabled:false,read_only:false}:{gateways:[gateway]})) {
  assert.ok(client,'the installation must expose stored gateway management');
  const calls=[];t.mock.method(globalThis,'fetch',async(url,options)=>{calls.push({url,method:options.method,body:options.body});return handler(url,options);});
  client.home.held=null;const mounted=client.mount({connected:true,drivers:[],registryRevision:0});
  t.after(()=>{mounted.dispose();client.home.held=null;});
  return {...mounted,calls};
}

test('stored gateways load without live drivers; pause needs an explicit confirmation',async(t)=>{
  const{component,calls}=fixture(t);await turn();assert.equal(component.inspect().gateways.length,1);
  component.select(gateway.instance,'withdraw');assert.equal(calls.length,1);assert.ok(component.inspect().confirmation);
  await component.confirm();assert.equal(calls.length,2);assert.equal(calls[1].body,'{}');
  const retained=component.inspect().gateways[0];assert.equal(retained.enabled,false);assert.equal(retained.running,false);assert.equal(retained.read_only,false);
});

test('replacement withdraws first and only then offers a new, separate gateway',async(t)=>{
  const{component,calls}=fixture(t);await turn();component.select(gateway.instance,'replace');
  assert.equal(component.inspect().replacement,null);await component.confirm();
  assert.equal(component.inspect().replacement,gateway.instance);assert.equal(calls.length,2);
  assert.ok(calls.every(c=>c.method==='GET'||c.url.endsWith('/withdraw')),'never installs a replacement automatically');
});

test('replacing a disabled TOML gateway still persists its withdrawal before offering another identity',async(t)=>{
  const {component,calls}=fixture(t,(url)=>response(url.endsWith('/withdraw')
    ? {instance:gateway.instance,stored:true,enabled:false,read_only:true}
    : {gateways:[{...gateway,enabled:false,running:false,mac:null}]}));
  await turn();component.select(gateway.instance,'replace');
  assert.equal(component.inspect().replacement,null);
  assert.ok(component.inspect().confirmation);
  component.addReplacement();assert.equal(component.inspect().creating,false);
  await component.confirm();
  assert.equal(calls[1].url,`/api/integrations/airzone/${gateway.instance}/withdraw`);
  component.addReplacement();assert.equal(component.inspect().creating,true);
});

test('PIN cancellation and unmount cannot replay a pending list or withdrawal action',async(t)=>{
  let allowed=false;const{component,calls,dispose}=fixture(t,(url)=>!allowed?response({},403):response(url.endsWith('/withdraw')?{instance:gateway.instance,stored:true,enabled:false,read_only:true}:{gateways:[gateway]}));
  await turn();const first=client.home.held;client.home.held=null;first.onCancel();await first.custom();assert.equal(calls.length,1);
  await component.refresh();const approval=client.home.held.custom;client.home.held=null;allowed=true;await approval();assert.equal(component.inspect().gateways.length,1);
  allowed=false;component.select(gateway.instance,'withdraw');await component.confirm();const stale=client.home.held.custom;dispose();await stale();assert.equal(calls.length,4);
});

test('a lost withdrawal response keeps the confirmation for explicit retry without choosing a different target',async(t)=>{
  let tries=0;const{component,calls}=fixture(t,(url)=>{
    if(!url.endsWith('/withdraw'))return response({gateways:[gateway]});
    if(++tries===1)throw new Error('lost');return response({instance:gateway.instance,stored:true,enabled:false,read_only:false});
  });
  await turn();component.select(gateway.instance,'withdraw');await component.confirm();
  assert.equal(component.inspect().uncertain,true);assert.equal(component.inspect().gateways[0].enabled,true);assert.equal(calls.length,2);
  await component.confirm();assert.deepEqual(calls[1],calls[2]);assert.equal(component.inspect().gateways[0].enabled,false);
});

test('busy, stale and disconnected actions cannot change another gateway',async(t)=>{
  let settle;const{component,calls,props,dispose}=fixture(t,()=>new Promise(resolve=>{settle=resolve;}));
  const loading=component.refresh();await component.refresh();component.select(gateway.instance,'withdraw');assert.equal(calls.length,1);assert.equal(component.inspect().confirmation,null);
  settle(response({gateways:[gateway]}));await loading;await turn();
  props.connected=false;client.flushSync();component.select(gateway.instance,'withdraw');await component.confirm();assert.equal(calls.length,1);
  props.connected=true;client.flushSync();const refreshing=component.refresh();dispose();settle(response({gateways:[]}));await refreshing;
  assert.equal(component.inspect().gateways.length,1);
});


test('replacement opens the shared add flow only after withdrawal and reserves paused instance identifiers',async(t)=>{
  const{component,calls}=fixture(t);await turn();
  component.addReplacement();assert.equal(component.inspect().creating,false);
  component.select(gateway.instance,'replace');component.addReplacement();assert.equal(component.inspect().creating,false);
  await component.confirm();component.addReplacement();assert.equal(component.inspect().creating,true);
  assert.equal(component.inspect().gateways[0].instance,gateway.instance);assert.equal(calls.length,2);
});


test('returning from an editor always reloads persisted state even without a success callback',async(t)=>{
  let updated=false;const{component,calls}=fixture(t,()=>response({gateways:[{...gateway,host:updated?'172.31.1.2':gateway.host}]}));
  await turn();component.select(gateway.instance,'edit');updated=true;
  await component.close();assert.equal(calls.length,2);assert.equal(component.inspect().gateways[0].host,'172.31.1.2');
});

test('registry events refresh an idle list and defer to an open editor without changing its draft',async(t)=>{
  let enabled=true;const{component,calls,props}=fixture(t,()=>response({gateways:[{...gateway,enabled,running:enabled}]}));
  await turn();enabled=false;props.registryRevision++;client.flushSync();await turn();
  assert.equal(calls.length,2);assert.equal(component.inspect().gateways[0].enabled,false);
  component.select(gateway.instance,'edit');enabled=true;props.registryRevision++;client.flushSync();await turn();
  assert.equal(calls.length,2);assert.equal(component.inspect().editing.enabled,false);
  await component.close();await turn();assert.equal(calls.length,3);assert.equal(component.inspect().gateways[0].enabled,true);
});

test('reconnect refreshes once and queued events never retry a mutation or reopen a cancelled PIN prompt',async(t)=>{
  let auth=false;const{component,calls,props}=fixture(t,()=>auth?response({},403):response({gateways:[gateway]}));
  await turn();props.connected=false;client.flushSync();props.connected=true;client.flushSync();await turn();assert.equal(calls.length,2);
  auth=true;props.registryRevision++;client.flushSync();await turn();assert.equal(calls.length,3);assert.equal(client.home.held,null);
  await component.refresh();assert.ok(client.home.held);props.registryRevision++;client.flushSync();await turn();assert.equal(calls.length,4);
  const cancelled=client.home.held;client.home.held=null;cancelled.onCancel();client.flushSync();await turn();
  assert.equal(client.home.held,null);assert.equal(calls.length,5);await turn();assert.equal(calls.length,5);
});

async function approveMode(component) {
  await component.confirmModeChange();
  const held=client.home.held;
  if (held) {client.home.held=null;await held.custom('482915');}
}

test('both modes require confirmation while only granting control asks for a fresh code',async(t)=>{
  let readOnly=true;
  const other={...gateway,instance:'airzone-other',read_only:true};
  const{component,calls}=fixture(t,(url,options)=>{
    if(options.method==='GET')return response({gateways:[{...gateway,read_only:readOnly},other]});
    assert.ok(url.endsWith('/mode'));
    const body=JSON.parse(options.body);assert.equal(body.expected_read_only,readOnly);readOnly=body.read_only;
    return response({instance:gateway.instance,stored:true,read_only:readOnly,running:true});
  });
  await turn();component.select(gateway.instance,'mode');
  assert.equal(calls.length,1);assert.equal(client.home.held,null);
  await component.confirmModeChange();assert.equal(calls.length,1);
  assert.equal(client.home.held.reason,'airzone_control');
  assert.match(client.home.held.label,new RegExp(gateway.instance));
  const first=client.home.held;client.home.held=null;await first.custom('482915');
  assert.equal(component.inspect().gateways[0].read_only,false);
  assert.equal(component.inspect().gateways[1].read_only,true);
  assert.equal(component.inspect().confirmation,null);
  assert.deepEqual(component.inspect().modeResult,{instance:gateway.instance,read_only:false});
  component.select(gateway.instance,'mode');await component.confirmModeChange();
  assert.equal(calls.length,3);assert.equal(client.home.held,null);
  assert.equal(component.inspect().gateways[0].read_only,true);
  assert.deepEqual(calls.slice(1).map(c=>JSON.parse(c.body)),[{read_only:false,expected_read_only:true,pin:'482915'},{read_only:true,expected_read_only:false}]);
  assert.ok(calls.every(c=>c.method==='GET'||c.url===`/api/integrations/airzone/${gateway.instance}/mode`),'no physical command endpoint');
});

test('cancelled or superseded PIN intent cannot grant control or retarget a mode transition',async(t)=>{
  const{component,calls,dispose}=fixture(t,()=>response({gateways:[{...gateway,read_only:true}]}));
  await turn();component.select(gateway.instance,'mode');await component.confirmModeChange();
  const cancelled=client.home.held;client.home.held=null;cancelled.onCancel();await cancelled.custom();
  assert.equal(calls.length,1);assert.equal(component.inspect().failure.error,'cancelled');
  await component.confirmModeChange();const stale=client.home.held;
  component.select('other','mode');assert.equal(component.inspect().confirmation.instance,gateway.instance);
  dispose();await stale.custom();assert.equal(calls.length,1);
});

test('lost mode response is checked by a read, never by automatically replaying authorization',async(t)=>{
  let changed=false;
  const{component,calls,props}=fixture(t,(url,options)=>{
    if(options.method==='GET')return response({gateways:[{...gateway,read_only:!changed}]});
    changed=true;throw new Error('response lost');
  });
  await turn();component.select(gateway.instance,'mode');await approveMode(component);
  assert.equal(component.inspect().uncertain,true);
  props.registryRevision++;client.flushSync();await turn();assert.equal(calls.length,2);
  await component.confirmModeChange();
  assert.equal(calls.length,3);assert.equal(calls[2].method,'GET');assert.equal(client.home.held,null);
  assert.equal(component.inspect().gateways[0].read_only,false);
  assert.equal(component.inspect().confirmation,null);
  assert.equal(calls.filter(c=>c.url.endsWith('/mode')).length,1);
});

test('a mode conflict reloads server truth and requires a new explicit choice',async(t)=>{
  let changed=false;
  const{component,calls}=fixture(t,(url,options)=>{
    if(options.method==='GET')return response({gateways:[{...gateway,read_only:!changed}]});
    changed=true;return response({error:'mode_conflict'},409);
  });
  await turn();component.select(gateway.instance,'mode');await approveMode(component);
  assert.equal(calls.length,3);assert.equal(calls[2].method,'GET');
  assert.equal(component.inspect().failure.error,'mode_conflict');
  assert.equal(component.inspect().gateways[0].read_only,false);
  assert.equal(component.inspect().confirmation,null);assert.equal(component.inspect().modeResult,null);
  await component.confirmModeChange();assert.equal(calls.length,3);
});

test('paused gateways cannot grant control but can revoke previous permission',async(t)=>{
  let readOnly=true;
  const{component,calls}=fixture(t,(url,options)=>options.method==='GET'
    ?response({gateways:[{...gateway,enabled:false,running:false,read_only:readOnly}]})
    :response({instance:gateway.instance,stored:true,read_only:true,running:false}));
  await turn();component.select(gateway.instance,'mode');assert.equal(component.inspect().confirmation,null);assert.equal(calls.length,1);
  readOnly=false;await component.refresh();component.select(gateway.instance,'mode');await approveMode(component);
  assert.equal(component.inspect().gateways[0].read_only,true);assert.equal(component.inspect().gateways[0].enabled,false);
});

test('disconnecting during PIN approval cannot send a mode mutation',async(t)=>{
  const{component,calls,props}=fixture(t,()=>response({gateways:[{...gateway,read_only:true}]}));await turn();component.select(gateway.instance,'mode');await component.confirmModeChange();
  const held=client.home.held;props.connected=false;client.flushSync();client.home.held=null;await held.custom();
  assert.equal(calls.length,1);assert.equal(component.inspect().failure.error,'connection');
});

test('an external mode change removes an outdated success notice while keeping server truth',async(t)=>{
  let readOnly=true;
  const{component,props}=fixture(t,(url,options)=>{
    if(options.method==='GET')return response({gateways:[{...gateway,read_only:readOnly}]});
    readOnly=JSON.parse(options.body).read_only;
    return response({instance:gateway.instance,stored:true,read_only:readOnly,running:true});
  });
  await turn();component.select(gateway.instance,'mode');await approveMode(component);assert.equal(component.inspect().modeResult.read_only,false);
  readOnly=true;props.registryRevision++;client.flushSync();await turn();
  assert.equal(component.inspect().gateways[0].read_only,true);assert.equal(component.inspect().modeResult,null);
});


test('enabling sends the fresh code only with that exact mode request',async(t)=>{
  const {component,calls}=fixture(t,(url,options)=>options.method==='GET'
    ? response({gateways:[{...gateway,read_only:true}]})
    : response({instance:gateway.instance,stored:true,read_only:false,running:true}));
  await turn();component.select(gateway.instance,'mode');await component.confirmModeChange();
  const held=client.home.held;assert.equal(held.freshPin,true);
  assert.equal(await component.submitCode('482915'),null);
  assert.deepEqual(JSON.parse(calls.find(call=>call.url.endsWith('/mode')).body),{read_only:false,expected_read_only:true,pin:'482915'});
  assert.ok(!JSON.stringify(component.inspect()).includes('482915'));
});

test('revocation does not prompt again when the human session is valid',async(t)=>{
  const {component,calls}=fixture(t,(url,options)=>options.method==='GET'
    ? response({gateways:[gateway]})
    : response({instance:gateway.instance,stored:true,read_only:true,running:true}));
  await turn();component.select(gateway.instance,'mode');await component.confirmModeChange();
  assert.equal(client.home.held,null);
  assert.deepEqual(JSON.parse(calls[1].body),{read_only:true,expected_read_only:false});
  assert.equal(component.inspect().gateways[0].read_only,true);
});
