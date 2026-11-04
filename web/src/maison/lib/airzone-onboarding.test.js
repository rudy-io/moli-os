import { test } from 'node:test';
import assert from 'node:assert/strict';
import * as onboarding from './airzone-onboarding.js';

const request = {instance:'airzone-2',host:'172.31.1.1',port:3000};
const preview = {instance:'airzone-2',mac:'aabbccddeeff',gateway_type:'ws_az',api_version:null,firmware:'4.21',systems:2,zones:12,incomplete:false,read_only:true};
const response = (data,status=200) => ({status,ok:status>=200&&status<300,json:async()=>data});

test('the form normalizes local connection data without exposing writable options', () => {
  assert.equal(typeof onboarding.gatewayRequest,'function');
  assert.deepEqual(onboarding.gatewayRequest({instance:' airzone-2 ',host:' 172.31.1.1 ',port:'3000',read_only:false,command:'on'}),{error:null,request});
  for(const host of ['10.1.2.3','172.16.1.2','172.31.255.254','192.168.0.1','172.31.1.2']) assert.equal(onboarding.gatewayRequest({...request,host}).error,null);
  const pin = 'AB:'.repeat(31)+'AB';
  assert.deepEqual(onboarding.gatewayRequest({...request,tls_fingerprint:pin}).request,{...request,tls_fingerprint:'ab'.repeat(32)});
});

