import {test,before,after} from 'node:test';
import assert from 'node:assert/strict';
import {createServer} from 'vite';
import {fileURLToPath} from 'node:url';
let vite,render,Alerts,hub,words;
const originals=new Map(['document','localStorage'].map(key=>[key,Object.getOwnPropertyDescriptor(globalThis,key)]));
before(async()=>{
 delete globalThis.localStorage;globalThis.document={querySelector:()=>null,documentElement:{}};
 vite=await createServer({root:fileURLToPath(new URL('../../../',import.meta.url)),server:{middlewareMode:true,watch:null,hmr:false},logLevel:'error'});
 ({render}=await vite.ssrLoadModule('svelte/server'));Alerts=(await vite.ssrLoadModule('/src/maison/ui/AirzoneAlerts.svelte')).default;({hub}=await vite.ssrLoadModule('/src/maison/lib/home.svelte.js'));words=await vite.ssrLoadModule('/src/lib/i18n.svelte.js');
 hub.drivers=[{instance:'synthetic',kind:'airzone'}];hub.connected=true;
});
after(async()=>{await vite?.close();for(const[key,value]of originals){if(value)Object.defineProperty(globalThis,key,value);else delete globalThis[key];}});
const device={id:'synthetic:zone',instance:'synthetic',model:'Local API',native_name:'Example zone',online:false,points:[],state:{}};
const report={items:[{id:'offline-example',kind:'offline',deviceId:device.id,instance:'synthetic',severity:'warning'}],diagnostics:{reported:0,unknown:1}};
const text=html=>html.replace(/<!--.*?-->/g,'').replace(/<[^>]*>/g,' ').replace(/\s+/g,' ').trim();
test('an alert exposes a single explicit read check and preserves zone and installation links in both languages',async()=>{
 for(const [lang,label]of [['fr','Vérifier maintenant'],['en','Check now']]){
  await words.setLanguage(lang);const html=render(Alerts,{props:{report,devices:[device]}}).body;
  assert.match(text(html),new RegExp(label));assert.match(html,/class="verify\b/);assert.doesNotMatch(html,/<input|<select|<form|airzone_health\./);
  assert.match(html,/href="#\/airzone\/synthetic%3Azone"/);assert.match(html,/href="#\/airzone\/installation"/);assert.doesNotMatch(text(html),/acquitter|dismiss|reset device/i);
 }
});
test('hidden devices retain checks and route to their management page without suppressing the alert',async()=>{
 await words.setLanguage('fr');const html=render(Alerts,{props:{report,devices:[device],hidden:[device.id]}}).body;
 assert.match(text(html),/Appareil injoignable/);assert.match(text(html),/Vérifier maintenant/);assert.match(text(html),/Masqué dans Moli/);assert.match(html,/href="#\/airzone\/devices\/synthetic%3Azone"/);
});
test('a disconnected hub exposes reconnection guidance without inventing a gateway read target',()=>{
 const connection={items:[{id:'connection',kind:'connection',deviceId:null,instance:null,severity:'warning'}],diagnostics:{reported:0,unknown:0}};
 const html=render(Alerts,{props:{report:connection,devices:[device]}}).body;assert.doesNotMatch(html,/class="verify\b/);assert.match(html,/href="#\/airzone\/installation"/);
});
