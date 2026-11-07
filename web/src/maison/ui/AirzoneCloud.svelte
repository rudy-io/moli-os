<script>
  import {onMount} from 'svelte';
  import {hub,home} from '../lib/home.svelte.js';
  import {t,locale} from '../../lib/i18n.svelte.js';
  import {ago} from '../../lib/format.js';
  import {createAirzoneCloudClient,cloudDevices,cloudReadings} from '../lib/airzone-cloud.js';
  import Icon from './Icon.svelte';
  let opened=$state(false),busy=$state(false),status=$state(null),error=$state('');
  let email=$state(''),password=$state('');
  let installationsLoaded=$state(false);
  let installations=$state([]),installationId=$state(''),installation=$state(null),deviceId=$state('');
  let lastReading=$state(null),technical=$state(null),lastError=$state(''),now=$state(Date.now());
  let revision=0;
  let pollPaused=$state(false),gatewayId=$state(''),includeDevices=$state(false);
  const client=createAirzoneCloudClient();
  const devices=$derived(cloudDevices(installation));
  const readings=$derived(cloudReadings(lastReading?.data));
  const gateways=$derived([...new Set([...(installations.find(item=>item.installation_id===installationId)?.ws_ids??[]),...devices.map(device=>device.ws_id)].filter(value=>typeof value==='string'&&value))]);
  const selected=$derived(devices.find(device=>device.device_id===deviceId));
  const fresh=$derived(lastReading!=null&&!lastError&&now-lastReading.received_at<=65000&&lastReading.received_at<=now+5000);
  const at=(value)=>new Date(value).toLocaleString(locale());
  const visible=()=>typeof document==='undefined'||document.visibilityState!=='hidden';
  const resetSelection=()=>{client.cancel();revision++;busy=false;installation=null;deviceId='';lastReading=null;technical=null;lastError='';gatewayId='';};
  function authorize(action){
    if(hub.session?.human){void action();return;}
    if(home.held)return;
    home.held={label:t('airzone_cloud.title'),reason:'configuration',custom:action,onCancel:()=>{password='';}};
  }
  async function request(operation,body){
    const id=++revision;busy=true;error='';
    const response=await client.request(operation,body);
    if(id!==revision||response.cancelled)return null;
    busy=false;
    if(response.error){error=response.error;if(error==='moli_auth')hub.session.human=false;if(['moli_auth','cloud_auth','migration_pending'].includes(error))pollPaused=true;return null;}
    return response.data;
  }
  async function open(){
    opened=true;
    const response=await request('status');
    if(!response)return;
    status=response;pollPaused=false;
    if(status.state==='connected'||status.state==='expired'){
      const loaded=await loadInstallations();
      if(loaded&&status.state==='expired'){const renewed=await request('status');if(renewed)status=renewed;}
    }
  }
  async function login(){
    if(busy||!email.trim()||!password)return;
    const secret=password;password='';resetSelection();installations=[];installationsLoaded=false;installationId='';
    const response=await request('login',{email:email.trim(),password:secret});
    if(!response)return;
    email='';status=response;pollPaused=false;
    if(status.state==='connected'||status.state==='expired'){
      const loaded=await loadInstallations();
      if(loaded&&status.state==='expired'){const renewed=await request('status');if(renewed)status=renewed;}
    }
  }
  async function disconnect(){
    resetSelection();installations=[];installationId='';
    const response=await request('disconnect',{});if(response)status=response;
  }
  async function loadInstallations(){
    const response=await request('read',{operation:'installations'});
    if(response){resetSelection();installationId='';installations=Array.isArray(response.data)?response.data.filter(item=>typeof item.installation_id==='string'):[];installationsLoaded=true;}
    return Boolean(response);
  }
  async function chooseInstallation(){
    resetSelection();if(!installationId)return;
    const selected=installationId;
    const response=await request('read',{operation:'installation',installation_id:selected});
    if(response&&installationId===selected)installation=response.data;
  }
  async function chooseDevice(){pollPaused=false;gatewayId=selected?.ws_id??'';lastReading=null;technical=null;lastError='';client.cancel();revision++;busy=false;await poll();}
  async function poll(){
    if(busy||pollPaused||!visible()||status?.state!=='connected'||!installationId||!deviceId)return;
    const target=deviceId,where=installationId;
    const response=await request('read',{operation:'device_status',installation_id:where,device_id:target});
    if(target!==deviceId||where!==installationId)return;
    if(response){lastReading=response;lastError='';now=Date.now();if(status.expires_at&&status.expires_at*1000<now)status.expires_at=null;}else if(error){lastError=error;if(['moli_auth','cloud_auth','cloud_forbidden','migration_pending'].includes(error))pollPaused=true;}
  }
  function refreshReading(){
    if(busy)return;
    authorize(async()=>{pollPaused=false;await poll();});
  }
  async function inspect(operation,section='all'){
    if(busy)return;
    const body={operation};
    if(['device_config','gateway_status'].includes(operation))body.installation_id=installationId;
    if(operation==='device_config'){body.device_id=deviceId;body.section=section;}
    if(operation==='gateway_status'){body.gateway_id=gatewayId;body.include_devices=includeDevices;}
    if(operation==='location')body.location_id=installation?.location_id;
    const response=await request('read',body);if(response)technical={operation,...response};
  }
  onMount(()=>{
    const timer=setInterval(()=>{void poll();},30000);
    const clock=setInterval(()=>{now=Date.now();},1000);
    const resume=()=>{if(visible())void poll();};
    const visibility=()=>{if(visible())resume();else{client.cancel();revision++;busy=false;}};
    document.addEventListener('visibilitychange',visibility);globalThis.addEventListener('online',resume);
    return()=>{clearInterval(timer);clearInterval(clock);client.cancel();revision++;password='';document.removeEventListener('visibilitychange',visibility);globalThis.removeEventListener('online',resume);};
  });
