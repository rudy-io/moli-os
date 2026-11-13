import {test,before,beforeEach,after} from 'node:test';
import assert from 'node:assert/strict';
import {createServer} from 'vite';
import {fileURLToPath} from 'node:url';
let vite,render,Controls,Sync,Diagnostic,hub,home,words,commands,contract;
const originals=new Map(['document','localStorage'].map(key=>[key,Object.getOwnPropertyDescriptor(globalThis,key)]));
before(async()=>{
 delete globalThis.localStorage;globalThis.document={querySelector:()=>null,documentElement:{}};
 vite=await createServer({root:fileURLToPath(new URL('../../../',import.meta.url)),server:{middlewareMode:true,watch:null,hmr:false},logLevel:'error'});
 ({render}=await vite.ssrLoadModule('svelte/server'));
 Controls=(await vite.ssrLoadModule('/src/maison/ui/AirzoneControls.svelte')).default;Sync=(await vite.ssrLoadModule('/src/maison/ui/AirzoneSyncStatus.svelte')).default;Diagnostic=(await vite.ssrLoadModule('/src/maison/ui/AirzoneDiagnostic.svelte')).default;
 ({hub,home}=await vite.ssrLoadModule('/src/maison/lib/home.svelte.js'));words=await vite.ssrLoadModule('/src/lib/i18n.svelte.js');commands=(await vite.ssrLoadModule('/src/maison/lib/airzone-command.svelte.js')).airzoneCommands;contract=await vite.ssrLoadModule('/src/maison/lib/airzone-controls.js');
});
beforeEach(async()=>{await words.setLanguage('fr');hub.drivers=[{instance:'synthetic',kind:'airzone'}];home.held=null;for(const key of Object.keys(commands))delete commands[key];});
after(async()=>{await vite?.close();for(const[key,value]of originals){if(value)Object.defineProperty(globalThis,key,value);else delete globalThis[key];}});
function zone(){
 const points=[{key:'on',label:'Marche',kind:{type:'binary'}},{key:'target_temperature',label:'Consigne',unit:'°C',kind:{type:'numeric',min:15,max:30,step:0.5}},{key:'mode',label:'Mode',kind:{type:'enum',values:['heat','cool']}},{key:'fan_speed',label:'Ventilation',kind:{type:'enum',values:['0','2']}},{key:'device_name',label:'Nom Airzone',kind:{type:'text'}}].map(point=>({...point,access:{read:true,write:true}}));
 return {id:'synthetic:zone',instance:'synthetic',native_name:'Synthetic zone',model:'Local API',online:true,points,state:{on:{value:false,ts:1},target_temperature:{value:20,ts:1},mode:{value:'heat',ts:1},fan_speed:{value:'2',ts:1},device_name:{value:'Zone',ts:1}}};
}
const htmlOf=(device,props={})=>render(Controls,{props:{device,sync:{state:'current'},...props}}).body;
const text=html=>html.replace(/<!--.*?-->/g,'').replace(/<[^>]*>/g,' ').replace(/\s+/g,' ').trim();
const editors=html=>[...html.matchAll(/<(?:input|select|button)\b[^>]*>/g)].map(([tag])=>tag);
test('main controls follow exact numeric metadata and enum choices while advanced text stays separate',()=>{
 const device=zone(),html=htmlOf(device);
 assert.match(html,/type="number"[^>]+min="15"[^>]+max="30"[^>]+step="0.5"/);assert.match(html,/role="switch" aria-checked="false"/);assert.doesNotMatch(html,/Nom Airzone/);assert.ok(editors(html).every(tag=>!/\bdisabled\b/.test(tag)));
 assert.match(text(htmlOf(device,{variant:'advanced'})),/Nom Airzone/);assert.doesNotMatch(text(htmlOf(device,{variant:'advanced'})),/Consigne/);
 delete device.points.find(point=>point.key==='target_temperature').kind.step;
 assert.doesNotMatch(htmlOf(device),/type="number"/);assert.match(text(htmlOf(device)),/limites ou choix non communiqués/);
});
test('read-only, stale and disconnected controls show values without editors and cannot imply a missing zero',()=>{
 const device=zone();device.points.forEach(point=>point.access.write=false);
 assert.equal(editors(htmlOf(device)).length,0);assert.match(text(htmlOf(device)),/Lecture seule/);assert.match(text(htmlOf(device)),/20 °C/);
 const unknown=zone();delete unknown.state.on;delete unknown.state.target_temperature;
 const html=htmlOf(unknown);assert.match(text(html),/Allumer \(état inconnu\)/);assert.doesNotMatch(html,/aria-checked="false"/);assert.match(html,/type="number"[^>]+value=""/);
 for(const state of ['stale','offline','unknown','disconnected']){const html=htmlOf(zone(),{sync:{state}});assert.equal(editors(html).length,0);assert.match(text(html),/20 °C/);}
});
test('intention remains separate from the reported value and timeout is not successful state',()=>{
 const device=zone(),point=device.id+'/target_temperature';commands[point]={value:23,phase:'awaiting'};
 let html=htmlOf(device);assert.match(text(html),/Lu sur l’appareil : 20 °C/);assert.match(text(html),/Envoyé : 23 °C.*en attente/);assert.match(html,/type="number"[^>]+value="23"/);assert.doesNotMatch(text(html),/Confirmé par/);
 commands[point]={value:23,phase:'error',error:'unconfirmed'};html=htmlOf(device);assert.match(html,/type="number"[^>]+value="20"/);assert.match(text(html),/Résultat non confirmé/);
});
test('shared-system scope is explicit and compound action reuses the target instead of a second slider',()=>{
 const device=zone();device.model='Airzone system';device.members=['synthetic:a','synthetic:b'];
 assert.match(text(htmlOf(device,{members:[{native_name:'Zone A'},{native_name:'Zone B'}]})),/zones associées.*Zone A, Zone B/);
 device.model='Local API';device.points.push({...device.points.find(point=>point.key==='target_temperature'),key:'target_temperature_on',access:{read:false,write:true}});
 const html=htmlOf(device);assert.match(text(html),/Allumer à 20 °C/);assert.equal([...html.matchAll(/type="number"/g)].length,1);assert.doesNotMatch(text(htmlOf(device,{variant:'advanced'})),/target_temperature_on/);
});
test('validators reject non-finite, out-of-range, off-grid, guessed enum and missing contracts',()=>{
 const device=zone(),point=device.points.find(point=>point.key==='target_temperature');
 for(const value of [null,NaN,Infinity,14,31,20.1,'21'])assert.equal(contract.validIntention(device,point,value),false);
 assert.equal(contract.validIntention(device,point,20.5),true);assert.equal(contract.validIntention(device,device.points.find(point=>point.key==='mode'),'auto'),false);
});
test('English diagnostics keep timestamps, error and receipt distinct and escape technical data',async()=>{
 await words.setLanguage('en');
 const html=render(Sync,{props:{sync:{state:'offline',received:1000,attempted:2000,responded:1500,succeeded:1200,browserReceivedAt:2100,interval:30,error:'synthetic error'},now:3000,detailed:true}}).body;
 assert.match(text(html),/Last attempt/);assert.match(text(html),/Last gateway response/);assert.match(text(html),/Last receipt for this device/);assert.match(text(html),/synthetic error/);assert.doesNotMatch(text(html),/airzone_live\./);
 const diagnostic=render(Diagnostic,{props:{instance:'synthetic'}}).body;assert.match(text(diagnostic),/On-demand reads without controlling devices/);assert.match(text(diagnostic),/Demonstration data/);assert.doesNotMatch(text(diagnostic),/airzone_diagnostic\./);
});

