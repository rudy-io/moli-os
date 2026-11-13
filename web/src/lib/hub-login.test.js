import {test,before,after,beforeEach} from 'node:test';
import assert from 'node:assert/strict';
import {createServer} from 'vite';
import {fileURLToPath} from 'node:url';
let vite,hub,login,home,confirmHeld,words;
const originals=new Map(['document','localStorage'].map(key=>[key,Object.getOwnPropertyDescriptor(globalThis,key)]));
before(async()=>{
 delete globalThis.localStorage;globalThis.document={querySelector:()=>null,documentElement:{}};
 vite=await createServer({root:fileURLToPath(new URL('../../',import.meta.url)),server:{middlewareMode:true,watch:null,hmr:false},logLevel:'error'});
 ({hub,login}=await vite.ssrLoadModule('/src/lib/hub.svelte.js'));({home,confirmHeld}=await vite.ssrLoadModule('/src/maison/lib/home.svelte.js'));words=await vite.ssrLoadModule('/src/lib/i18n.svelte.js');
});
beforeEach(async()=>{home.held=null;hub.session={human:false};await words.setLanguage('fr');});
after(async()=>{await vite?.close();for(const[key,value]of originals){if(value)Object.defineProperty(globalThis,key,value);else delete globalThis[key];}});
const session=()=>({ok:true,status:200,json:async()=>({human:false,language:'fr'})});
test('a plain-text cross-site rejection remains a readable refusal and never continues the Airzone action',async(t)=>{
 const calls=[];let continued=0;
 t.mock.method(globalThis,'fetch',async(url,options)=>{calls.push({url,method:options?.method});return options?.method==='POST'?new Response('cross-site request rejected',{status:403,headers:{'content-type':'text/plain'}}):session();});
 const held={reason:'airzone_control',custom:async()=>{continued++;}};home.held=held;
 const error=await confirmHeld('000000');assert.equal(error,words.t('commun.code.erreur',{status:403}));assert.doesNotMatch(error,/JSON|Unexpected|cross-site/);assert.equal(continued,0);assert.equal(home.held.reason,held.reason);assert.equal(hub.session.human,false);assert.deepEqual(calls,[{url:'/api/session',method:'POST'},{url:'/api/session',method:undefined}]);
});
test('JSON login errors keep their message while malformed error values use the HTTP fallback',async(t)=>{
 for(const body of [{error:'Code incorrect'},{error:{private:'data'}},null]){
  t.mock.method(globalThis,'fetch',async(url,options)=>options?.method==='POST'?new Response(JSON.stringify(body),{status:403,headers:{'content-type':'application/json'}}):session());
  assert.equal(await login('000000'),typeof body?.error==='string'?body.error:words.t('commun.code.erreur',{status:403}));
 }
});
test('successful login still refreshes the session and continues the held action once',async(t)=>{
 let continued=0;t.mock.method(globalThis,'fetch',async()=>({ok:true,status:200,json:async()=>({human:true,language:'fr'})}));
 home.held={reason:'airzone_control',custom:async()=>{continued++;}};assert.equal(await confirmHeld('000000'),null);assert.equal(continued,1);assert.equal(home.held,null);assert.equal(hub.session.human,true);
});
