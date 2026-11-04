import {readingOf} from './airzone.js';
import {numericRange,quantize} from './capabilities.js';

export const COMMON_AIRZONE_POINTS=['on','target_temperature','heatsetpoint','coolsetpoint','mode','fan_speed'];
export const commandPending=(intent)=>['queued','authorizing','sending','awaiting'].includes(intent?.phase);
export function controlPoints(device,variant='common') {
  const points=(device?.points ?? []).filter(point=>!point.key.startsWith('sync_')&&point.key!=='target_temperature_on');
  if(variant==='common')return COMMON_AIRZONE_POINTS.flatMap(key=>points.filter(point=>point.key===key));
  return points.filter(point=>point.access?.write===true&&(variant==='all'||!COMMON_AIRZONE_POINTS.includes(point.key)));
}
export function controlContract(device,point) {
  const kind=point?.kind;
  if(kind?.type==='numeric'){
    // No invented limits, increment or zero. Missing metadata is observable,
    // but it is not a safe editor contract.
    const range=numericRange(device,point.key,null);
    return range?{type:'numeric',range}:null;
  }
  if(kind?.type==='binary')return {type:'binary'};
  if(kind?.type==='enum'&&Array.isArray(kind.values)){
    const choices=[...new Set(kind.values.filter(value=>typeof value==='string'&&value.length))];
    return choices.length?{type:'enum',choices}:null;
  }
  if(kind?.type==='text')return {type:'text'};
  return null;
}
export function validIntention(device,point,value) {
  const contract=controlContract(device,point);
  if(!contract||point.access?.write!==true)return false;
  if(contract.type==='numeric')return typeof value==='number'&&Number.isFinite(value)&&Math.abs((quantize(value,contract.range) ?? Infinity)-value)<1e-8;
  if(contract.type==='binary')return typeof value==='boolean';
  if(contract.type==='enum')return contract.choices.includes(value);
  return typeof value==='string'&&value.length>0&&[...value].length<=4000;
}
export function controlValue(device,point,intent) {
  return commandPending(intent)?intent.value:readingOf(device,point.key)?.value ?? null;
}