test('cloud account opens as a distinct read-only surface without exposing a password form before PIN access',async()=>{
 const Cloud=(await vite.ssrLoadModule('/src/maison/ui/AirzoneCloud.svelte')).default;
 for(const language of ['fr','en']){await words.setLanguage(language);const html=render(Cloud).body;assert.doesNotMatch(html,/type="password"|type="email"|airzone_cloud\./);assert.match(text(html),/Airzone Cloud/);assert.match(text(html),language==='fr'?/Lecture seule/:/Read-only/);}
});
test('known mode labels are translated without changing API values, including read-only and pending states',async()=>{
 const device=zone(),point=device.points.find(point=>point.key==='mode');
 point.kind.values=['off','heat','cool','auto','dry','fan','future_mode'];
 for(const [language,labels]of [['fr',['Éteinte','Chaud','Froid','Auto','Déshumidifier','Ventilation','future_mode']],['en',['Off','Heat','Cool','Auto','Dry','Fan','future_mode']]]){
  await words.setLanguage(language);
  for(const write of [true,false]){
   point.access.write=write;
   const html=htmlOf(device),select=html.match(/<select id="airzone-control-synthetic%3Azone-mode"[\s\S]*?<\/select>/)?.[0];
   if(!write){assert.equal(select,undefined);assert.match(text(html),new RegExp(labels[1]));continue;}
   assert.ok(select);
   const options=[...select.matchAll(/<option value="([^"]+)"[^>]*>([^<]*)<\/option>/g)].map(([,value,label])=>({value,label}));
   assert.deepEqual(options,point.kind.values.map((value,i)=>({value,label:labels[i]})));
   assert.equal(/\bdisabled\b/.test(select.match(/^<select[^>]*>/)[0]),!write);
  }
  commands[device.id+'/mode']={value:'cool',phase:'awaiting'};
  const pending=text(htmlOf(device));
  assert.match(pending,language==='fr'?/Lu sur l’appareil : Chaud/:/Read from device: Heat/);
  assert.match(pending,language==='fr'?/Envoyé : Froid/:/Sent: Cool/);
  delete commands[device.id+'/mode'];
 }
});
test('a fresh receipt ahead of the UI tick reads as now without hiding genuine clock skew',async()=>{
 const {airzoneSync}=await vite.ssrLoadModule('/src/maison/lib/airzone-live.js');
 const now=100000;
 const syncAt=received=>{
  const device={online:true,points:['sync_last_received','sync_poll_interval'].map(key=>({key,kind:{type:'numeric'},access:{read:true}})),state:{sync_last_received:{value:received,ts:received},sync_poll_interval:{value:30,ts:received}}};
  return airzoneSync(device,device,{now});
 };
 for(const language of ['fr','en']){
  await words.setLanguage(language);
  const recent=render(Sync,{props:{sync:syncAt(now+900),now}}).body;
  assert.match(text(recent),language==='fr'?/Reçu maintenant/:/Received now/);
  assert.doesNotMatch(text(recent),language==='fr'?/dans 1 seconde/:/in 1 second/);
  const skewed=render(Sync,{props:{sync:syncAt(now+6000),now}}).body;
  assert.match(skewed,/data-sync-state="unknown"/);
  assert.match(text(skewed),language==='fr'?/Reçu dans 6 secondes/:/Received in 6 seconds/);
  const previous=render(Sync,{props:{sync:syncAt(now-15000),now}}).body;
  assert.match(text(previous),language==='fr'?/il y a 15 secondes/:/15 seconds ago/);
 }
});

