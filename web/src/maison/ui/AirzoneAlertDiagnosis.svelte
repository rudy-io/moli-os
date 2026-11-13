<script>
  import {onDestroy} from 'svelte';
  import {t,locale} from '../../lib/i18n.svelte.js';
  import {hub,home} from '../lib/home.svelte.js';
  import {readingOf} from '../lib/airzone.js';
  import {gatewayApproval} from '../lib/gateway-approval.js';
  import {diagnosticRequest,summarizeZoneDiagnostic,diagnosticAdvice,ALERT_DIAGNOSTIC_READ} from '../lib/airzone-diagnostic.js';
  import Icon from './Icon.svelte';
  let {instance,device=null,kind='offline',href='#/airzone/installation',connected=true}=$props();
  let busy=$state(false),awaitingPin=$state(false),result=$state(null),failure=$state('');
  let revision=0,alive=true,controller=null;
  const system=$derived(readingOf(device,'systemID')?.value);
  const zone=$derived(readingOf(device,'zoneID')?.value);
  const scope=$derived(JSON.stringify([instance,device?.id,system,zone,connected]));
  const available=$derived(Boolean(connected&&instance&&hub.drivers.some(driver=>driver.kind==='airzone'&&driver.instance===instance)));
  const current=intent=>alive&&available&&intent.revision===revision&&intent.scope===scope;
  const approval=gatewayApproval({home,label:()=>t('airzone_health.check.title'),waiting:value=>{awaitingPin=value;},failed:code=>{failure=code;},current,perform});
  function stop(){revision++;controller?.abort();controller=null;approval.cancel();busy=false;result=null;failure='';}
  $effect(()=>{scope;return stop;});
  onDestroy(()=>{alive=false;stop();approval.destroy();});
  function verify(){if(!available||busy||awaitingPin)return;return perform({revision,scope,instance,system,zone});}
  async function perform(intent,authenticated=false){
    if(!current(intent)||busy)return;
    if(!authenticated&&!hub.session?.human){approval.ask(intent);return;}
    busy=true;result=null;failure='';
    const request=new AbortController();controller=request;
    const answer=await diagnosticRequest(intent.instance,ALERT_DIAGNOSTIC_READ,{signal:request.signal,realOnly:true});
    if(!current(intent)||controller!==request)return;
    controller=null;busy=false;
    if(!answer.ok){if(answer.error==='auth')hub.session.human=false;failure=answer.error;return;}
    result={...summarizeZoneDiagnostic(answer.data,{system:intent.system,zone:intent.zone}),checkedAt:Date.now()};
  }
  const checkedTime=value=>new Date(value).toLocaleString(locale(),{day:'numeric',month:'short',hour:'2-digit',minute:'2-digit',second:'2-digit'});
</script>

<div class="alert-diagnosis">
  <div class="actions">
    <button class="verify" disabled={!available||busy||awaitingPin||Boolean(home.held)} onclick={verify}><Icon name="search" size={18}/>{t(awaitingPin?'airzone_health.check.waiting_pin':busy?'airzone_health.check.running':'airzone_health.check.run')}</button>
    {#if busy||awaitingPin}<button class="cancel" onclick={stop}>{t('commun.annuler')}</button>{/if}
    <a {href}>{t('airzone_health.inspect')}<Icon name="chevron-right" size={16}/></a>
    {#if href!=='#/airzone/installation'}<a href="#/airzone/installation">{t('airzone_ui.installation')}</a>{/if}
  </div>
  {#if failure}<p class="failure" role={failure==='cancelled'?'status':'alert'}>{t(`airzone_health.check.errors.${failure}`)}</p>{/if}
  {#if result}
    <div class="result" role="status" aria-live="polite">
      <header><strong><Icon name="check" size={17}/>{t('airzone_health.check.responding')}</strong><time datetime={new Date(result.checkedAt).toISOString()}>{t('airzone_health.check.checked_at',{time:checkedTime(result.checkedAt)})}</time></header>
      <p class="presence">{t(`airzone_health.check.presence.${result.state}`)}</p>
      <p>{t(`airzone_health.check.next.${diagnosticAdvice(kind,result.state)}`)}</p>
      <p class="retained">{t('airzone_health.check.retained')}</p>
    </div>
  {/if}
</div>
<style>
.alert-diagnosis {min-width:0;margin-top:16px}.actions {display:flex;align-items:center;flex-wrap:wrap;gap:8px 16px}
button,a {min-height:44px;font:inherit;font-size:.875rem;display:inline-flex;align-items:center;justify-content:center;gap:7px;border-radius:11px;touch-action:manipulation}
button {padding:10px 14px;border:1px solid var(--line);background:var(--surface-2);color:var(--ink);cursor:pointer;font-weight:600}button:disabled {opacity:.6;cursor:default}a {color:var(--ink-2);font-weight:550;text-decoration:none}a:hover {text-decoration:underline}
button:focus-visible,a:focus-visible {outline:3px solid var(--cool);outline-offset:3px}.cancel {background:transparent}
.result {padding:16px;margin-top:16px;border-radius:var(--r-sm);background:var(--surface-2);line-height:1.6;font-size:.9375rem}.result header {display:flex;align-items:center;justify-content:space-between;flex-wrap:wrap;gap:8px 16px}.result strong {display:flex;align-items:center;gap:7px;font-weight:650}.result time {font-size:.8125rem;color:var(--ink-2)}.result p {margin-top:10px;overflow-wrap:anywhere}.presence {font-weight:600}.result .retained {color:var(--ink-2);font-size:.875rem}.failure {font-size:.9375rem;line-height:1.5;margin-top:14px;color:var(--alert)}
@media(max-width:440px){.actions {gap:8px 12px}.result {padding:14px}.actions .verify {flex-basis:100%;justify-content:center}}
@media(prefers-reduced-motion:no-preference){button:not(:disabled):active {transform:scale(.98)}}
@media(prefers-contrast:more){button {border-color:var(--ink-2)}.result {border:1px solid var(--ink-2)}}
</style>
