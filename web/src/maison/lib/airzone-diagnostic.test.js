import {test} from 'node:test';
import assert from 'node:assert/strict';
import {diagnosticRequest,summarizeZoneDiagnostic,ALERT_DIAGNOSTIC_READ} from './airzone-diagnostic.js';
const row=(systemID=1,zoneID=2)=>({systemID,zoneID});
const report=response=>({read_only:true,synthetic:false,response});
const address={system:1,zone:2};

test('flat and nested complete inventories prove presence or absence using explicit identities',()=>{
 for(const response of [{data:[row(),row(2,1)]},{systems:[{systemID:1,data:[row()]},{data:[row(2,1)]}]}]){
  assert.equal(summarizeZoneDiagnostic(report(response),address).state,'present');
  assert.equal(summarizeZoneDiagnostic(report(response),{system:1,zone:3}).state,'absent');
 }
 assert.equal(summarizeZoneDiagnostic(report({data:row()}),address).state,'present');
 assert.equal(summarizeZoneDiagnostic(report({systems:[]}),address).state,'absent');
});
test('unknown or malformed shapes never prove an absent device',()=>{
 for(const response of [null,{},[],{data:null},{data:[null]},{data:[{systemID:'1',zoneID:2}]},{data:[row(0,2)]},{data:[row(1,33)]},{data:[row(),row()]},{data:[row()],systems:[]},{systems:[{systemID:2,data:[row()]}]},{systems:[{data:[row(1,1),row(2,2)]}]},{data:[],error:'[redacted]'}])assert.equal(summarizeZoneDiagnostic(report(response),address).state,'unknown');
 for(const extra of [{read_only:false},{synthetic:true},{synthetic:null}])assert.equal(summarizeZoneDiagnostic({...report({data:[row()]}),...extra},address).gateway,false);
});
test('redaction limits allow positive presence but never inferred absence',()=>{
 const zones=Array.from({length:32},(_,i)=>row(1,i+1));
 for(const response of [{data:zones},{systems:[{data:zones}]},{systems:Array.from({length:32},(_,i)=>({data:[row(i+1,1)]}))}]){
  assert.notEqual(summarizeZoneDiagnostic(report(response),{system:2,zone:32}).state,'absent');
  assert.equal(summarizeZoneDiagnostic(report(response),{system:1,zone:1}).state,'present');
 }
 assert.equal(summarizeZoneDiagnostic(report({data:[...zones,row(2,1)]}),address).state,'unknown');
});
test('errors in a nested system envelope cannot establish a complete inventory',()=>{
 for(const failure of [{error:'[redacted]'},{errors:[{system:'C09'}]},{errors:'[redacted]'},{errors:{}}]){
  const response={systems:[{systemID:1,data:[],...failure}]};
  assert.equal(summarizeZoneDiagnostic(report(response),address).state,'unknown');
 }
 for(const errors of [null,[]]){
  const response={systems:[{systemID:1,data:[row(1,1)],errors}]};
  assert.equal(summarizeZoneDiagnostic(report(response),address).state,'absent');
 }
});
test('an alarm on a reported zone is distinct from a failed system envelope',()=>{
 const response={systems:[{systemID:1,data:[{...row(),errors:[{zone:'low_battery'}]}]}]};
 assert.equal(summarizeZoneDiagnostic(report(response),address).state,'present');
});
test('unknown native addresses report gateway connectivity without guessing a zone',()=>{
 for(const supplied of [{},{system:1,zone:null},{system:'1',zone:2},{system:0,zone:2}])assert.equal(summarizeZoneDiagnostic(report({data:[row()]}),supplied).state,'gateway');
});
test('the shared transport only calls the read-only diagnostic endpoint and rejects synthetic alert data',async()=>{
 const calls=[];const fetcher=async(url,options)=>{calls.push({url,...options});return {ok:true,status:200,json:async()=>report({data:[row()]})};};
 const result=await diagnosticRequest('synthetic',ALERT_DIAGNOSTIC_READ,{fetcher,realOnly:true});assert.equal(result.ok,true);
 assert.equal(calls.length,1);assert.equal(calls[0].url,'/api/integrations/airzone/synthetic/diagnostic');assert.deepEqual(JSON.parse(calls[0].body),{operation:'hvac',system:0,zone:0,get:false});assert.equal(calls[0].method,'POST');assert.ok(calls[0].signal);
 for(const payload of [{read_only:false,synthetic:false},{read_only:true,synthetic:true},{read_only:true,synthetic:false},{read_only:true},null]){
  const answer=await diagnosticRequest('synthetic',ALERT_DIAGNOSTIC_READ,{realOnly:true,fetcher:async()=>({ok:true,status:200,json:async()=>payload})});assert.equal(answer.error,'failed');
 }
});
test('HTTP failures and timeout expose only fixed safe errors',async(t)=>{
 for(const [status,error] of [[401,'auth'],[403,'auth'],[404,'missing'],[429,'busy'],[422,'invalid'],[500,'failed']]){
  const answer=await diagnosticRequest('synthetic',ALERT_DIAGNOSTIC_READ,{fetcher:async()=>({ok:false,status,json:async()=>({error:'private upstream text'})})});assert.deepEqual(answer,{ok:false,error});
 }
 const timeout=AbortSignal.abort(new DOMException('timed out','TimeoutError'));t.mock.method(AbortSignal,'timeout',()=>timeout);
 const answer=await diagnosticRequest('synthetic',ALERT_DIAGNOSTIC_READ,{fetcher:async(url,{signal})=>{signal.throwIfAborted();}});assert.equal(answer.error,'timeout');
});