test('a zone presents one contextual target and keeps alternate setpoints in advanced settings',()=>{
 const device=zone(),target=device.points.find(point=>point.key==='target_temperature');
 device.points.push({...target,key:'heatsetpoint',label:'Heating target'},{...target,key:'coolsetpoint',label:'Cooling target'},{key:'double_sp',kind:{type:'binary'},access:{read:true}});
 device.state.heatsetpoint={value:19};device.state.coolsetpoint={value:25};device.state.double_sp={value:false};
 assert.deepEqual(contract.primarySetpointKeys(device),['target_temperature']);
 const generic=device.state.target_temperature;delete device.state.target_temperature;
 assert.deepEqual(contract.primarySetpointKeys(device),['heatsetpoint']);
 device.state.mode.value='auto';assert.deepEqual(contract.primarySetpointKeys(device),['heatsetpoint','coolsetpoint']);
 device.state.mode.value='cool';device.state.coolsetpoint={value:null};assert.deepEqual(contract.primarySetpointKeys(device),['target_temperature'],'an unknown cooling target cannot fall back to heating');
 device.state.coolsetpoint={value:25};device.state.target_temperature=generic;device.state.mode.value='heat';
 let html=htmlOf(device);assert.equal((html.match(/type="number"/g)||[]).length,1);assert.doesNotMatch(html,/data-point="(?:heatsetpoint|coolsetpoint|fan_speed)"/);
 const advanced=htmlOf(device,{variant:'advanced'});for(const key of ['heatsetpoint','coolsetpoint','fan_speed'])assert.match(advanced,new RegExp(`data-point="${key}"`));
 device.state.double_sp.value=true;assert.deepEqual(contract.primarySetpointKeys(device),['heatsetpoint','coolsetpoint']);assert.equal((htmlOf(device).match(/type="number"/g)||[]).length,2);
 device.state.double_sp.value=false;device.points=device.points.filter(point=>point.key!=='target_temperature');
 assert.deepEqual(contract.primarySetpointKeys(device),['heatsetpoint']);device.state.mode.value='cool';assert.deepEqual(contract.primarySetpointKeys(device),['coolsetpoint']);
 device.state.mode.value='auto';assert.deepEqual(contract.primarySetpointKeys(device),['heatsetpoint','coolsetpoint']);
 delete device.state.mode;assert.deepEqual(contract.primarySetpointKeys(device),['heatsetpoint','coolsetpoint']);
});

