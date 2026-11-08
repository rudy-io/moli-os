<script>
  import { onDestroy, untrack, tick } from 'svelte';
  import { t } from '../../lib/i18n.svelte.js';
  import { home } from '../lib/home.svelte.js';
  import { gatewayRequest, nextGatewayInstance, probeGateway, addGateway, probeExistingGateway, updateGateway, discoverGateways } from '../lib/airzone-onboarding.js';
  import { gatewayApproval } from '../lib/gateway-approval.js';
  import Icon from './Icon.svelte';
  import AirzoneDiscoveryResults from './AirzoneDiscoveryResults.svelte';

  let { drivers = [], connected = true, gateway = null, onclose = () => {}, onsaved = () => {} } = $props();
  // The parent keys this editor by instance; a live list refresh cannot retarget a draft.
  const original = untrack(() => gateway ? {...gateway} : null);
  const editing = original !== null;
  let draft = $state(original ? {instance:original.instance,host:original.host,port:String(original.port),transport:original.tls_fingerprint ? 'https' : 'http',fingerprint:original.tls_fingerprint ?? ''} : {instance:untrack(() => nextGatewayInstance(drivers)),host:'',port:'3000',transport:'http',fingerprint:''});
  let stage = $state(editing ? 'manual' : 'search');
  let results = $state(null);
  let selectedMac = null;
  let fromDiscovery = false;
  let reading = null;
  let stepHeading = $state(null);
  const step = $derived(installed ? 4 : checked ? 3 : stage === 'choose' || stage === 'manual' ? 2 : 1);
  const title = $derived(installed ? editing ? 'airzone_gateways.updated' : 'airzone_add.saved' : checked ? editing ? 'airzone_wizard.confirm_edit_title' : 'airzone_wizard.confirm_title' : editing ? original.enabled ? 'airzone_gateways.edit_title' : 'airzone_gateways.resume_title' : stage === 'manual' ? 'airzone_wizard.manual_title' : stage === 'choose' ? 'airzone_wizard.choose_title' : 'airzone_wizard.search_title');
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
  const approval = gatewayApproval({home,label:(intent) => t(intent.kind === 'scan' ? 'airzone_wizard.search_button' : editing ? 'airzone_gateways.edit' : 'airzone_add.gateway_title'),reason:(intent) => intent.kind === 'scan' ? 'discovery' : 'configuration',waiting:(value) => {awaitingPin=value;},failed:refuse,current,perform});

  async function focusStep() {
    const at = revision;
    await tick();
    if (alive && at === revision) stepHeading?.focus();
  }
  function cancelReading() {
    if (pending === 'install') return;
    revision++;
    reading?.abort();
    reading = null;
    approval.cancel();
    pending = null;
    checked = null;
    failure = null;
    uncertain = false;
  }
  function manual() {
    if (pending === 'install') return;
    cancelReading();
    selectedMac = null;
    fromDiscovery = false;
    stage = 'manual';
    focusStep();
  }
  function back() {
    if (busy || installed) return;
    const wasChecked = checked !== null;
    cancelReading();
    stage = wasChecked ? fromDiscovery ? 'choose' : 'manual' : 'search';
    focusStep();
  }
  async function search() {
    if (busy || editing || installed) return;
    cancelReading();
    stage = 'search';
    results = null;
    selectedMac = null;
    await perform({kind:'scan',revision});
  }
  function stopSearch() {
    cancelReading();
    stage = 'search';
    focusStep();
  }
  async function choose(candidate) {
    if (busy || editing || installed || candidate?.existing_instance || !results?.gateways.includes(candidate)) return;
    revision++;
    draft = {instance:nextGatewayInstance(drivers),host:candidate.host,port:String(candidate.port),transport:candidate.tls_fingerprint ? 'https' : 'http',fingerprint:candidate.tls_fingerprint ?? ''};
    selectedMac = candidate.mac;
    fromDiscovery = true;
    failure = null;
    await verify();
  }
  function settings() {
    return gatewayRequest({instance:draft.instance,host:draft.host,port:draft.port,
      ...(draft.transport === 'https' ? {tls_fingerprint:draft.fingerprint} : {})},{existing:editing});
  }
  function edit(field, value) {
    if (busy || !Object.hasOwn(draft,field) || (editing && field === 'instance')) return;
    draft[field] = value;
    selectedMac = null;
    fromDiscovery = false;
    stage = 'manual';
    revision++;
    checked = null;
    installed = null;
    failure = null;
    uncertain = false;
  }
  function current(intent) {
    if (intent.kind === 'scan') return alive && !editing && stage === 'search' && intent.revision === revision;
    const parsed = settings();
    return alive && intent.revision === revision && !parsed.error
      && JSON.stringify(parsed.request) === JSON.stringify(intent.request);
  }
  function refuse(error) {
    failure = {error};
    if (['invalid_instance','invalid_port','invalid_fingerprint','duplicate_instance'].includes(error)) advanced = true;
  }
  async function perform(intent, authenticated = false) {
    if (!current(intent)) return;
    if (!connected) {refuse('connection');return;}
    pending = intent.kind;
    failure = null;
    if (intent.kind !== 'install') reading = new AbortController();
    const result = intent.kind === 'scan'
      ? await discoverGateways({signal:reading.signal})
      : intent.kind === 'probe'
        ? await (editing ? probeExistingGateway : probeGateway)(intent.request,globalThis.fetch,{signal:reading.signal})
        : await (editing ? updateGateway : addGateway)(intent.request,intent.mac);
    if (!alive || !current(intent)) return;
    pending = null;
    reading = null;
    if (!result.ok) {
      if (result.error === 'auth' && !authenticated) approval.ask(intent);
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
    if (intent.kind === 'scan') {
      results = result.data;
      stage = 'choose';
      focusStep();
    }
    else if (intent.kind === 'probe') {
      if ((original?.mac && original.mac !== result.data.mac) || (selectedMac && selectedMac !== result.data.mac)) {refuse('identity_changed');return;}
      checked = {request:intent.request,report:result.data,revision};
      focusStep();
    }
    else {
      installed = result.data;
      checked = null;
      onsaved(result.data);
      focusStep();
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
    if (busy || editing) return;
    const previous = installed?.instance;
    stage = 'search';
    results = null;
    selectedMac = null;
    fromDiscovery = false;
    draft = {instance:nextGatewayInstance([...drivers,...(previous ? [{instance:previous}] : [])]),host:'',port:'3000',transport:'http',fingerprint:''};
    revision++;
    checked = null;
    installed = null;
    failure = null;
    uncertain = false;
    focusStep();
  }
  onDestroy(() => {
    alive = false;
    reading?.abort();
    approval.destroy();
  });
</script>

<div class="add-stack">
  <section class="gateway" aria-labelledby="gateway-heading">
    {#if !editing}
      <ol class="steps" aria-label={t('airzone_wizard.steps')}>
        {#each ['search','choose','add'] as name,index}
          <li aria-current={step === index+1 ? 'step' : undefined} class:done={step > index+1}><span class="step-number">{step > index+1 ? '✓' : index+1}</span><span>{t(`airzone_wizard.step_${name}`)}</span></li>
        {/each}
      </ol>
    {/if}
    <header><span class="hero-icon"><Icon name={installed ? 'ok' : checked ? 'air-conditioner' : stage === 'search' ? 'search' : 'lan'} size={30} /></span><h2 id="gateway-heading" tabindex="-1" bind:this={stepHeading}>{t(title)}</h2>
      <p>{t(installed ? 'airzone_wizard.saved_help' : checked ? 'airzone_wizard.confirm_help' : editing ? 'airzone_gateways.edit_help' : stage === 'manual' ? 'airzone_wizard.manual_help' : stage === 'choose' ? 'airzone_wizard.choose_help' : 'airzone_wizard.search_help')}</p>
    </header>
    {#if editing && !installed}<p class="read-only"><Icon name="lock" size={16} />{t('airzone_gateways.save_read_only')}</p>{/if}
    {#if !connected}<p class="notice" role="status">{t('airzone_add.error.connection')}</p>{/if}
    {#if installed}
      <div class="confirmed" role="status"><p><Icon name={installed.read_only ? 'lock' : 'tune'} size={16} />{t(installed.read_only ? 'airzone_wizard.observation' : 'airzone_gateways.existing_control_kept')}</p><a class="primary-link" href="#/airzone">{t('airzone_wizard.see_zones')}<Icon name="chevron-right" size={18} /></a>{#if !installed.read_only}<a class="mode-link" href="#/airzone/installation">{t('airzone_gateways.manage')}</a>{/if}</div>
      {#if editing}<button type="button" class="secondary" onclick={onclose}>{t('airzone_gateways.back')}</button>{:else}<button type="button" class="quiet" onclick={restart}>{t('airzone_add.another')}</button>{/if}
    {:else}
      {#if checked}
        <div class="preview" aria-label={t('airzone_add.preview')}>
          <div class="preview-identity"><Icon name="air-conditioner" size={26} /><div><h3>{checked.report.gateway_type ? t(`airzone_add.model.${checked.report.gateway_type}`) : t('airzone_wizard.gateway')}</h3><p>{t('airzone_wizard.zone_count',{count:checked.report.zones})}</p></div></div>
          <p class="read-only"><Icon name="lock" size={16} />{t('airzone_wizard.observation')}</p>
          <details><summary>{t('airzone_wizard.connection_details')}</summary><dl><div><dt>{t('airzone_add.host')}</dt><dd>{checked.request.host}:{checked.request.port}</dd></div><div><dt>{t('airzone_add.id')}</dt><dd>{checked.request.instance}</dd></div><div><dt>{t('airzone_add.firmware')}</dt><dd>{checked.report.firmware || t('airzone_add.not_reported')}</dd></div><div><dt>{t('airzone_add.systems')}</dt><dd>{checked.report.systems}</dd></div></dl></details>
          {#if checked.report.incomplete}<p class="notice">{t('airzone_add.incomplete')}</p>{:else if checked.report.zones === 0}<p class="field-help">{t('airzone_add.empty_gateway')}</p>{/if}
          {#if uncertain}<p class="notice" role="status">{t('airzone_add.uncertain')}</p>{/if}
          <button type="button" class="primary" disabled={busy || !connected} onclick={install}>{t(pending === 'install' ? 'airzone_add.saving' : uncertain ? 'airzone_add.retry_install' : editing ? original.enabled ? 'airzone_gateways.save' : 'airzone_gateways.resume_save' : 'airzone_wizard.add_button')}</button>
        </div>
      {:else if stage === 'manual'}
      <form onsubmit={verify} aria-busy={busy} novalidate>
        <label class="address">{t('airzone_add.host')}<input value={draft.host} oninput={(event)=>edit('host',event.currentTarget.value)} autocomplete="off" inputmode="decimal" maxlength="15" required disabled={busy || !connected} placeholder="192.168.1.x" aria-describedby="gateway-host-help" aria-invalid={failure?.error === 'invalid_host'} /></label>
        <p class="field-help" id="gateway-host-help">{t('airzone_add.host_help')}</p>
        <details bind:open={advanced}><summary>{t('airzone_add.advanced')}</summary>
          <div class="advanced-fields">
            <label>{t('airzone_add.id')}<input aria-invalid={failure?.error === 'invalid_instance' || failure?.error === 'duplicate_instance'} value={draft.instance} oninput={(event)=>edit('instance',event.currentTarget.value)} autocomplete="off" maxlength="32" required disabled={editing || busy || !connected} /></label>
            <label>{t('airzone_add.port')}<input aria-invalid={failure?.error === 'invalid_port'} value={draft.port} oninput={(event)=>edit('port',event.currentTarget.value)} inputmode="numeric" maxlength="5" required disabled={busy || !connected} /></label>
            <label>{t('airzone_add.transport')}<select value={draft.transport} onchange={(event)=>edit('transport',event.currentTarget.value)} disabled={busy || !connected}><option value="http">{t('airzone_add.http')}</option><option value="https">{t('airzone_add.https')}</option></select></label>
          </div>
          <p class="field-help">{t(editing ? 'airzone_gateways.stable_id' : 'airzone_add.id_help')}</p>
          {#if draft.transport === 'https'}
            <label>{t('airzone_add.fingerprint')}<input aria-invalid={failure?.error === 'invalid_fingerprint'} value={draft.fingerprint} oninput={(event)=>edit('fingerprint',event.currentTarget.value)} autocomplete="off" maxlength="128" required disabled={busy || !connected} /></label>
            <p class="field-help">{t('airzone_add.fingerprint_help')}</p>
          {/if}
        </details>
        <button type="submit" disabled={busy || !connected}>{t(pending === 'probe' ? 'airzone_add.checking' : 'airzone_wizard.continue')}</button>
      </form>

      {:else if stage === 'choose'}
        <AirzoneDiscoveryResults report={results} {busy} {connected} onchoose={choose} />
        <div class="secondary-actions"><button type="button" class="secondary" disabled={busy || !connected} onclick={search}>{t('airzone_wizard.search_again')}</button><button type="button" class="quiet" disabled={busy} onclick={manual}>{t('airzone_wizard.manual_button')}</button></div>
      {:else}
        {#if pending === 'scan'}
          <div class="search-status" role="status"><span class="spinner" aria-hidden="true"></span><p>{t('airzone_wizard.searching')}</p><p class="field-help">{t('airzone_wizard.search_wait')}</p></div>
          <button type="button" class="secondary" onclick={stopSearch}>{t('airzone_wizard.cancel_search')}</button>
        {:else}
          <button type="button" class="primary" disabled={busy || !connected} onclick={search}>{t('airzone_wizard.search_button')}<Icon name="chevron-right" size={18} /></button>
          <button type="button" class="quiet" disabled={busy} onclick={manual}>{t('airzone_wizard.manual_button')}</button>
        {/if}
      {/if}
      {#if pending === 'probe'}<p class="notice" role="status">{t('airzone_add.checking')}</p>{/if}
      {#if awaitingPin}<p class="notice" role="status">{t('airzone_add.awaiting_pin')}</p>{/if}
      {#if failure}<div class="error" role="alert"><p>{t(failure.error === 'busy' && stage === 'search' ? 'airzone_wizard.scan_busy' : `airzone_add.error.${failure.error}`)}</p>{#if failure.existingInstance}<p>{t('airzone_add.existing',{instance:failure.existingInstance})}</p><a href="#/airzone/installation">{t('airzone_add.installation')}<Icon name="chevron-right" size={18} /></a>{/if}</div>{/if}
      {#if checked || (stage !== 'search' && !editing)}<button class="back quiet" type="button" disabled={busy} onclick={back}><Icon name="chevron-left" size={18} />{t('airzone_wizard.back')}</button>{/if}
      {#if stage === 'search' || stage === 'choose'}<p class="reassurance"><Icon name="lock" size={15} />{t('airzone_wizard.scan_help')}</p>{/if}
    {/if}
  </section>
  {#if !editing}
    <details class="help"><summary>{t('airzone_wizard.help')}</summary><div class="help-content"><h3>{t('airzone_add.zone_title')}</h3><p>{t('airzone_add.zone_help')}</p><ol><li>{t('airzone_add.zone_step1')}</li><li>{t('airzone_add.zone_step2')}</li><li>{t('airzone_add.zone_step3')}</li></ol><h3>{t('airzone_add.restore_title')}</h3><p>{t('airzone_add.restore_help')}</p><a href="#/airzone/devices">{t('airzone_add.manage')}<Icon name="chevron-right" size={18} /></a></div></details>
  {/if}
</div>

<style>
  .add-stack {display:grid;gap:18px;max-width:720px;margin:0 auto;}
  .gateway {background:var(--surface);border:1px solid var(--line);border-radius:var(--r-lg);padding:32px;box-shadow:var(--shadow);}
  header {margin:26px 0;text-align:center;}
  .hero-icon {display:inline-grid;place-items:center;width:64px;height:64px;border-radius:20px;background:var(--surface-3);color:var(--cool);margin-bottom:20px;}
  h2 {font-size:clamp(1.4rem,3vw,1.75rem);line-height:1.2;letter-spacing:-.03em;margin:0 0 12px;}
  h2:focus {outline:none;}
  h3 {font-size:1.1rem;letter-spacing:-.015em;margin:0;overflow-wrap:anywhere;}
  p,li {font-size:1rem;line-height:1.55;color:var(--ink-2);}
  header p {max-width:44ch;margin:0 auto;}
  .steps {display:flex;gap:12px;justify-content:space-between;list-style:none;margin:0;padding:0 0 22px;border-bottom:1px solid var(--line);}
  .steps li {display:flex;align-items:center;gap:8px;font-size:.8125rem;color:var(--ink-2);}
  .step-number {display:inline-grid;place-items:center;width:26px;height:26px;border-radius:50%;border:1px solid var(--line);font-size:.75rem;flex:none;}
  .steps li[aria-current] {color:var(--ink);font-weight:650;}
  [aria-current] .step-number {background:var(--cool);border-color:var(--cool);color:var(--paper);}
  .done .step-number {background:var(--surface-3);color:var(--cool);}
  button,.primary-link {display:inline-flex;gap:8px;align-items:center;justify-content:center;min-height:48px;padding:12px 18px;border:1px solid transparent;border-radius:var(--r-sm);font:inherit;font-size:.9375rem;font-weight:650;cursor:pointer;background:var(--cool);color:var(--paper);transition:filter 150ms,transform 150ms;}
  .primary,.primary-link {width:100%;}
  button.secondary {color:var(--ink);background:var(--surface-2);border-color:var(--line);}
  button.quiet {display:flex;background:transparent;color:var(--ink-2);margin:8px auto 0;padding:10px 12px;font-weight:500;}
  button:hover:not(:disabled),.primary-link:hover {filter:brightness(1.06);}
  button:active:not(:disabled) {transform:scale(.985);}
  button:disabled,input:disabled,select:disabled {opacity:.55;cursor:not-allowed;}
  :is(button,a,input,select,summary):focus-visible {outline:3px solid var(--cool);outline-offset:3px;}
  a {display:inline-flex;align-items:center;gap:7px;color:var(--cool);min-height:44px;font-weight:600;}
  a.primary-link {color:var(--paper);text-decoration:none;}
  .reassurance,.read-only {display:flex;align-items:center;justify-content:center;gap:7px;font-size:.8125rem;line-height:1.5;}
  .reassurance {margin:20px 0 0;text-align:center;}
  label {display:grid;gap:8px;font-size:.875rem;font-weight:600;}
  input,select {min-width:0;width:100%;background:var(--surface-2);color:var(--ink);border:1px solid var(--line);border-radius:var(--r-sm);padding:12px;min-height:48px;font:inherit;font-size:1rem;}
  .field-help {font-size:.8125rem;margin:8px 0 16px;line-height:1.55;}
  summary {cursor:pointer;min-height:44px;display:list-item;padding:12px 0;font-size:.875rem;font-weight:600;color:var(--ink-2);}
  details {margin:12px 0 18px;}
  .advanced-fields {display:grid;grid-template-columns:minmax(0,1fr) 95px minmax(0,1fr);gap:14px;margin-top:8px;}
  .preview {border:1px solid var(--line);border-radius:var(--r-md);padding:22px;background:var(--surface-2);}
  .preview-identity {display:flex;align-items:center;gap:14px;color:var(--cool);}
  .preview-identity h3 {color:var(--ink);}.preview-identity p {margin:6px 0 0;}
  .preview .read-only {justify-content:flex-start;}
  dl {display:grid;grid-template-columns:repeat(2,minmax(0,1fr));gap:14px;margin:14px 0;}
  dt {font-size:.75rem;color:var(--ink-2);margin-bottom:5px;}dd {margin:0;font-size:.875rem;overflow-wrap:anywhere;}
  .notice,.error {padding:12px 14px;background:var(--surface-2);border-radius:var(--r-sm);border:1px solid var(--line);margin-top:16px;}
  .notice,.error p {font-size:.875rem;line-height:1.5;}.error {color:var(--alert);}.error p {margin:0;overflow-wrap:anywhere;}.error p+p {margin-top:8px;}
  .confirmed {text-align:center;}.confirmed p {display:flex;gap:8px;justify-content:center;font-size:.875rem;margin:0 0 24px;}
  .confirmed .mode-link {display:flex;justify-content:center;margin-top:10px;}
  .secondary-actions {display:flex;align-items:center;gap:12px;flex-wrap:wrap;margin-top:18px;}.secondary-actions .quiet {margin:0;}
  .search-status {text-align:center;margin:28px 0;}.search-status p {margin:12px 0 0;}.spinner {display:inline-block;width:30px;height:30px;border:3px solid var(--line);border-top-color:var(--cool);border-radius:50%;animation:spin 900ms linear infinite;}
  .search-status+.secondary {display:flex;margin:auto;}
  .help {padding:0 16px;margin:0;}.help-content {padding:10px 0;}.help h3 {font-size:1rem;margin:16px 0 8px;}.help p,.help li {font-size:.875rem;}.help ol {padding-left:22px;display:grid;gap:8px;}
  @keyframes spin {to {transform:rotate(360deg);}}
  @media(max-width:600px) {.gateway {padding:22px 18px;}header {margin:22px 0;}.steps {gap:7px;}.steps li {gap:5px;font-size:.75rem;}.step-number {width:23px;height:23px;}.advanced-fields {grid-template-columns:1fr;}.preview {padding:18px;}dl {grid-template-columns:1fr;}.secondary-actions {flex-direction:column;align-items:stretch;}.secondary-actions button {width:100%;}}
  @media(prefers-reduced-motion:reduce) {button {transition:none;}button:active:not(:disabled) {transform:none;}.spinner {animation:none;border-style:dotted;}}
</style>
