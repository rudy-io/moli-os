import {test,before,after} from 'node:test';
import assert from 'node:assert/strict';
import {fileURLToPath} from 'node:url';
import {createServer} from 'vite';
let vite,render,Health,words;
const storage=Object.getOwnPropertyDescriptor(globalThis,'localStorage');
before(async()=>{
 delete globalThis.localStorage;
 vite=await createServer({root:fileURLToPath(new URL('../../../',import.meta.url)),server:{middlewareMode:true,watch:null,hmr:false},logLevel:'error'});
 ({render}=await vite.ssrLoadModule('svelte/server'));
 words=await vite.ssrLoadModule('/src/lib/i18n.svelte.js');await words.setLanguage('fr');
 Health=(await vite.ssrLoadModule('/src/maison/ui/AirzoneThermostatHealth.svelte')).default;
});
after(async()=>{await vite?.close();if(storage)Object.defineProperty(globalThis,'localStorage',storage);});
function zone(values){return {online:true,points:Object.entries(values).map(([key,value])=>({key,kind:{type:typeof value==='number'?'numeric':typeof value==='boolean'?'binary':'text'},access:{read:true,write:false},unit:['battery','coverage'].includes(key)?'%':null})),state:Object.fromEntries(Object.entries(values).map(([key,value])=>[key,{value,ts:1700000000000}]))};}
const text=(html)=>html.replace(/<!--[\s\S]*?-->/g,'').replace(/<[^>]*>/g,' ').replace(/\s+/g,' ');
const html=(values,live=true)=>render(Health,{props:{zone:zone(values),live}}).body;
test('missing metadata is explained once without implying a dead battery or bad radio',()=>{
 const value=html({});assert.match(text(value),/Informations non fournies par l’API locale/);assert.match(text(value),/Cela ne signifie pas que les piles sont vides/);assert.doesNotMatch(value,/<dl/);assert.match(value,/href="#\/airzone\/installation"/);assert.doesNotMatch(value,/<button|<input|<select/);
});
test('wired thermostat shows its documented model and reported firmware without empty radio readings',()=>{
 const value=html({thermos_radio:false,thermos_type:'2',thermos_firmware:'6.0'});assert.match(text(value),/Thermostat filaire/);assert.match(text(value),/Modèle Blueface Zero/);assert.match(text(value),/Version du thermostat 6.0/);assert.match(text(value),/Non applicable \(filaire\)/);assert.doesNotMatch(text(value),/Non communiqué|Niveau de batterie non communiqué|Type de liaison non communiqué/);
});
test('zero battery and coverage remain readings; malformed values never become zero',()=>{
 assert.match(text(html({thermos_radio:true,battery:0,coverage:0})),/Batterie 0 % Couverture radio 0 %/);
 for(const bad of [null,-1,101,NaN,'50',.5]){const value=text(html({thermos_radio:true,battery:bad,coverage:bad}));assert.match(value,/Niveau de batterie non communiqué/);assert.doesNotMatch(value,/\d+ %/);}
 assert.match(text(html({coverage:60})),/Couverture radio 60 %/);
});
test('historic metadata remains last-known; text is escaped and unknown model codes are not guessed',async()=>{
 for(const code of ['999','constructor','__proto__']){const value=html({thermos_type:code,thermos_firmware:'<script>bad</script>',battery:20},false);assert.match(text(value),/Dernières valeurs connues/);assert.doesNotMatch(value,/<script>|Blueface|Lite|Think|airzone_health.model\./);assert.match(value,/&lt;script/);}
 await words.setLanguage('en');const en=text(html({}));assert.match(en,/Information not supplied by the Local API/);assert.doesNotMatch(en,/airzone_health\./);
});
