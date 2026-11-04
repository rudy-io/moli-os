<script>
  import {t} from '../../lib/i18n.svelte.js';
  import {formatValue,deviceName} from '../../lib/format.js';
  import {hub,home} from '../lib/home.svelte.js';
  import {quantize} from '../lib/capabilities.js';
  import {climateMode} from '../lib/climate.js';
  import {readingOf} from '../lib/airzone.js';
  import {controlPoints,controlContract,controlValue,commandPending,validIntention,SETPOINT_KEYS,setpointLabelKey,numericEditorValue} from '../lib/airzone-controls.js';
  import {airzoneCommands,requestAirzone,denyPendingAirzone,hasPendingAirzone} from '../lib/airzone-command.svelte.js';
  import Icon from './Icon.svelte';
  let {device,sync,variant='common',members=[],embedded=false,onhistory}=$props();
  let drafts=$state({});
  let confirmation=$state(null);
  const points=$derived(controlPoints(device,variant));
  const fresh=$derived(sync?.state==='current');
  const native=$derived(hub.drivers.some(driver=>driver.kind==='airzone'&&driver.instance===device?.instance));
  const writable=$derived(points.some(point=>point.access?.write===true));
  const turnOn=$derived(variant==='common'?device.points?.find(point=>point.key==='target_temperature_on'):null);
  const target=$derived(device.points?.find(point=>point.key==='target_temperature'));
  const turnOnValue=$derived(target?controlValue(device,target,airzoneCommands[`${device.id}/target_temperature`]):null);
  const shared=$derived(device.model==='Airzone system');
  const retained=$derived(variant==='common'?Object.entries(airzoneCommands).filter(([key])=>key.startsWith(`${device.id}/`)&&!points.some(point=>key===`${device.id}/${point.key}`)&&(!turnOn||key!==`${device.id}/target_temperature_on`)).map(([id,intent])=>({id,intent,key:id.slice(device.id.length+1),point:device.points?.find(point=>id===`${device.id}/${point.key}`)})):[]);
  const primary=(point)=>variant==='common'&&SETPOINT_KEYS.includes(point.key);
  const pointLabel=(point)=>primary(point)?t(setpointLabelKey(point.key)):point.label||point.key;
  function valueLabel(point,value){
    const mode=(point.key==='mode'||point.key.endsWith('_mode'))?climateMode(value):null;
    return mode?t(mode.label):point.key==='on'&&typeof value==='boolean'?t(value?'airzone_control.on':'airzone_control.off'):formatValue(point,value);
  }
  const shown=(point,value)=>value==null?'—':`${valueLabel(point,value)}${point.unit?` ${point.unit}`:''}`;
  function submit(point,value){
    if(!native||!fresh||(shared&&!members.length)||home.held||!validIntention(device,point,value))return;
    if(shared||variant!=='common'||point.kind.type==='text'){confirmation={key:point.key,value};return;}
    requestAirzone(`${device.id}/${point.key}`,value);
  }
  function confirm(){
    const pending=confirmation;confirmation=null;
    if(pending)requestAirzone(`${device.id}/${pending.key}`,pending.value);
  }
  $effect(()=>{if(!fresh||!native||!device.points?.some(point=>point.key===confirmation?.key&&point.access?.write))confirmation=null;});
