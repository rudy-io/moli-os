import {test,before,after} from 'node:test';
import assert from 'node:assert/strict';
import {scriptFixture} from '../../../test-support/svelte-script-fixture.js';
let client;
const pageEvents=new Map(),windowEvents=new Map();
const originals=new Map(['document','localStorage','addEventListener','removeEventListener'].map(key=>[key,Object.getOwnPropertyDescriptor(globalThis,key)]));
before(async()=>{
 delete globalThis.localStorage;globalThis.document={querySelector:()=>null,documentElement:{},visibilityState:'visible',addEventListener:(key,fn)=>pageEvents.set(key,fn),removeEventListener:key=>pageEvents.delete(key)};globalThis.addEventListener=(key,fn)=>windowEvents.set(key,fn);globalThis.removeEventListener=key=>windowEvents.delete(key);
 client=await scriptFixture('/src/maison/ui/AirzoneCloud.svelte',`
 export function session(human){hub.session.human=human;}
 export function access(){authorize(open);}
 export function credentials(mail,secret){email=mail;password=secret;}
 export function signIn(){return login();}
 export function selectInstallation(id){installationId=id;return chooseInstallation();}
 export function selectDevice(id){deviceId=id;return chooseDevice();}
 export function pulse(){return poll();}
 export function refresh(){refreshReading();}
 export function closeAccount(){return disconnect();}
 export function inspectState(){return {opened,status,busy,error,email,password,installations,installationId,deviceId,lastReading,lastError,pollPaused,hubDevices:Object.keys(hub.devices)};}
 `);
});
after(()=>{for(const[key,value]of originals){if(value)Object.defineProperty(globalThis,key,value);else delete globalThis[key];}});
const response=(data,status=200)=>({ok:status===200,status,json:async()=>data});
const status={state:'connected',read_only:true,persistence_available:true,expires_at:9999999999};
const read=data=>({read_only:true,received_at:Date.now(),data});
const turn=()=>new Promise(resolve=>setImmediate(resolve));
function fixture(t,handler){
 const calls=[];t.mock.method(globalThis,'fetch',async(url,options)=>{calls.push({url,...options,body:options.body?JSON.parse(options.body):null});return handler(url,options);});
 client.home.held=null;const mounted=client.mount({});mounted.component.session(true);
 t.after(()=>{mounted.dispose();client.home.held=null;document.visibilityState='visible';});return {...mounted,calls};
}
test('opening cloud waits for PIN and login clears the password before any response without persisting credentials',async(t)=>{
 let finish;const f=fixture(t,(url)=>url.endsWith('/login')?new Promise(resolve=>finish=resolve):response(url.endsWith('/status')?{...status,state:'disconnected'}:read([])));
 f.component.session(false);f.component.access();assert.equal(f.calls.length,0);assert.ok(client.home.held);
 const held=client.home.held;client.home.held=null;await held.custom();assert.equal(f.calls.length,1);
 f.component.credentials('user@example.com','synthetic-password');const pending=f.component.signIn();assert.equal(f.component.inspectState().password,'');assert.equal(f.calls[1].body.password,'synthetic-password');finish(response(status));await pending;
 assert.equal(f.component.inspectState().email,'');assert.equal(f.component.inspectState().password,'');assert.equal(f.component.inspectState().status.state,'connected');
 assert.deepEqual(f.component.inspectState().hubDevices,[]);assert.ok(f.calls.every(call=>!call.url.includes('/api/command')));
});
test('device polling is read-only, pauses hidden and on auth expiry, and preserves the last known reading',async(t)=>{
 let statusCode=200;
 const f=fixture(t,(url,options)=>{
  if(url.endsWith('/status'))return response(status);
  const body=JSON.parse(options.body);const data=body.operation==='installations'?[{installation_id:'installation',name:'Synthetic installation',ws_ids:['gateway']}]:body.operation==='installation'?{installation_id:'installation',location_id:'location',groups:[{name:'Group',devices:[{device_id:'device',name:'Synthetic device',ws_id:'gateway'}]}]}:{power:false,local_temp:{celsius:21}};
  return response(read(data),body.operation==='device_status'?statusCode:200);
 });
 f.component.access();await turn();await f.component.selectInstallation('installation');await f.component.selectDevice('device');assert.equal(f.component.inspectState().lastReading.data.power,false);
 const before=f.calls.length;document.visibilityState='hidden';await f.component.pulse();assert.equal(f.calls.length,before);document.visibilityState='visible';await f.component.pulse();assert.equal(f.calls.length,before+1);
 statusCode=403;await f.component.pulse();assert.equal(f.component.inspectState().pollPaused,true);assert.equal(f.component.inspectState().lastReading.data.local_temp.celsius,21);const expiredCount=f.calls.length;await f.component.pulse();assert.equal(f.calls.length,expiredCount);assert.deepEqual(f.component.inspectState().hubDevices,[]);
});
test('switching selection invalidates an older cloud response and disconnect clears private selections',async(t)=>{
 let late;
 const f=fixture(t,(url,options)=>{
  if(url.endsWith('/status'))return response(status);if(url.endsWith('/disconnect'))return response({...status,state:'disconnected'});
  const body=JSON.parse(options.body);
  if(body.operation==='installations')return response(read([{installation_id:'installation'}]));
  if(body.operation==='installation')return response(read({groups:[{devices:[{device_id:'a'},{device_id:'b'}]}]}));
  if(body.device_id==='a')return new Promise(resolve=>late=resolve);
  return response(read({power:false}));
 });
 f.component.access();await turn();await f.component.selectInstallation('installation');const first=f.component.selectDevice('a');await f.component.selectDevice('b');late(response(read({power:true})));await first;
 assert.equal(f.component.inspectState().deviceId,'b');assert.equal(f.component.inspectState().lastReading.data.power,false);
 await f.component.closeAccount();assert.equal(f.component.inspectState().lastReading,null);assert.equal(f.component.inspectState().installations.length,0);assert.equal(f.component.inspectState().status.state,'disconnected');
});
test('an expired cloud access token renews through read without asking for a password or changing the Moli session',async(t)=>{
 let statusReads=0;const f=fixture(t,(url)=>url.endsWith('/status')?response({...status,state:++statusReads===1?'expired':'connected'}):response(read([{installation_id:'installation'}])));
 f.component.access();await turn();assert.equal(f.component.inspectState().status.state,'connected');assert.equal(statusReads,2);assert.equal(f.calls.filter(call=>call.url.endsWith('/login')).length,0);assert.equal(client.home.held,null);
});

