const ROOT='/api/integrations/airzone/cloud';
const STATES=new Set(['disconnected','connected','expired','authentication_required','migration_pending']);
/** One bounded request at a time. Switching a selection invalidates its old
 * reply even if a transport ignores AbortController. No automatic retries. */
export function createAirzoneCloudClient({fetcher=fetch,timeoutMs=35000}={}) {
  let generation=0,controller=null;
  const cancel=()=>{generation++;controller?.abort();controller=null;};
  return {cancel,async request(operation,body){
    cancel();const revision=generation,abort=new AbortController();controller=abort;
    const timer=setTimeout(()=>abort.abort(),timeoutMs);
    try{
      const response=await fetcher(`${ROOT}/${operation}`,{method:body===undefined?'GET':'POST',headers:{'content-type':'application/json','x-moli-origin':'ui'},body:body===undefined?undefined:JSON.stringify(body),signal:abort.signal,cache:'no-store'});
      const data=await response.json().catch(()=>null);
      if(revision!==generation)return {cancelled:true};
      if(!response.ok){
        const cloudErrors={authentication_required:'cloud_auth',forbidden:'cloud_forbidden',migration_pending:'migration_pending',rate_limited:'busy',busy:'busy',transport:'network',storage:'storage',incomplete_inventory:'incomplete_inventory',invalid_response:'invalid_response',invalid_input:'invalid',unavailable:'unavailable'};
        return {error:cloudErrors[data?.error]??(response.status===401||response.status===403?'moli_auth':response.status===429?'busy':response.status===404?'missing':response.status===422?'invalid':'failed')};
      }
      if(data?.read_only!==true)return {error:'invalid_response'};
      if(operation==='read'){
        if(!Number.isFinite(data.received_at)||data.received_at<0||!Object.hasOwn(data,'data'))return {error:'invalid_response'};
      }else if(!STATES.has(data.state))return {error:'invalid_response'};
      return {data};
    }catch{return revision!==generation?{cancelled:true}:{error:'network'};}
    finally{clearTimeout(timer);if(revision===generation)controller=null;}
  }};
}
export function cloudDevices(installation){
  const seen=new Set();
  return (Array.isArray(installation?.groups)?installation.groups:[]).flatMap(group=>(Array.isArray(group.devices)?group.devices:[]).flatMap(device=>{
    if(typeof device.device_id!=='string'||seen.has(device.device_id))return [];
    seen.add(device.device_id);return [{...device,group_name:group.name??''}];
  }));
}
const numeric=value=>typeof value==='number'&&Number.isFinite(value)?value:null;
const temperature=value=>numeric(value?.celsius)!=null?{value:value.celsius,unit:'°C'}:numeric(value?.fah)!=null?{value:value.fah,unit:'°F'}:null;
/** Explicitly typed Web API readings only: manufacturer modes and speeds keep
 * their actual code; local-API enums must not be guessed for cloud families. */
export function cloudReadings(data){
  if(!data||typeof data!=='object'||Array.isArray(data))return [];
  const readings=[];
  const add=(key,value,unit=null)=>{if(value!==null&&value!==undefined)readings.push({key,value,unit});};
  add('connection',typeof data.isConnected==='boolean'?data.isConnected:null);
  add('power',typeof data.power==='boolean'?data.power:null);
  for(const[key,field]of [['temperature','local_temp'],['heating_target','setpoint_air_heat'],['cooling_target','setpoint_air_cool']]){
    const reading=temperature(data[field]);if(reading)add(key,reading.value,reading.unit);
  }
  add('humidity',numeric(data.humidity)!=null&&data.humidity>=0&&data.humidity<=100?data.humidity:null,'%');
  for(const[key,field]of [['mode','mode'],['fan_speed','speed_conf']])add(key,typeof data[field]==='string'||numeric(data[field])!=null?data[field]:null);
  if(Array.isArray(data.errors))add('errors',data.errors.length);
  return readings;
}