</script>
{#if native&&(points.length||retained.length)}
<div class="controls" class:embedded class:primary-controls={variant==='common'} role="group" aria-label={t(variant==='common'?'airzone_control.title':'airzone_control.advanced')}>
  {#if !embedded}<header><strong>{shared?t('airzone_control.system_title'):t(variant==='common'?'airzone_control.title':'airzone_control.advanced')}</strong></header>{/if}
  {#if !writable&&variant==='common'}<p class="access-note"><span class="access-state"><Icon name="lock" size={16}/>{t('airzone_control.read_only_short')}</span><a href="#/airzone/installation">{t('airzone_control.enable')}</a></p>{:else if !fresh}<p class="explanation">{t('airzone_control.unavailable')}</p>{/if}
  {#if shared}<p class="explanation">{t('airzone_control.shared')}{#if members.length} {members.map(deviceName).join(', ')}{:else} {t('airzone_control.scope_unknown')}{/if}</p>{/if}
  {#each points as point (point.key)}
    {@const id=`airzone-control-${encodeURIComponent(device.id)}-${point.key}`}
    {@const intent=airzoneCommands[`${device.id}/${point.key}`]}
    {@const contract=controlContract(device,point)}
    {@const value=controlValue(device,point,intent)}
    {@const disabled=!fresh||!point.access?.write||!contract||hasPendingAirzone(`${device.id}/${point.key}`)||(shared&&!members.length)||Boolean(home.held)}
    <div class="control-row" class:setpoint={primary(point)} data-point={point.key}>
      <label for={id}>{pointLabel(point)}{#if commandPending(intent)}<small>{t('airzone_control.observed',{value:shown(point,readingOf(device,point.key)?.value)})}</small>{/if}</label>
      {#if !point.access?.write||!fresh}
        <output {id} class="static-value num">{shown(point,value)}</output>
      {:else if contract?.type==='binary'}
        <button {id} class="toggle" role={typeof value==='boolean'?'switch':undefined} aria-checked={typeof value==='boolean'?value:undefined} aria-label={point.label||point.key} {disabled} onclick={()=>submit(point,value!==true)}>{value===true?t('airzone_control.on'):value===false?t('airzone_control.off'):t('airzone_control.turn_on_unknown')}</button>
      {:else if contract?.type==='numeric'}
        <div class="numeric"><button disabled={disabled||typeof value!=='number'||value<=contract.range.first} aria-label={t('airzone_control.decrease',{label:point.label||point.key})} onclick={()=>submit(point,quantize(value-contract.range.step,contract.range))}>−</button><input {id} type="number" min={contract.range.min} max={contract.range.max} step={contract.range.step} value={numericEditorValue(value)} {disabled} aria-label={pointLabel(point)} oninput={event=>{if(event.currentTarget.value!=='')submit(point,event.currentTarget.valueAsNumber);}}/><span>{point.unit??''}</span><button disabled={disabled||typeof value!=='number'||value>=contract.range.last} aria-label={t('airzone_control.increase',{label:point.label||point.key})} onclick={()=>submit(point,quantize(value+contract.range.step,contract.range))}>+</button></div>
      {:else if contract?.type==='enum'}
        <select {id} value={value??''} {disabled} onchange={event=>submit(point,event.currentTarget.value)}><option disabled value="">{t('airzone_ui.unknown')}</option>{#each contract.choices as choice}<option value={choice}>{valueLabel(point,choice)}</option>{/each}</select>
      {:else if contract?.type==='text'}
        <div class="text-editor"><input {id} type="text" maxlength="4000" value={drafts[point.key]??value??''} {disabled} oninput={event=>{drafts[point.key]=event.currentTarget.value;}}/><button disabled={disabled||!validIntention(device,point,drafts[point.key]??value)} onclick={()=>submit(point,drafts[point.key]??value)}>{t('airzone_control.apply')}</button></div>
      {:else}<output {id} class="static-value num">{shown(point,value)}</output><span class="explanation">{t('airzone_control.incomplete_contract')}</span>{/if}
      {#if primary(point)&&onhistory&&readingOf(device,point.key)}<button class="history-action" aria-label={t('airzone_ui.history_value',{label:pointLabel(point),value:shown(point,readingOf(device,point.key)?.value)})} onclick={()=>onhistory(point.key)}><Icon name="history" size={16}/>{t('airzone_detail.view_history')}</button>{/if}
      {#if intent}<p class="command-status" class:error={intent.phase==='error'} role="status" aria-live="polite"><Icon name={intent.phase==='confirmed'?'check':intent.phase==='error'?'alert':'clock'} size={15}/>{intent.phase==='error'?t(`airzone_control.errors.${intent.error}`):t(`airzone_control.phase.${intent.phase}`,{value:shown(point,intent.value)})}</p>{/if}
      {#if hasPendingAirzone(`${device.id}/${point.key}`)&&!home.held}<button onclick={()=>denyPendingAirzone(`${device.id}/${point.key}`)}>{t('airzone_control.deny_pending')}</button>{/if}
    </div>
  {/each}
  {#if turnOn && (turnOn.access?.write || airzoneCommands[`${device.id}/target_temperature_on`] || hasPendingAirzone(`${device.id}/target_temperature_on`))}
    {@const intent=airzoneCommands[`${device.id}/target_temperature_on`]}
    {#if turnOn.access?.write && fresh && points.some(point=>point.key==='target_temperature') && readingOf(device,'on')?.value!==true}<button class="turn-on" disabled={!fresh||Boolean(home.held)||hasPendingAirzone(`${device.id}/target_temperature_on`)||!validIntention(device,turnOn,turnOnValue)} onclick={()=>submit(turnOn,turnOnValue)}>{t('airzone_control.turn_on_at',{value:shown(turnOn,turnOnValue)})}</button>{/if}
    {#if intent}<p class="command-status" class:error={intent.phase==='error'} role="status">{intent.phase==='error'?t(`airzone_control.errors.${intent.error}`):t(`airzone_control.phase.${intent.phase}`,{value:shown(turnOn,intent.value)})}</p>{/if}
    {#if hasPendingAirzone(`${device.id}/target_temperature_on`)&&!home.held}<button onclick={()=>denyPendingAirzone(`${device.id}/target_temperature_on`)}>{t('airzone_control.deny_pending')}</button>{/if}
  {/if}
  {#each retained as item (item.id)}
    <div class="retained-command" data-retained-command={item.key}>
      <strong>{item.point?.label||item.key}</strong>
      <p class="command-status" class:error={item.intent.phase==='error'} role="status" aria-live="polite">{item.intent.phase==='error'?t(`airzone_control.errors.${item.intent.error}`):t(`airzone_control.phase.${item.intent.phase}`,{value:item.point?shown(item.point,item.intent.value):String(item.intent.value)})}</p>
      {#if hasPendingAirzone(item.id)&&!home.held}<button onclick={()=>denyPendingAirzone(item.id)}>{t('airzone_control.deny_pending')}</button>{/if}
    </div>
  {/each}
  {#if confirmation}
    {@const point=device.points.find(point=>point.key===confirmation.key)}
    <div class="confirmation" role="group" aria-label={t('airzone_control.confirm_title')}>
      <strong>{t('airzone_control.confirm_title')}</strong><p>{point?.label||confirmation.key} : {point?shown(point,confirmation.value):'—'}</p><p>{shared?t('airzone_control.shared'):t('airzone_control.native_setting')}{#if shared&&members.length} {members.map(deviceName).join(', ')}{/if}</p>
      <div class="actions"><button onclick={()=>confirmation=null}>{t('commun.annuler')}</button><button class="primary" onclick={confirm}>{t('airzone_control.confirm')}</button></div>
    </div>
  {/if}
</div>
{/if}
<style>
.embedded {border:0!important;padding:0!important;background:transparent!important}
.primary-controls {display:grid;grid-template-columns:1fr 1fr;gap:12px 20px}
.primary-controls>header,.primary-controls>.explanation,.access-note,.setpoint,.turn-on,.retained-command,.confirmation {grid-column:1/-1}
.primary-controls .control-row {grid-template-columns:minmax(0,1fr);border:0;padding:8px 0}
.primary-controls .setpoint {text-align:center;padding:12px 0 20px}
.primary-controls .setpoint>label {font-size:1rem;color:var(--ink-2);font-weight:550}
.primary-controls .setpoint .numeric {grid-template-columns:52px minmax(0,1fr) auto 52px;gap:12px;max-width:440px;margin:auto;width:100%}
.primary-controls .setpoint .numeric button {height:52px;font-size:1.75rem;border-radius:50%;background:var(--surface);box-shadow:var(--shadow)}
.primary-controls .setpoint .numeric input {font-size:clamp(2.25rem,7vw,4.5rem);font-weight:550;line-height:1.2;letter-spacing:-.05em;background:transparent;border:0;padding:8px 0;appearance:textfield;-moz-appearance:textfield}
.primary-controls .setpoint .numeric input::-webkit-inner-spin-button,.primary-controls .setpoint .numeric input::-webkit-outer-spin-button {appearance:none;margin:0}
.primary-controls .setpoint .numeric span {font-size:1.25rem}
.static-value {font-size:1.125rem;font-weight:600;overflow-wrap:anywhere}
.primary-controls .setpoint .static-value {font-size:clamp(2.5rem,8vw,4.5rem);line-height:1.2;letter-spacing:-.05em}
.access-note {display:flex;align-items:center;gap:8px;flex-wrap:wrap;font-size:.875rem;line-height:1.45;color:var(--ink-2);padding:12px 14px;border-radius:12px;background:var(--surface-2)}
.access-note .access-state {flex:1;display:flex;align-items:center;gap:6px;font-weight:600}.access-note a {font-weight:600;min-height:44px}
.primary-controls .history-action {justify-self:center;width:auto;display:flex;align-items:center;gap:6px;background:transparent;border:0;color:var(--ink-2);font-size:.875rem;font-weight:500}
.retained-command {display:grid;gap:10px;padding:14px;border:1px solid var(--line);border-radius:12px}
@media (max-width:440px) {.primary-controls .setpoint .numeric {grid-template-columns:44px minmax(0,1fr) auto 44px;gap:6px}.primary-controls .setpoint .numeric button {height:44px;font-size:1.5rem}}
@media (prefers-reduced-motion:no-preference) {button:not(:disabled):active {transform:scale(.97)}}

.controls {padding:16px;border:1px solid var(--line);border-radius:var(--r-md);background:var(--surface);min-width:0}header {display:flex;align-items:center;justify-content:space-between;gap:12px;flex-wrap:wrap;margin-bottom:8px}header strong {font-size:1rem;font-weight:650}a {color:var(--ink-2);font-size:.875rem;min-height:44px;display:inline-flex;align-items:center}.explanation {color:var(--ink-2);font-size:.875rem;line-height:1.5;margin-bottom:10px}.control-row {display:grid;grid-template-columns:minmax(0,1fr) minmax(100px,1fr);align-items:center;gap:10px 14px;padding:12px 0;border-top:1px solid var(--line)}label {font-size:.9375rem;font-weight:550;overflow-wrap:anywhere}label small {display:block;color:var(--ink-2);font-size:.8125rem;line-height:1.5;margin-top:5px}button,input,select {min-height:44px;border:1px solid var(--line);border-radius:10px;background:var(--surface-2);color:var(--ink);font:inherit;padding:9px 11px;min-width:0;width:100%;touch-action:manipulation}button {cursor:pointer;font-weight:600}button:disabled,input:disabled,select:disabled {opacity:.55;cursor:default}.toggle[aria-checked=true] {background:var(--cool-soft);color:var(--cool)}button:focus-visible,input:focus-visible,select:focus-visible,a:focus-visible {outline:3px solid var(--cool);outline-offset:3px}.control-row:has(.numeric)>label {grid-column:1/-1}.numeric {display:grid;grid-template-columns:44px minmax(0,1fr) auto 44px;grid-column:1/-1;align-items:center;gap:7px;min-width:0}.numeric input {text-align:center}.numeric span {font-size:.875rem;color:var(--ink-2)}.command-status {grid-column:1/-1;display:flex;align-items:center;gap:7px;font-size:.8125rem;color:var(--ink-2);line-height:1.5}.command-status.error {color:var(--alert)}.text-editor {display:grid;gap:6px}.confirmation {border:1px solid var(--line);border-radius:12px;padding:16px;background:var(--surface-2);line-height:1.5}.confirmation p {font-size:.875rem;margin-top:8px;overflow-wrap:anywhere}.actions {display:flex;gap:12px;margin-top:14px}.actions .primary {background:var(--ink);color:var(--bg)}@media(max-width:340px){.control-row {grid-template-columns:1fr}.command-status {grid-column:auto}.actions {flex-direction:column}}
</style>
