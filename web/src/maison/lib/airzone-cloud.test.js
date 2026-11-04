import {test} from 'node:test';
import assert from 'node:assert/strict';
import {createAirzoneCloudClient,cloudDevices,cloudReadings} from './airzone-cloud.js';
const response=(data,status=200)=>({ok:status===200,status,json:async()=>data});
test('cloud client sends only the declared UI request and ignores an old selection reply even if fetch ignores abort',async()=>{
 const calls=[];const client=createAirzoneCloudClient({fetcher:(url,options)=>new Promise(resolve=>calls.push({url,options,resolve}))});
 const first=client.request('read',{operation:'device_status',installation_id:'one',device_id:'a'}),second=client.request('read',{operation:'device_status',installation_id:'one',device_id:'b'});
 assert.equal(calls[0].options.signal.aborted,true);assert.equal(calls[1].options.headers['x-moli-origin'],'ui');assert.equal(calls[1].url,'/api/integrations/airzone/cloud/read');
 calls[1].resolve(response({read_only:true,received_at:2000,data:{power:false}}));assert.deepEqual((await second).data.data,{power:false});
 calls[0].resolve(response({read_only:true,received_at:1000,data:{power:true}}));assert.deepEqual(await first,{cancelled:true});
});
test('cloud responses fail closed for invalid readonly markers, auth expiry and malformed documents',async()=>{
 for(const [data,status,error]of [[{read_only:false,received_at:1,data:{}},200,'invalid_response'],[{read_only:true,data:{}},200,'invalid_response'],[{},403,'moli_auth'],[{error:'authentication_required'},403,'cloud_auth'],[{error:'forbidden'},403,'cloud_forbidden'],[{},429,'busy'],[{read_only:true,state:'fictional'},200,'invalid_response']]){
  const client=createAirzoneCloudClient({fetcher:async()=>response(data,status)});assert.equal((await client.request(data.state?'status':'read',{})).error,error);
 }
});
test('cloud status preserves unknown data, false and zero without guessed units or local mode mapping',()=>{
 assert.deepEqual(cloudReadings(null),[]);assert.deepEqual(cloudReadings({battery:null,local_temp:{celsius:null},humidity:101}),[]);
 const readings=cloudReadings({isConnected:false,power:false,local_temp:{fah:71},humidity:0,mode:3,speed_conf:0,errors:[]});
 assert.deepEqual(readings.find(item=>item.key==='temperature'),{key:'temperature',value:71,unit:'°F'});assert.deepEqual(readings.find(item=>item.key==='humidity'),{key:'humidity',value:0,unit:'%'});assert.equal(readings.find(item=>item.key==='power').value,false);assert.equal(readings.find(item=>item.key==='mode').value,3);assert.equal(readings.find(item=>item.key==='fan_speed').unit,null);
});
test('cloud installation deduplicates devices without admitting malformed identifiers',()=>{
 assert.deepEqual(cloudDevices({groups:[{name:'A',devices:[{device_id:'one',name:'Unit'}]},{name:'B',devices:[{device_id:'one'},{device_id:null},{device_id:'two'}]}]}).map(item=>item.device_id),['one','two']);
});
test('UI timeout exceeds the full backend inventory/refresh budget without waiting in the test',async(t)=>{
 let delay,expire;const timer=t.mock.method(globalThis,'setTimeout',(callback,ms)=>{delay=ms;expire=callback;return 1;});t.mock.method(globalThis,'clearTimeout',()=>{});
 const client=createAirzoneCloudClient({fetcher:(_url,options)=>new Promise((_resolve,reject)=>options.signal.addEventListener('abort',()=>reject(Error('abort'))))});
 const pending=client.request('read',{operation:'installations'});assert.ok(delay>30000);expire();assert.equal((await pending).error,'network');timer.mock.restore();
});
