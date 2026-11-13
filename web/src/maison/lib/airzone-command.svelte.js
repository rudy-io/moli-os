import {hub,observeAirzone,reconcileAirzoneDevice} from '../../lib/hub.svelte.js';
import {home} from './home.svelte.js';
import {deviceName,formatValue} from '../../lib/format.js';
import {readingOf} from './airzone.js';
import {airzoneSync,gatewayFor,entityGeneration} from './airzone-live.js';
import {validIntention} from './airzone-controls.js';
import {createAirzoneCommandQueue} from './airzone-command-queue.js';

export const airzoneCommands=$state({});
const pendingApprovals=$state({});
export const hasPendingAirzone=(point)=>Boolean(pendingApprovals[point]);
const headers={'content-type':'application/json','x-moli-origin':'ui'};
function target(point){
  const split=point.lastIndexOf('/'),id=point.slice(0,split),key=point.slice(split+1);
  const device=hub.devices[id];
  const native=hub.drivers.some(driver=>driver.kind==='airzone'&&driver.instance===device?.instance);
  return {device:native?device:null,key,id};
}
function read(point){
  const {device,key}=target(point);
  return {writable:device?.points?.some(spec=>spec.key===key&&spec.access?.write===true),
    online:airzoneSync(device,gatewayFor(hub.devices,device?.instance),{connected:hub.connected}).state==='current',
    value:key==='target_temperature_on'?(readingOf(device,'on')?.value===true?readingOf(device,'target_temperature')?.value:null):readingOf(device,key)?.value,generation:entityGeneration(device)};
}
async function request(url,options={}){
  return fetch(url,{...options,signal:AbortSignal.timeout(20000),cache:'no-store'});
}
async function readBarrier(point){
  const {id}=target(point);
  const response=await request(`/api/devices/${encodeURIComponent(id)}`);
  if(!response.ok)return {ok:false,error:'unconfirmed'};
  const device=await response.json();
  if(device.id!==id)return {ok:false,error:'unconfirmed'};
  const barrier=readingOf(device,'sync_last_command_generation')?.value;
  reconcileAirzoneDevice(device);
  return {ok:true,barrier};
}
function statusError(status){return status===403?'read_only':status===404?'missing':status===409||status===422?'invalid':status===503?'unavailable':status===504?'unconfirmed':'failed';}
const labelFor=(point,value)=>{const {device,key}=target(point);const spec=device?.points?.find(item=>item.key===key);return `${deviceName(device ?? {})} · ${spec?.label ?? key} : ${spec?formatValue(spec,value):value}${spec?.unit?` ${spec.unit}`:''}`;};
async function denyError(point,id){
  try{
    const response=await request(`/api/approvals/${id}`,{method:'POST',headers,body:JSON.stringify({approve:false,point})});
    if(response.ok||response.status===410)delete pendingApprovals[point];
    return response.ok?'cancelled':response.status===410?'approval_resolved':'approval_pending';
  }catch{return 'approval_pending';}
}
async function approve(point,value,body){
  pendingApprovals[point]={id:body.pending,value};
  if(home.held)return {ok:false,error:await denyError(point,body.pending)};
  airzoneCommands[point]={value,phase:'authorizing',error:null};
  return new Promise(resolve=>{
    home.held={id:body.pending,point,reason:body.reason,label:labelFor(point,value),
      // Custom continuation keeps the ACK/readback barrier even through the PIN
      // guard, rather than falling back to the generic optimistic control path.
      custom:async()=>{
        if(!Object.is(airzoneCommands[point]?.value,value)||!read(point).writable||!read(point).online){const error=await denyError(point,body.pending);airzoneCommands[point]={value,phase:'error',error};resolve({ok:false,error});return;}
        try{
          const response=await request(`/api/approvals/${body.pending}`,{method:'POST',headers,body:JSON.stringify({approve:true,point})});
          if(response.ok||response.status===410)delete pendingApprovals[point];
          resolve(response.ok?await readBarrier(point):{ok:false,error:response.status===403||response.status===409?'approval_pending':response.status===410?'cancelled':statusError(response.status)});
        }catch{resolve({ok:false,error:'unconfirmed'});}
      },
      onCancel:async()=>{const error=await denyError(point,body.pending);airzoneCommands[point]={value,phase:'error',error};resolve({ok:false,error});},
    };
  });
}
async function send(point,value){
  const {device,key}=target(point),spec=device?.points?.find(item=>item.key===key);
  if(!validIntention(device,spec,value))return {ok:false,error:'invalid'};
  const response=await request('/api/command',{method:'POST',headers,body:JSON.stringify({point,value})});
  if(response.status===202){const body=await response.json();return Number.isSafeInteger(body.pending)?approve(point,value,body):{ok:false,error:'failed'};}
  if(!response.ok)return {ok:false,error:statusError(response.status)};
  return readBarrier(point);
}
const queue=createAirzoneCommandQueue({send,read,changed:(point,state)=>{if(state)airzoneCommands[point]=state;else delete airzoneCommands[point];}});
observeAirzone(()=>queue.observe());
export function requestAirzone(point,value){
  const {device,key}=target(point),spec=device?.points?.find(item=>item.key===key);
  if(home.held||hasPendingAirzone(point)||!read(point).online||!validIntention(device,spec,value))return false;
  return queue.request(point,value,{coalesce:spec.kind.type==='numeric'});
}

/** A cancelled code sheet can leave a server-held order when the session has
 * expired. Offer a fresh, explicit denial instead of pretending it vanished. */
export function denyPendingAirzone(point){
  const pending=pendingApprovals[point];
  if(!pending||home.held)return false;
  home.held={label:labelFor(point,pending.value),reason:'airzone_deny',custom:async()=>{
    airzoneCommands[point]={value:pending.value,phase:'error',error:await denyError(point,pending.id)};
  }};
  return true;
}
