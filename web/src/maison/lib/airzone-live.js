import { readingOf } from './airzone.js';

const number=(device,key)=>{
  const value=readingOf(device,key)?.value;
  return Number.isFinite(value)&&value>=0?value:null;
};
export const entityGeneration=(device)=>number(device,'sync_generation');

/** Freshness is a receipt, never the timestamp of the last value change. */
export function airzoneSync(device,gateway,{connected=true,now=Date.now(),browserReceivedAt=null}={}) {
  const received=number(device,'sync_last_received');
  const interval=number(device,'sync_poll_interval')??number(gateway,'sync_poll_interval');
  const age=received==null||received>now+5000?null:Math.max(0,now-received);
  const lateAfter=interval>0?(interval*2+5)*1000:null;
  const error=readingOf(gateway,'sync_last_error')?.value || null;
  const inProgress=readingOf(gateway,'sync_in_progress')?.value===true;
  const state=!connected?'disconnected':device?.online===false?'offline':device?.online!==true||received==null?'unknown'
    :age==null||lateAfter==null?'unknown':age>lateAfter?'stale':'current';
  return {state,received,age,interval,error,inProgress,browserReceivedAt,
    attempted:number(gateway,'sync_last_attempt'),responded:number(gateway,'sync_last_response'),
    succeeded:number(gateway,'sync_last_success'),generation:entityGeneration(device)};
}

export function gatewayFor(devices,instance) {
  return (Array.isArray(devices)?devices:Object.values(devices ?? {})).find(device=>device.instance===instance&&device.model==='Airzone Webserver') ?? null;
}
