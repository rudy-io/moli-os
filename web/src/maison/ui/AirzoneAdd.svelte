<script>
  import { onDestroy, untrack } from 'svelte';
  import { t } from '../../lib/i18n.svelte.js';
  import { home } from '../lib/home.svelte.js';
  import { gatewayRequest, nextGatewayInstance, probeGateway, addGateway } from '../lib/airzone-onboarding.js';
  import Icon from './Icon.svelte';

  let { drivers = [], connected = true } = $props();
  let draft = $state({instance:untrack(() => nextGatewayInstance(drivers)),host:'',port:'3000',transport:'http',fingerprint:''});
  let advanced = $state(false);
  let pending = $state(null);
  let awaitingPin = $state(false);
  let checked = $state(null);
  let installed = $state(null);
  let failure = $state(null);
  let uncertain = $state(false);
  const busy = $derived(pending !== null || awaitingPin);
  let revision = 0;
  let alive = true;
  let heldRetry = null;

  function settings() {
    return gatewayRequest({instance:draft.instance,host:draft.host,port:draft.port,
      ...(draft.transport === 'https' ? {tls_fingerprint:draft.fingerprint} : {})});
  }
  function edit(field, value) {
    if (busy || !Object.hasOwn(draft,field)) return;
    draft[field] = value;
    revision++;
    checked = null;
    installed = null;
    failure = null;
    uncertain = false;
  }
  function current(intent) {
    const parsed = settings();
    return alive && intent.revision === revision && !parsed.error
      && JSON.stringify(parsed.request) === JSON.stringify(intent.request);
  }
  function refuse(error) {
    failure = {error};
    if (['invalid_instance','invalid_port','invalid_fingerprint','duplicate_instance'].includes(error)) advanced = true;
  }
  function askHuman(intent) {
    if (home.held) {refuse('auth_busy');return;}
    awaitingPin = true;
    const retry = async () => {
      if (!alive || heldRetry !== retry) return;
      heldRetry = null;
      awaitingPin = false;
      if (current(intent)) await perform(intent,true);
    };
    heldRetry = retry;
    home.held = {
      label:t('airzone_add.gateway_title'),reason:'configuration',custom:retry,
      onCancel:() => {
        if (!alive || heldRetry !== retry) return;
        heldRetry = null;
        awaitingPin = false;
        refuse('cancelled');
      },
    };
  }
  async function perform(intent, authenticated = false) {
    if (!current(intent)) return;
    if (!connected) {refuse('connection');return;}
    pending = intent.kind;
    failure = null;
    const result = intent.kind === 'probe' ? await probeGateway(intent.request) : await addGateway(intent.request,intent.mac);
    if (!alive || !current(intent)) return;
    pending = null;
    if (!result.ok) {
      if (result.error === 'auth' && !authenticated) askHuman(intent);
      else {
        failure = result;
        if (result.error === 'duplicate_instance') advanced = true;
        if (intent.kind === 'install') {
          uncertain = ['network','response','server'].includes(result.error);
          if (['identity_changed','invalid_identity','expected_identity_required'].includes(result.error)) checked = null;
        }
      }
      return;
    }
    uncertain = false;
    if (intent.kind === 'probe') checked = {request:intent.request,report:result.data,revision};
    else {
      installed = result.data;
      checked = null;
    }
  }
  async function verify(event) {
    event?.preventDefault();
    if (busy) return;
    if (!connected) {refuse('connection');return;}
    const parsed = settings();
    if (parsed.error) {refuse(parsed.error);return;}
    checked = null;
    uncertain = false;
    await perform({kind:'probe',request:parsed.request,revision});
  }
  async function install() {
    if (busy || installed) return;
    if (!checked || !current(checked)) {refuse('verify_first');return;}
    await perform({kind:'install',request:checked.request,mac:checked.report.mac,revision});
  }
  function restart() {
    if (busy) return;
    const previous = installed?.instance;
    draft = {instance:nextGatewayInstance([...drivers,...(previous ? [{instance:previous}] : [])]),host:'',port:'3000',transport:'http',fingerprint:''};
    revision++;
    checked = null;
    installed = null;
    failure = null;
    uncertain = false;
  }
  onDestroy(() => {
    alive = false;
    if (heldRetry && home.held?.custom === heldRetry) home.held = null;
    heldRetry = null;
  });
</script>