</script>
<section class="cloud" aria-label={t('airzone_cloud.title')}>
  <header><span class="cloud-icon"><Icon name="lan" size={22}/></span><div><h2>{t('airzone_cloud.title')}</h2><p>{t('airzone_cloud.subtitle')}</p></div><span class="badge">{t('airzone_cloud.read_only')}</span></header>
  <p class="help">{t('airzone_cloud.separate')}</p>
  {#if !opened}<button class="primary" onclick={()=>authorize(open)} disabled={Boolean(home.held)}>{t('airzone_cloud.open')}</button>
  {:else}
    {#if status}<p class="state" role="status">{t(`airzone_cloud.state.${status.state}`)}{#if status.expires_at}<span>{t('airzone_cloud.expires',{time:at(status.expires_at*1000)})}</span>{/if}</p>{/if}
    {#if !status||status.state!=='connected'}
      <form onsubmit={event=>{event.preventDefault();authorize(login);}}>
        <label>{t('airzone_cloud.email')}<input type="email" autocomplete="username" bind:value={email} disabled={busy} required/></label>
        <label>{t('airzone_cloud.password')}<input type="password" autocomplete="current-password" bind:value={password} disabled={busy} required/></label>
        <p class="help">{t('airzone_cloud.password_help')}</p>
        <button class="primary" disabled={busy||!email.trim()||!password||Boolean(home.held)}>{t(busy?'airzone_cloud.working':'airzone_cloud.login')}</button>
      </form>
      <button disabled={busy} onclick={()=>authorize(open)}>{t('airzone_cloud.check_connection')}</button>
      {#if status?.persistence_available===false}<p class="help">{t('airzone_cloud.no_persistence')}</p>{/if}
    {:else}
      <div class="selectors">
        <label>{t('airzone_cloud.installation')}<select bind:value={installationId} onchange={chooseInstallation} disabled={busy}><option value="">{t('airzone_cloud.choose_installation')}</option>{#each installations as entry (entry.installation_id)}<option value={entry.installation_id}>{entry.name||entry.installation_id}</option>{/each}</select></label>
        {#if installation}<label>{t('airzone_cloud.device')}<select bind:value={deviceId} onchange={chooseDevice} disabled={busy}><option value="">{t('airzone_cloud.choose_device')}</option>{#each devices as device (device.device_id)}<option value={device.device_id}>{device.name||device.device_id}{device.group_name?` · ${device.group_name}`:''}</option>{/each}</select></label>{/if}
      </div>
      {#if installationsLoaded&&!installations.length&&!busy}<p class="help">{t('airzone_cloud.no_installations')}</p>{/if}
      {#if deviceId}
        <div class="device-reading">
          <div class="reading-heading"><h3>{selected?.name||deviceId}</h3><button onclick={refreshReading} disabled={busy||Boolean(home.held)}>{t('airzone_cloud.refresh')}</button></div>
          <p class="freshness" role="status">{t(fresh?'airzone_cloud.current':lastReading?'airzone_cloud.stale':'airzone_cloud.no_reading')}{#if lastReading}<span>{t('airzone_cloud.received',{time:ago(lastReading.received_at,now)})}</span>{/if}</p>
          {#if lastReading}<dl>{#each readings as reading (reading.key)}<div><dt>{t(`airzone_cloud.readings.${reading.key}`)}</dt><dd>{typeof reading.value==='boolean'?t(reading.value?'airzone_cloud.yes':'airzone_cloud.no'):reading.value}{#if reading.unit} {reading.unit}{/if}</dd></div>{/each}</dl><details><summary>{t('airzone_cloud.raw_status')}</summary><pre>{JSON.stringify(lastReading.data,null,2)}</pre></details>{/if}
          <p class="help">{t('airzone_cloud.poll_help')}</p>
        </div>
      {/if}
      <details class="advanced"><summary>{t('airzone_cloud.advanced')}</summary><div class="actions">
        <button disabled={busy} onclick={()=>inspect('profile')}>{t('airzone_cloud.profile')}</button>
        {#if installation?.location_id}<button disabled={busy} onclick={()=>inspect('location')}>{t('airzone_cloud.location')}</button>{/if}
        {#if deviceId}{#each ['all','user','advanced'] as section}<button disabled={busy} onclick={()=>inspect('device_config',section)}>{t(`airzone_cloud.config.${section}`)}</button>{/each}{/if}
        {#if gateways.length}<label>{t('airzone_cloud.gateway_choice')}<select bind:value={gatewayId} disabled={busy}><option value="">{t('airzone_cloud.choose_gateway')}</option>{#each gateways as gateway}<option value={gateway}>{gateway}</option>{/each}</select></label><label class="checkbox"><input type="checkbox" bind:checked={includeDevices} disabled={busy}/>{t('airzone_cloud.include_devices')}</label><button disabled={busy||!gatewayId} onclick={()=>inspect('gateway_status')}>{t('airzone_cloud.gateway')}</button>{/if}
      </div>{#if technical}<p class="help">{t('airzone_cloud.received',{time:ago(technical.received_at,now)})}</p><details><summary>{t('airzone_cloud.technical_result')}</summary><pre>{JSON.stringify(technical.data,null,2)}</pre></details>{/if}</details>
      <div class="actions account"><button disabled={busy} onclick={()=>authorize(loadInstallations)}>{t('airzone_cloud.reload_installations')}</button><button disabled={busy} onclick={()=>authorize(disconnect)}>{t('airzone_cloud.disconnect')}</button></div>
    {/if}
    {#if error}<p class="error" role="alert">{t(`airzone_cloud.errors.${error}`)}</p>{#if ['moli_auth','cloud_auth','migration_pending'].includes(error)}<button onclick={()=>authorize(open)}>{t('airzone_cloud.authenticate')}</button>{/if}{/if}
  {/if}
  <p class="scope">{t('airzone_cloud.schedules_scope')}</p>
</section>
<style>
.cloud {border:1px solid var(--line);border-radius:var(--r-lg);padding:24px;background:var(--surface);min-width:0}header {display:flex;align-items:flex-start;gap:12px;flex-wrap:wrap}header>div {flex:1;min-width:0}h2 {font-size:1.125rem;font-weight:650}h3 {font-size:1rem;font-weight:600;overflow-wrap:anywhere}header p,.help,.scope {font-size:.875rem;line-height:1.55;color:var(--ink-2)}header p {margin-top:4px}.help {margin:12px 0}.scope {padding-top:16px;border-top:1px solid var(--line);margin-top:20px}.cloud-icon {color:var(--ink-2);padding-top:2px}.badge {font-size:.75rem;color:var(--ink-2);border:1px solid var(--line);border-radius:99px;padding:6px 10px}form,.selectors {display:grid;grid-template-columns:repeat(2,minmax(0,1fr));gap:14px;margin-top:16px}form>.help {grid-column:1/-1;margin:0}label {display:grid;gap:7px;font-size:.875rem}input,select,button {font:inherit;min-height:44px;min-width:0;padding:10px 12px;border:1px solid var(--line);border-radius:10px;background:var(--surface-2);color:var(--ink)}button {cursor:pointer;font-weight:600;font-size:.875rem}button:disabled,input:disabled,select:disabled {opacity:.5;cursor:default}.primary {background:var(--ink);color:var(--bg)}input:focus-visible,select:focus-visible,button:focus-visible,summary:focus-visible {outline:3px solid var(--cool);outline-offset:3px}.state {font-size:.9375rem;line-height:1.5;margin-top:16px}.state span,.freshness span {display:block;color:var(--ink-2);font-size:.8125rem;margin-top:3px}.device-reading {padding:18px;border:1px solid var(--line);border-radius:var(--r-md);margin-top:18px}.reading-heading {display:flex;gap:12px;align-items:center;justify-content:space-between}.freshness {margin:12px 0;font-size:.875rem;line-height:1.5}dl {margin:0}dl>div {display:flex;justify-content:space-between;gap:16px;padding:10px 0;border-top:1px solid var(--line);font-size:.9375rem}dt {color:var(--ink-2)}dd {margin:0;font-weight:600}details {margin-top:14px}summary {cursor:pointer;min-height:44px;display:flex;align-items:center;font-weight:600;font-size:.875rem}pre {padding:12px;background:var(--surface-2);border-radius:10px;max-height:400px;overflow:auto;white-space:pre-wrap;word-break:break-word;font-size:.75rem;line-height:1.5}.actions {display:flex;flex-wrap:wrap;gap:10px}.account {margin-top:18px}.checkbox {display:flex;align-items:center;gap:8px}.checkbox input {width:20px;min-height:20px}.error {color:var(--alert);margin-top:14px;line-height:1.5;font-size:.875rem}@media(max-width:600px){.cloud {padding:18px}form,.selectors {grid-template-columns:1fr}.reading-heading {align-items:flex-start;flex-wrap:wrap}.device-reading {padding:14px}}
</style>
