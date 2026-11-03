import { test } from 'node:test';
import assert from 'node:assert/strict';
import * as onboarding from './airzone-onboarding.js';

const request = {instance:'airzone-2',host:'127.0.0.1',port:3000};
const preview = {instance:'airzone-2',mac:'aabbccddeeff',gateway_type:'ws_az',api_version:null,firmware:'4.21',systems:2,zones:12,incomplete:false,read_only:true};
const response = (data,status=200) => ({status,ok:status>=200&&status<300,json:async()=>data});

test('the form normalizes local connection data without exposing writable options', () => {
  assert.equal(typeof onboarding.gatewayRequest,'function');
  assert.deepEqual(onboarding.gatewayRequest({instance:' airzone-2 ',host:' 127.0.0.1 ',port:'3000',read_only:false,command:'on'}),{error:null,request});
  for(const host of ['10.1.2.3','172.16.1.2','172.31.255.254','192.168.0.1','127.0.0.2']) assert.equal(onboarding.gatewayRequest({...request,host}).error,null);
  const pin = 'AB:'.repeat(31)+'AB';
  assert.deepEqual(onboarding.gatewayRequest({...request,tls_fingerprint:pin}).request,{...request,tls_fingerprint:'ab'.repeat(32)});
});

test('bad hosts, reserved instances and malformed ports or certificates are refused before network access', async () => {
  assert.equal(typeof onboarding.gatewayRequest,'function');
  for(const host of ['controller.local','8.8.8.8','169.254.169.254','0.0.0.0','224.0.0.1','172.15.1.1','172.32.1.1','256.1.1.1','127.01.0.1','https://127.0.0.1','127.0.0.1/path','127.0.0.1\n']) {
    const expected = host === '127.0.0.1\n' ? null : 'invalid_host';
    assert.equal(onboarding.gatewayRequest({...request,host}).error,expected,host);
  }
  for(const instance of ['core','assistant','automations','energie','charges','lumieres','personnes','bad/id','-first','a'.repeat(33),'']) assert.equal(onboarding.gatewayRequest({...request,instance}).error,'invalid_instance');
  for(const port of [0,65536,'3e3','1.5','']) assert.equal(onboarding.gatewayRequest({...request,port}).error,'invalid_port');
  for(const tls_fingerprint of ['','ab','x'.repeat(64)]) assert.equal(onboarding.gatewayRequest({...request,tls_fingerprint}).error,'invalid_fingerprint');
  const noNetwork=()=>assert.fail('Invalid form must not reach the network');
  assert.equal((await onboarding.probeGateway({...request,host:'8.8.8.8'},noNetwork)).error,'invalid_host');
  assert.equal((await onboarding.addGateway(request,'',noNetwork)).error,'expected_identity_required');
});

test('suggestions reserve every known driver identity without changing existing instances', () => {
  assert.equal(typeof onboarding.nextGatewayInstance,'function');
  assert.equal(onboarding.nextGatewayInstance([]),'airzone-2');
  assert.equal(onboarding.nextGatewayInstance([{instance:'airzone-2'},{instance:'airzone-3'},{instance:'airzone'}]),'airzone-4');
});

test('verification and installation use separate bounded endpoints and bind the checked identity', async () => {
  assert.equal(typeof onboarding.probeGateway,'function');
  const calls=[];
  const fetcher=async(url,options)=>{calls.push({url,...options});return response(url.endsWith('/probe')?preview:{instance:'airzone-2',stored:true,read_only:true,already_installed:false});};
  assert.deepEqual(await onboarding.probeGateway(request,fetcher),{ok:true,data:preview});
  assert.deepEqual(await onboarding.addGateway(request,preview.mac,fetcher),{ok:true,data:{instance:'airzone-2',stored:true,read_only:true,already_installed:false}});
  assert.deepEqual(calls,[
    {url:'/api/integrations/airzone/probe',method:'POST',headers:{'content-type':'application/json','x-moli-origin':'ui'},body:JSON.stringify(request)},
    {url:'/api/integrations/airzone',method:'POST',headers:{'content-type':'application/json','x-moli-origin':'ui'},body:JSON.stringify({...request,expected_mac:'aabbccddeeff'})},
  ]);
});

test('a response for another instance or claiming writable/unconfirmed storage cannot succeed', async () => {
  assert.equal(typeof onboarding.probeGateway,'function');
  for(const data of [{...preview,read_only:false},{...preview,instance:'other'},{...preview,mac:'bad'}, {...preview,mac:112233445566}, {...preview,zones:-1},{...preview,incomplete:'false'},{pending:1},null]) {
    assert.equal((await onboarding.probeGateway(request,async()=>response(data))).error,'response');
  }
  for(const data of [{instance:'airzone-2',stored:false,read_only:true,already_installed:false},{instance:'other',stored:true,read_only:true,already_installed:false},{instance:'airzone-2',stored:true,read_only:false,already_installed:false},{instance:'airzone-2',stored:true,read_only:true}]) {
    assert.equal((await onboarding.addGateway(request,preview.mac,async()=>response(data))).error,'response');
  }
  assert.equal((await onboarding.probeGateway(request,async()=>response(preview,202))).error,'response');
});

test('auth, known conflicts and uncertain network outcomes stay distinct and never auto-retry', async () => {
  assert.equal(typeof onboarding.probeGateway,'function');
  let calls=0;
  assert.deepEqual(await onboarding.probeGateway(request,async()=>{calls++;return response({error:'private response'},403);}),{ok:false,error:'auth',status:403});
  assert.equal(calls,1);
  assert.deepEqual(await onboarding.probeGateway(request,async()=>response({error:'duplicate_gateway',existing_instance:'airzone-original'},409)),{ok:false,error:'duplicate_gateway',status:409,existingInstance:'airzone-original'});
  assert.deepEqual(await onboarding.probeGateway(request,async()=>response({error:'private upstream data',existing_instance:'unsafe/<id>'},502)),{ok:false,error:'server',status:502});
  assert.deepEqual(await onboarding.probeGateway(request,async()=>{throw new Error('private network data');}),{ok:false,error:'network'});
  assert.equal((await onboarding.addGateway(request,preview.mac,async()=>response({instance:'airzone-2',stored:true,read_only:true,already_installed:true}))).data.already_installed,true);
});