<div class="add-stack">
  <section class="gateway" aria-labelledby="gateway-heading">
    <header><span class="step"><Icon name="air-conditioner" size={26} /></span><div><h2 id="gateway-heading">{t('airzone_add.gateway_title')}</h2><p>{t('airzone_add.gateway_help')}</p></div></header>
    <p class="read-only"><Icon name="lock" size={16} />{t('airzone_add.read_only')}</p>
    {#if !connected}<p class="notice" role="status">{t('airzone_add.error.connection')}</p>{/if}
    {#if installed}
      <div class="confirmed" role="status">
        <h3>{t(installed.already_installed ? 'airzone_add.already_saved' : 'airzone_add.saved')}</h3>
        <p>{t('airzone_add.saved_help',{instance:installed.instance})}</p>
        <div class="links"><a href="#/airzone">{t('airzone_add.see_zones')}<Icon name="chevron-right" size={18} /></a><a href="#/airzone/installation">{t('airzone_add.installation')}</a></div>
      </div>
      <button type="button" class="secondary" onclick={restart}>{t('airzone_add.another')}</button>
    {:else}
      <form onsubmit={verify} aria-busy={busy} novalidate>
        <label class="address">{t('airzone_add.host')}<input value={draft.host} oninput={(event)=>edit('host',event.currentTarget.value)} autocomplete="off" inputmode="decimal" maxlength="15" required disabled={busy || !connected} placeholder="192.168.1.x" aria-describedby="gateway-host-help" aria-invalid={failure?.error === 'invalid_host'} /></label>
        <p class="field-help" id="gateway-host-help">{t('airzone_add.host_help')}</p>
        <details bind:open={advanced}><summary>{t('airzone_add.advanced')}</summary>
          <div class="advanced-fields">
            <label>{t('airzone_add.id')}<input aria-invalid={failure?.error === 'invalid_instance' || failure?.error === 'duplicate_instance'} value={draft.instance} oninput={(event)=>edit('instance',event.currentTarget.value)} autocomplete="off" maxlength="32" required disabled={busy || !connected} /></label>
            <label>{t('airzone_add.port')}<input aria-invalid={failure?.error === 'invalid_port'} value={draft.port} oninput={(event)=>edit('port',event.currentTarget.value)} inputmode="numeric" maxlength="5" required disabled={busy || !connected} /></label>
            <label>{t('airzone_add.transport')}<select value={draft.transport} onchange={(event)=>edit('transport',event.currentTarget.value)} disabled={busy || !connected}><option value="http">{t('airzone_add.http')}</option><option value="https">{t('airzone_add.https')}</option></select></label>
          </div>
          <p class="field-help">{t('airzone_add.id_help')}</p>
          {#if draft.transport === 'https'}
            <label>{t('airzone_add.fingerprint')}<input aria-invalid={failure?.error === 'invalid_fingerprint'} value={draft.fingerprint} oninput={(event)=>edit('fingerprint',event.currentTarget.value)} autocomplete="off" maxlength="128" required disabled={busy || !connected} /></label>
            <p class="field-help">{t('airzone_add.fingerprint_help')}</p>
          {/if}
        </details>
        <button type="submit" class:secondary={checked} disabled={busy || !connected}>{t(pending === 'probe' ? 'airzone_add.checking' : checked ? 'airzone_add.check_again' : 'airzone_add.verify')}</button>
      </form>
      {#if awaitingPin}<p class="notice" role="status">{t('airzone_add.awaiting_pin')}</p>{/if}
      {#if failure}<div class="error" role="alert"><p>{t(`airzone_add.error.${failure.error}`)}</p>{#if failure.existingInstance}<p>{t('airzone_add.existing',{instance:failure.existingInstance})}</p><a href="#/airzone/installation">{t('airzone_add.installation')}<Icon name="chevron-right" size={18} /></a>{/if}</div>{/if}
      {#if checked}
        <div class="preview" aria-label={t('airzone_add.preview')}>
          <div class="preview-heading" role="status"><h3>{t('airzone_add.verified')}</h3><span>{t('airzone_add.read_only_short')}</span></div>
          <dl>
            <div><dt>{t('airzone_add.model')}</dt><dd>{checked.report.gateway_type ? t(`airzone_add.model.${checked.report.gateway_type}`) : t('airzone_add.not_reported')}</dd></div>
            <div><dt>{t('airzone_add.firmware')}</dt><dd>{checked.report.firmware || t('airzone_add.not_reported')}</dd></div>
            <div><dt>{t('airzone_add.systems')}</dt><dd>{checked.report.systems}</dd></div>
            <div><dt>{t('airzone_add.zones')}</dt><dd>{checked.report.zones}</dd></div>
          </dl>
          {#if checked.report.incomplete}<p class="notice">{t('airzone_add.incomplete')}</p>{:else if checked.report.zones === 0}<p class="field-help">{t('airzone_add.empty_gateway')}</p>{/if}
          {#if uncertain}<p class="notice" role="status">{t('airzone_add.uncertain')}</p>{/if}
          <button type="button" disabled={busy || !connected} onclick={install}>{t(pending === 'install' ? 'airzone_add.saving' : uncertain ? 'airzone_add.retry_install' : 'airzone_add.install')}</button>
          <p class="field-help">{t('airzone_add.install_help')}</p>
        </div>
      {/if}
    {/if}
  </section>
  <section class="discovery">
    <h2>{t('airzone_add.zone_title')}</h2><p>{t('airzone_add.zone_help')}</p>
    <ol><li>{t('airzone_add.zone_step1')}</li><li>{t('airzone_add.zone_step2')}</li><li>{t('airzone_add.zone_step3')}</li></ol>
    <a href="#/airzone">{t('airzone_add.see_zones')}<Icon name="chevron-right" size={18} /></a>
  </section>
  <section class="restore"><h2>{t('airzone_add.restore_title')}</h2><p>{t('airzone_add.restore_help')}</p><a href="#/airzone/devices">{t('airzone_add.manage')}<Icon name="chevron-right" size={18} /></a></section>
</div>

<style>
  .add-stack {display:grid; gap:20px; max-width:850px;}
  section {background:var(--surface); border-radius:var(--r-lg); padding:28px; box-shadow:var(--shadow);}
  header {display:flex; gap:14px; align-items:flex-start;}
  header p {margin-bottom:0;}
  .step {display:inline-flex; color:var(--cool); padding-top:3px;}
  h2 {font-size:22px; margin:0 0 10px; letter-spacing:-.025em;}
  h3 {font-size:18px; margin:0; letter-spacing:-.02em;}
  p,li {font-size:15px; line-height:1.65; color:var(--ink-2); max-width:65ch;}
  .read-only {display:flex; gap:8px; align-items:center; font-size:13px; padding:10px 0;}
  ol {padding-left:22px; display:grid; gap:9px;}
  a {display:inline-flex; align-items:center; gap:7px; color:var(--cool); min-height:44px; font-weight:600;}
  label {display:grid; gap:8px; font-size:13px; font-weight:600;}
  input,select {min-width:0; width:100%; background:var(--surface-2); color:var(--ink); border:1px solid var(--line); border-radius:var(--r-sm); padding:12px; min-height:46px; font:inherit;}
  .address {max-width:450px;}
  .field-help {font-size:12px; margin:8px 0 16px; max-width:70ch;}
  summary {cursor:pointer; padding:12px 0; min-height:44px; font-size:14px; font-weight:600; color:var(--ink-2);}
  details {margin:0 0 16px;}
  .advanced-fields {display:grid; grid-template-columns:minmax(0,1fr) 95px minmax(0,1fr); gap:14px; margin:8px 0 0;}
  button {border:0; background:var(--cool); color:var(--paper); border-radius:var(--r-sm); min-height:46px; padding:12px 18px; cursor:pointer; font:inherit; font-size:14px; font-weight:650; transition:filter 150ms,transform 150ms;}
  button:hover:not(:disabled) {filter:brightness(1.08);}
  button:active:not(:disabled) {transform:scale(.98);}
  button.secondary {background:var(--surface-2);color:var(--ink);border:1px solid var(--line);}
  button:disabled,input:disabled,select:disabled {opacity:.55;cursor:not-allowed;}
  :is(input,select,button,a,summary):focus-visible {outline:3px solid var(--cool);outline-offset:3px;}
  .preview,.confirmed {padding:22px;margin-top:24px;border:1px solid var(--line);border-radius:var(--r-md);background:var(--surface-2);}
  .preview-heading {display:flex;gap:12px;align-items:center;justify-content:space-between;flex-wrap:wrap;}
  .preview-heading span {font-size:12px;color:var(--ink-2);}
  dl {display:grid;grid-template-columns:repeat(2,minmax(0,1fr));gap:18px;margin:24px 0;}
  dt {font-size:12px;color:var(--ink-2);margin-bottom:6px;}
  dd {margin:0;font-size:18px;font-weight:600;overflow-wrap:anywhere;}
  .preview .field-help {margin-bottom:0;}
  .notice,.error {font-size:13px;padding:12px 14px;background:var(--surface-2);border-radius:var(--r-sm);border-left:3px solid var(--warm);margin-top:16px;}
  .error {border-left-color:var(--alert);}
  .error p {margin:0;font-size:13px;overflow-wrap:anywhere;}
  .error p+p {margin-top:8px;}
  .links {display:flex;gap:22px;flex-wrap:wrap;}
  .confirmed {margin:0 0 18px;}
  .discovery h2,.restore h2 {font-size:18px;}
  .discovery p,.discovery li,.restore p {font-size:14px;}
  @media(max-width:600px) {section {padding:20px;} h2 {font-size:20px;}.advanced-fields {grid-template-columns:minmax(0,1fr);}.preview,.confirmed {padding:18px;} .address {max-width:none;} button {max-width:100%;}}
  @media(prefers-reduced-motion:reduce) {button {transition:none;}button:active:not(:disabled) {transform:none;}}
</style>