test('floating point sensor noise is cleaned only for display and never snapped to the control grid',()=>{
 const device=zone(),point=device.points.find(point=>point.key==='target_temperature');
 const raw=22.299999237060547;device.state.target_temperature.value=raw;
 const before=structuredClone(device);
 assert.equal(contract.numericEditorValue(raw),'22.3');assert.equal(contract.numericEditorValue(68.25),'68.25');
 assert.equal(contract.numericEditorValue(22.345678),'22.345678');assert.equal(contract.numericEditorValue(null),'');assert.equal(contract.numericEditorValue(Infinity),'');
 const html=htmlOf(device);assert.match(html,/type="number"[^>]+value="22.3"/);assert.doesNotMatch(html,/22\.299999/);
 assert.deepEqual(device,before);assert.equal(contract.validIntention(device,point,22.3),false,'display does not make an off-grid reading an accepted intention');
 commands[device.id+'/target_temperature']={value:23.5,phase:'awaiting'};
 assert.match(htmlOf(device),/type="number"[^>]+value="23.5"/);assert.equal(commands[device.id+'/target_temperature'].value,23.5);
});

test('read-only values stay readable and an alternate setpoint error remains visible',()=>{
 const device=zone(),target=device.points.find(point=>point.key==='target_temperature');
 device.points.push({...target,key:'heatsetpoint',label:'Heating target'});device.state.heatsetpoint={value:19};
 device.points.forEach(point=>point.access.write=false);
 let html=htmlOf(device);assert.equal(editors(html).length,0);assert.match(text(html),/20 °C/);assert.match(text(html),/Chaud/);
 commands[device.id+'/heatsetpoint']={value:21,phase:'error',error:'approval_pending',pendingId:12};
 html=htmlOf(device);assert.match(text(html),/Heating target/);assert.match(text(html),/Une demande attend encore/);
 assert.doesNotMatch(html,/<input|<select|role="switch"/);
});

test('read-only text mode uses known translated labels without inventing enum choices',async()=>{
 const device=zone(),mode=device.points.find(p=>p.key==='mode');mode.kind={type:'text'};device.points.forEach(p=>p.access.write=false);
 for(const [language,label] of [['fr','Chaud'],['en','Heat']]){await words.setLanguage(language);const html=htmlOf(device);assert.match(text(html),new RegExp(label));assert.doesNotMatch(html,/<input|<select|role="switch"/);assert.equal(contract.controlContract(device,mode).type,'text');}
 device.state.mode.value='future_mode';assert.match(text(htmlOf(device)),/future_mode/);
});
