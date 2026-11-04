import {test,before,after} from 'node:test';
import assert from 'node:assert/strict';
import {createServer} from 'vite';
import {fileURLToPath} from 'node:url';
let vite,airzoneSync,entityGeneration,gatewayFor;
before(async()=>{vite=await createServer({root:fileURLToPath(new URL('../../../',import.meta.url)),server:{middlewareMode:true,watch:null,hmr:false},logLevel:'error'});({airzoneSync,entityGeneration,gatewayFor}=await vite.ssrLoadModule('/src/maison/lib/airzone-live.js'));});
after(async()=>{await vite?.close();});
const entity=(values,extra={})=>({online:true,points:Object.entries(values).map(([key,value])=>({key,access:{read:true},kind:{type:typeof value==='number'?'numeric':typeof value==='boolean'?'binary':'text'}})),state:Object.fromEntries(Object.entries(values).map(([key,value])=>[key,{value,ts:100}])),...extra});
const gateway=entity({sync_poll_interval:30,sync_last_success:100000,sync_last_attempt:100000,sync_last_response:100000,sync_last_error:''});
test('unchanged values get freshness from their own entity receipt, not their last change or gateway heartbeat',()=>{
 const zone=entity({temperature:20,sync_last_received:99000,sync_generation:4});
 assert.equal(airzoneSync(zone,gateway,{now:100000}).state,'current');
 assert.equal(entityGeneration(zone),4);
 assert.equal(airzoneSync(entity({temperature:20,sync_last_received:1000}),gateway,{now:100000}).state,'stale');
 assert.equal(airzoneSync(entity({temperature:20}),gateway,{now:100000}).state,'unknown');
});
test('offline, disconnected, unknown availability and clock skew never look current',()=>{
 const zone=entity({sync_last_received:99000});
 assert.equal(airzoneSync({...zone,online:false},gateway,{now:100000}).state,'offline');
 assert.equal(airzoneSync(zone,gateway,{connected:false,now:100000}).state,'disconnected');
 assert.equal(airzoneSync({...zone,online:null},gateway,{now:100000}).state,'unknown');
 assert.equal(airzoneSync(entity({sync_last_received:110000}),gateway,{now:100000}).state,'unknown');
 assert.equal(airzoneSync(zone,null,{now:100000}).state,'unknown');
});
test('only an advertised receipt is usable and gateway matching stays within its instance',()=>{
 const zone=entity({sync_last_received:99000});zone.points=[];
 assert.equal(airzoneSync(zone,gateway,{now:100000}).received,null);
 assert.equal(gatewayFor([{instance:'other',model:'Airzone Webserver'},{instance:'native',model:'Airzone Webserver',id:'correct'}],'native').id,'correct');
});
test('optional resources use their own rotation interval instead of the faster HVAC gateway interval',()=>{
 const fast=entity({sync_poll_interval:5});
 assert.equal(airzoneSync(entity({sync_last_received:60000,sync_poll_interval:25}),fast,{now:100000}).state,'current');
 assert.equal(airzoneSync(entity({sync_last_received:60000,sync_poll_interval:5}),fast,{now:100000}).state,'stale');
});