test('polling clock is bounded to 30 seconds, resumes once on foreground, and is removed when leaving the page',async(t)=>{
 const timers=new Map();let id=0;
 t.mock.method(globalThis,'setInterval',(fn,ms)=>{timers.set(++id,{fn,ms});return id;});t.mock.method(globalThis,'clearInterval',key=>timers.delete(key));
 const f=fixture(t,(url,options)=>{
  if(url.endsWith('/status'))return response(status);const body=JSON.parse(options.body);
  return response(read(body.operation==='installations'?[{installation_id:'installation'}]:body.operation==='installation'?{groups:[{devices:[{device_id:'device'}]}]}:{power:true}));
 });
 assert.deepEqual([...timers.values()].map(timer=>timer.ms).sort((a,b)=>a-b),[1000,30000]);
 f.component.access();await turn();await f.component.selectInstallation('installation');await f.component.selectDevice('device');
 const before=f.calls.length;document.visibilityState='hidden';pageEvents.get('visibilitychange')();[...timers.values()].find(timer=>timer.ms===30000).fn();await turn();assert.equal(f.calls.length,before);
 document.visibilityState='visible';pageEvents.get('visibilitychange')();await turn();assert.equal(f.calls.length,before+1);
 f.dispose();assert.equal(timers.size,0);assert.equal(pageEvents.has('visibilitychange'),false);assert.equal(windowEvents.has('online'),false);
});

test('manual refresh retries a forbidden cloud reading once after permission returns while the timer stays paused',async(t)=>{
 let forbidden=true;
 const f=fixture(t,(url,options)=>{
  if(url.endsWith('/status'))return response(status);const body=JSON.parse(options.body);
  if(body.operation==='installations')return response(read([{installation_id:'installation'}]));
  if(body.operation==='installation')return response(read({groups:[{devices:[{device_id:'device'}]}]}));
  return forbidden?response({error:'forbidden'},403):response(read({power:true}));
 });
 f.component.access();await turn();await f.component.selectInstallation('installation');await f.component.selectDevice('device');assert.equal(f.component.inspectState().pollPaused,true);assert.equal(f.component.inspectState().error,'cloud_forbidden');
 const before=f.calls.length;await f.component.pulse();assert.equal(f.calls.length,before);forbidden=false;f.component.refresh();await turn();assert.equal(f.calls.length,before+1);assert.equal(f.component.inspectState().lastReading.data.power,true);assert.equal(f.component.inspectState().lastError,'');assert.equal(client.home.held,null);
});
