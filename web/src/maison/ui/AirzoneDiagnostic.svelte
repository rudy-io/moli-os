<script>
  import {diagnosticRequest} from '../lib/airzone-diagnostic.js';
  import {t} from '../../lib/i18n.svelte.js';
  import {hub,home} from '../lib/home.svelte.js';
  let {instance,connected=true}=$props();
  let operation=$state('version'),system=$state(0),zone=$state(''),sensor=$state(0),get=$state(false);
  let busy=$state(false),result=$state(null),error=$state('');
  const integer=(value,min,max)=>Number.isInteger(Number(value))&&Number(value)>=min&&Number(value)<=max;
  const valid=$derived(operation==='hvac'?(integer(system,0,32)||Number(system)===127&&zone==='')&&(zone===''||integer(zone,0,32)):operation==='iaq'?integer(system,0,32)&&integer(sensor,0,32):true);
  async function inspect(authenticated=false){
    if(busy||!connected||!valid)return;
    if(!authenticated&&!hub.session?.human){if(home.held)return;home.held={label:t('airzone_diagnostic.title'),reason:'configuration',custom:()=>inspect(true)};return;}
    busy=true;result=null;error='';
    const body={operation};
    if(['hvac','iaq'].includes(operation)){body.system=Number(system);body.get=get;}
    if(operation==='hvac'&&zone!=='')body.zone=Number(zone);
    if(operation==='iaq')body.sensor=Number(sensor);
    const answer=await diagnosticRequest(instance,body);
    if(answer.ok)result=answer.data;
    else {if(answer.error==='auth')hub.session.human=false;error=answer.error==='timeout'||answer.error==='cancelled'?'network':answer.error;}
    busy=false;
  }
</script>
<details class="diagnostic">
  <summary>{t('airzone_diagnostic.title')}</summary>
  <div class="body">
    <p>{t('airzone_diagnostic.help')}</p>
    <form onsubmit={event=>{event.preventDefault();inspect();}}>
      <label>{t('airzone_diagnostic.operation')}<select bind:value={operation} disabled={busy}>{#each ['hvac','iaq','version','webserver','integration','demo','thermostat'] as choice}<option value={choice}>{t(`airzone_diagnostic.operations.${choice}`)}</option>{/each}</select></label>
      {#if ['hvac','iaq'].includes(operation)}
        <label>{t('airzone_diagnostic.system')}<input type="number" min="0" max={operation==='hvac'?127:32} step="1" bind:value={system} disabled={busy}/></label>
        {#if operation==='hvac'}<label>{t('airzone_diagnostic.zone')}<input type="number" min="0" max="32" step="1" value={zone} oninput={event=>zone=event.currentTarget.value} disabled={busy}/></label>{:else}<label>{t('airzone_diagnostic.sensor')}<input type="number" min="0" max="32" step="1" bind:value={sensor} disabled={busy}/></label>{/if}
        <label>{t('airzone_diagnostic.method')}<select bind:value={get} disabled={busy}><option value={false}>POST</option><option value={true}>GET</option></select></label>
        <p class="hint">{t('airzone_diagnostic.address_help')}</p>
      {/if}
      <button disabled={busy||!connected||!valid||Boolean(home.held)}>{t(busy?'airzone_diagnostic.running':'airzone_diagnostic.run')}</button>
    </form>
    {#if error}<p class="error" role="alert">{t(`airzone_diagnostic.errors.${error}`)}</p>{/if}
    {#if result}<div class="result" role="status"><strong>{t(result.synthetic?'airzone_diagnostic.synthetic':result.interface_available?'airzone_diagnostic.interface_available':'airzone_diagnostic.success')}</strong>{#if result.synthetic}<p>{t('airzone_diagnostic.synthetic_help')}</p>{/if}{#if result.interface_available}<p>{t('airzone_diagnostic.interface_help',{bytes:result.response_bytes??t('airzone_ui.unknown')})}</p>{/if}<details><summary>{t('airzone_diagnostic.technical')}</summary><pre>{JSON.stringify(result,null,2)}</pre></details></div>{/if}
  </div>
</details>
<style>
.diagnostic {min-width:0;border:1px solid var(--line);border-radius:var(--r-md);background:var(--surface)}summary {padding:16px;cursor:pointer;min-height:44px;font-weight:600}.body {padding:0 16px 16px}.body>p,.hint,.result p {color:var(--ink-2);font-size:.875rem;line-height:1.5;margin-bottom:12px}form {display:grid;grid-template-columns:repeat(2,minmax(0,1fr));gap:14px}label {display:grid;gap:6px;font-size:.875rem}input,select,button {font:inherit;min-height:44px;width:100%;min-width:0;padding:10px;border:1px solid var(--line);border-radius:10px;background:var(--surface-2);color:var(--ink)}button {font-weight:600;cursor:pointer}button:disabled {opacity:.5;cursor:default}input:focus-visible,select:focus-visible,button:focus-visible,summary:focus-visible {outline:3px solid var(--cool);outline-offset:3px}.hint {grid-column:1/-1}.error {color:var(--alert);margin-top:12px;line-height:1.5}.result {margin-top:16px}.result p {margin-top:8px}pre {white-space:pre-wrap;word-break:break-word;font-size:.75rem;line-height:1.5;background:var(--surface-2);padding:12px;max-height:400px;overflow:auto}@media(max-width:420px){form {grid-template-columns:1fr}}
</style>