test('bad hosts, reserved instances and malformed ports or certificates are refused before network access', async () => {
  assert.equal(typeof onboarding.gatewayRequest,'function');
  for(const host of ['controller.local','localhost','localhost.localdomain','127.0.0.1','127.0.0.2','127.255.255.255','::1','[::1]','::ffff:127.0.0.1','2130706433','0x7f000001','8.8.8.8','169.254.169.254','0.0.0.0','224.0.0.1','172.15.1.1','172.32.1.1','256.1.1.1','127.01.0.1','https://127.0.0.1','127.0.0.1/path','127.0.0.1\n','172.31.1.1\n']) {
    const expected = host === '172.31.1.1\n' ? null : 'invalid_host';
    assert.equal(onboarding.gatewayRequest({...request,host}).error,expected,host);
  }
  for(const instance of ['core','assistant','automations','energie','charges','lumieres','personnes','bad/id','-first','a'.repeat(33),'']) assert.equal(onboarding.gatewayRequest({...request,instance}).error,'invalid_instance');
  for(const port of [0,65536,'3e3','1.5','']) assert.equal(onboarding.gatewayRequest({...request,port}).error,'invalid_port');
  for(const tls_fingerprint of ['','ab','x'.repeat(64)]) assert.equal(onboarding.gatewayRequest({...request,tls_fingerprint}).error,'invalid_fingerprint');
  const noNetwork=()=>assert.fail('Invalid form must not reach the network');
  assert.equal((await onboarding.probeGateway({...request,host:'8.8.8.8'},noNetwork)).error,'invalid_host');
  for(const host of ['localhost','127.0.0.1','127.0.0.2','127.255.255.255']) {
    const input={...request,host};
    assert.equal((await onboarding.probeGateway(input,noNetwork)).error,'invalid_host');
    assert.equal((await onboarding.probeExistingGateway(input,noNetwork)).error,'invalid_host');
    assert.equal((await onboarding.addGateway(input,preview.mac,noNetwork)).error,'invalid_host');
    assert.equal((await onboarding.updateGateway(input,preview.mac,noNetwork)).error,'invalid_host');
  }
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

test('lifecycle lists stored gateways independently of runtime and rejects malformed metadata',async()=>{
  assert.equal(typeof onboarding.listGateways,'function');
  const gateway={instance:'airzone-2',host:'172.31.1.1',port:3000,tls_fingerprint:null,mac:preview.mac,enabled:false,source:'configuration',read_only:false,running:false};
  let sent;
  const result=await onboarding.listGateways(async(url,options)=>{sent={url,options};return response({gateways:[gateway]});});
  assert.deepEqual(result,{ok:true,data:{gateways:[gateway]}});
  assert.equal(sent.options.method,'GET');assert.equal(sent.options.body,undefined);
  assert.equal(sent.options.headers['x-moli-origin'],'ui');
  for(const changed of [{mac:123456789012},{enabled:'false'},{source:'unknown'},{read_only:null},{running:true},{tls_fingerprint:'bad'}]) {
    assert.equal((await onboarding.listGateways(async()=>response({gateways:[{...gateway,...changed}]}))).error,'response');
  }
  assert.equal((await onboarding.listGateways(async()=>response({gateways:[gateway,gateway]}))).error,'response');
});

test('update and resume bind an existing instance and identity; withdrawal is reversible configuration only',async()=>{
  assert.equal(typeof onboarding.updateGateway,'function');
  const calls=[];
  const transport=async(url,options)=>{calls.push({url,...options,body:JSON.parse(options.body)});return response(url.endsWith('/probe')?preview:url.endsWith('/withdraw')?{instance:request.instance,stored:true,enabled:false,read_only:false}:{instance:request.instance,stored:true,read_only:true,already_installed:false});};
  assert.equal((await onboarding.probeExistingGateway(request,transport)).ok,true);
  assert.equal((await onboarding.updateGateway(request,preview.mac,transport)).ok,true);
  assert.equal((await onboarding.withdrawGateway(request.instance,transport)).ok,true);
  assert.deepEqual(calls.map(c=>[c.method,c.url]),[['POST','/api/integrations/airzone/airzone-2/probe'],['PUT','/api/integrations/airzone/airzone-2'],['POST','/api/integrations/airzone/airzone-2/withdraw']]);
  assert.equal(calls[1].body.expected_mac,preview.mac);assert.deepEqual(calls[2].body,{});
  const count=calls.length;
  assert.equal((await onboarding.updateGateway(request,null,transport)).error,'expected_identity_required');
  assert.equal((await onboarding.withdrawGateway('../wrong',transport)).error,'invalid_instance');assert.equal(calls.length,count);
  assert.equal((await onboarding.withdrawGateway(request.instance,async()=>response({instance:request.instance,stored:true,enabled:true,read_only:true}))).error,'response');
});

test('legacy configured IDs are preserved exactly and encoded in lifecycle paths without relaxing new IDs',async()=>{
  for(const instance of ['Airzone étage.v1','  Loft  ','gateway?one#A','..inside']) {
    const input={...request,instance};
    const gateway={...input,tls_fingerprint:null,mac:preview.mac,enabled:true,source:'configuration',read_only:true,running:true};
    assert.equal((await onboarding.listGateways(async()=>response({gateways:[gateway]}))).ok,true);
    assert.equal(onboarding.gatewayRequest(input).error,'invalid_instance');
    const calls=[];
    const transport=async(url,options)=>{
      calls.push({url,body:JSON.parse(options.body)});
      return response(url.endsWith('/probe')?{...preview,instance}:url.endsWith('/withdraw')?{instance,stored:true,enabled:false,read_only:true}:{instance,stored:true,read_only:true,already_installed:false});
    };
    assert.equal((await onboarding.probeExistingGateway(input,transport)).ok,true);
    assert.equal((await onboarding.updateGateway(input,preview.mac,transport)).ok,true);
    assert.equal((await onboarding.withdrawGateway(instance,transport)).ok,true);
    const path='/api/integrations/airzone/'+encodeURIComponent(instance);
    assert.deepEqual(calls.map(call=>call.url),[path+'/probe',path,path+'/withdraw']);
    assert.equal(calls[0].body.instance,instance);assert.equal(calls[1].body.instance,instance);
  }
});

test('dot-only legacy IDs remain visible but never become normalized browser paths',async()=>{
  for(const instance of ['.','..']) {
    const input={...request,instance};
    const gateway={...input,tls_fingerprint:null,mac:null,enabled:false,source:'configuration',read_only:true,running:false};
    assert.equal((await onboarding.listGateways(async()=>response({gateways:[gateway]}))).ok,true);
    const noNetwork=()=>assert.fail('dot segments must never be sent to a different route');
    assert.equal((await onboarding.probeExistingGateway(input,noNetwork)).error,'unaddressable_instance');
    assert.equal((await onboarding.updateGateway(input,preview.mac,noNetwork)).error,'unaddressable_instance');
    assert.equal((await onboarding.withdrawGateway(instance,noNetwork)).error,'unaddressable_instance');
  }
});
