/** Shared transport for explicit diagnostic reads. No device command or state publication. */
export const ALERT_DIAGNOSTIC_READ=Object.freeze({operation:'hvac',system:0,zone:0,get:false});
const statusError=status=>status===401||status===403?'auth':status===429?'busy':status===404?'missing':status===400||status===422?'invalid':'failed';
export async function diagnosticRequest(instance,body,{signal,realOnly=false,fetcher=globalThis.fetch}={}) {
  const timeout=AbortSignal.timeout(20000);
  const combined=signal?AbortSignal.any([signal,timeout]):timeout;
  try {
    combined.throwIfAborted();
    const response=await fetcher(`/api/integrations/airzone/${encodeURIComponent(instance)}/diagnostic`,{
      method:'POST',headers:{'content-type':'application/json','x-moli-origin':'ui'},body:JSON.stringify(body),signal:combined,
    });
    if(!response.ok)return {ok:false,error:statusError(response.status)};
    const data=await response.json().catch(()=>null);
    combined.throwIfAborted();
    if(data?.read_only!==true||(realOnly&&(data.synthetic!==false||!Object.hasOwn(data,'response'))))return {ok:false,error:'failed'};
    return {ok:true,data};
  } catch {
    return {ok:false,error:timeout.aborted?'timeout':signal?.aborted?'cancelled':'network'};
  }
}

const object=value=>value!==null&&typeof value==='object'&&!Array.isArray(value);
const address=value=>Number.isInteger(value)&&value>=1&&value<=32;
const failedEnvelope=value=>Object.hasOwn(value,'error')||(value.errors!=null&&(!Array.isArray(value.errors)||value.errors.length>0));
/** A successful read is not proof that an alarm has cleared. An absent zone is
 * only meaningful in a valid complete inventory. The server caps arrays at 32
 * without a truncation marker, so a full-sized array cannot prove absence. */
export function summarizeZoneDiagnostic(report,{system,zone}={}) {
  if(report?.read_only!==true||report.synthetic!==false)return {gateway:false,state:'unknown'};
  const unknown={gateway:true,state:'unknown'};
  if(!address(system)||!address(zone))return {gateway:true,state:'gateway'};
  const source=report.response;
  if(!object(source)||failedEnvelope(source))return unknown;
  const nested=Object.hasOwn(source,'systems');
  if(nested&&(Object.hasOwn(source,'data')||!Array.isArray(source.systems)||source.systems.length>32))return unknown;
  const groups=nested?source.systems:[source];
  let complete=!nested||groups.length<32;
  const identities=new Set(),systems=new Set();
  for(const group of groups){
    // A failed system read can still contain an empty/partial data array.
    // Zone-level alarms remain valid observations and do not enter this check.
    if(!object(group)||failedEnvelope(group))return unknown;
    const data=group.data;
    if(!Array.isArray(data)&&!object(data))return unknown;
    if(Array.isArray(data)&&data.length>32)return unknown;
    if(Array.isArray(data)&&data.length===32)complete=false;
    let parent=nested?group.systemID:undefined;
    if(parent!==undefined&&!address(parent))return unknown;
    for(const row of Array.isArray(data)?data:[data]){
      if(!object(row)||!address(row.systemID)||!address(row.zoneID))return unknown;
      if(nested){if(parent!==undefined&&parent!==row.systemID)return unknown;parent=row.systemID;}
      const key=`${row.systemID}/${row.zoneID}`;
      if(identities.has(key))return unknown;
      identities.add(key);
    }
    if(nested&&parent!==undefined){if(systems.has(parent))return unknown;systems.add(parent);}
  }
  if(identities.has(`${system}/${zone}`))return {gateway:true,state:'present'};
  return {gateway:true,state:complete?'absent':'unknown'};
}
export function diagnosticAdvice(kind,state){
  if(state==='absent')return 'absent';
  if(state==='unknown')return 'unknown';
  if(kind==='low_battery')return 'battery';
  if(kind==='offline'&&state==='present')return 'present';
  return 'compare';
}
