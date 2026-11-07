import {readingOf} from './airzone.js';
import {numericRange,quantize} from './capabilities.js';

export const SETPOINT_KEYS=['target_temperature','heatsetpoint','coolsetpoint'];
export const setpointLabelKey=key=>key==='heatsetpoint'?'airzone_ui.heating_target':key==='coolsetpoint'?'airzone_ui.cooling_target':'airzone_ui.target';
export const primarySetpoints=device=>primarySetpointKeys(device).map(key=>({key,label:setpointLabelKey(key),reading:readingOf(device,key)}));
export const commandPending=(intent)=>['queued','authorizing','sending','awaiting'].includes(intent?.phase);
/** Presentation only: never infer an active target from temperature or demand. */
export function primarySetpointKeys(device) {
  const has=key=>device?.points?.some(point=>point.key===key);
  const separate=['heatsetpoint','coolsetpoint'].filter(has);
  const dual=readingOf(device,'double_sp')?.value;
  if((dual===true||dual===1)&&separate.length)return separate;
  const reported=key=>typeof readingOf(device,key)?.value==='number';
  const generic=has('target_temperature');
  if(generic&&reported('target_temperature'))return ['target_temperature'];
  const mode=readingOf(device,'mode')?.value;
  const contextual=mode==='heat'?'heatsetpoint':mode==='cool'?'coolsetpoint':null;
  if(contextual&&has(contextual)&&(!generic||reported(contextual)))return [contextual];
  if(generic&&(contextual||!separate.some(reported)))return ['target_temperature'];
  // Auto or unknown mode cannot justify choosing one of two independent targets.
  return separate;
}
/** A short decimal that represents exactly the same reported float32 value.
 * This formats a reading; it never quantizes it onto a command's step grid. */
export function numericEditorValue(value) {
  if(typeof value!=='number'||!Number.isFinite(value))return '';
  if(Math.fround(value)===value){
    for(let decimals=0;decimals<=9;decimals++){
      const short=Number(value.toFixed(decimals));
      if(Math.fround(short)===value)return String(short);
    }
  }
  return String(value);
}
export function controlPoints(device,variant='common') {
  const points=(device?.points ?? []).filter(point=>!point.key.startsWith('sync_')&&point.key!=='target_temperature_on');
  const main=[...primarySetpointKeys(device),'on','mode'];
  if(variant==='common')return main.flatMap(key=>points.filter(point=>point.key===key));
  return points.filter(point=>point.access?.write===true&&(variant==='all'||!main.includes(point.key)));
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
