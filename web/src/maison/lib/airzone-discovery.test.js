import { test } from 'node:test';
import assert from 'node:assert/strict';
import * as onboarding from './airzone-onboarding.js';

const gateway={host:'192.168.1.20',port:3000,tls_fingerprint:null,mac:'aabbccddeeff',gateway_type:'ws_az',api_version:null,firmware:'4.21',systems:2,zones:12,incomplete:false,read_only:true,existing_instance:null,existing_enabled:null};
const report={gateways:[gateway],scanned:254,target_count:254,incomplete:false};
const response=(data,status=200)=>({ok:status===200,status,json:async()=>data});
const discover=(...args)=>{assert.equal(typeof onboarding.discoverGateways,'function','the guided flow needs a bounded read-only discovery request');return onboarding.discoverGateways(...args);};

test('discovery makes one authenticated UI request with no arbitrary targets',async()=>{
 const calls=[];const controller=new AbortController();
 const result=await discover({signal:controller.signal},async(...args)=>{calls.push(args);return response(report);});
 assert.equal(result.ok,true);assert.equal(calls.length,1);assert.equal(calls[0][0],'/api/integrations/airzone/discover');
 const options=calls[0][1];assert.equal(options.method,'POST');assert.equal(options.headers['x-moli-origin'],'ui');assert.deepEqual(JSON.parse(options.body),{});assert.equal(options.signal,controller.signal);
});

test('discovery accepts empty and explicitly incomplete results',async()=>{
 for(const data of [{...report,gateways:[]},{gateways:[],scanned:0,target_count:0,incomplete:true},{...report,incomplete:true,scanned:20}])assert.equal((await discover({},async()=>response(data))).ok,true);
});

test('discovery refuses malformed identities, unsafe endpoints, writes and duplicate controllers',async()=>{
 const bad=[{...gateway,host:'8.8.8.8'},{...gateway,host:'127.0.0.1'},{...gateway,host:'192.168.1.0'},{...gateway,host:'192.168.1.255'},{...gateway,port:0},{...gateway,read_only:false},{...gateway,mac:'not-a-mac'},{...gateway,zones:-1},{...gateway,gateway_type:'other'},{...gateway,existing_instance:'known',existing_enabled:null},{...gateway,existing_instance:null,existing_enabled:true},{...gateway,tls_fingerprint:'invalid'}];
 for(const item of bad)assert.equal((await discover({},async()=>response({...report,gateways:[item]}))).error,'response');
 assert.equal((await discover({},async()=>response({...report,gateways:[gateway,gateway]}))).error,'response');
 for(const invalid of [{...report,scanned:0},{...report,scanned:255},{...report,target_count:513},{...report,scanned:-1},{...report,incomplete:'yes'}])assert.equal((await discover({},async()=>response(invalid))).error,'response');
});

test('discovery reports cancellation, authentication and rate limiting without retrying',async()=>{
 let calls=0;
 for(const status of [403,429]){const result=await discover({},async()=>{calls++;return response({error:'busy'},status);});assert.equal(result.error,status===403?'auth':'busy');}
 assert.equal(calls,2);
 const controller=new AbortController();controller.abort();
 const result=await discover({signal:controller.signal},async()=>{throw new DOMException('aborted','AbortError');});assert.equal(result.error,'cancelled');
});
