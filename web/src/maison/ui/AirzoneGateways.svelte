<script>
  import { onMount, onDestroy, untrack } from 'svelte';
  import { t } from '../../lib/i18n.svelte.js';
  import { home } from '../lib/home.svelte.js';
  import { listGateways, withdrawGateway } from '../lib/airzone-onboarding.js';
  import { gatewayApproval } from '../lib/gateway-approval.js';
  import AirzoneGatewayCard from './AirzoneGatewayCard.svelte';
  import AirzoneAdd from './AirzoneAdd.svelte';
  import Icon from './Icon.svelte';

  let { connected = true, drivers = [], registryRevision = 0 } = $props();
  let gateways = $state(null);
  let pending = $state(null);
  let awaitingPin = $state(false);
  let failure = $state(null);
  let editing = $state(null);
  let creating = $state(false);
  let confirmation = $state(null);
  let replacement = $state(null);
  let withdrawn = $state(null);
  let uncertain = $state(false);
  const busy = $derived(pending !== null || awaitingPin);
  let alive = true;
  let revision = 0;
  let refreshNeeded = $state(false);
  let seenRegistry = untrack(() => registryRevision);
  let seenConnected = untrack(() => connected);
  // Coalesce lifecycle events while an editor, confirmation or PIN owns the UI.
  $effect(() => {
    const next = registryRevision;
    const online = connected;
    if (next !== seenRegistry || (online && !seenConnected)) refreshNeeded = true;
    seenRegistry = next;
    seenConnected = online;
  });
  $effect(() => {
    if (refreshNeeded && connected && !busy && !editing && !creating && !confirmation && !home.held) {
      untrack(() => {load(true);});
    }
  });
  const approval = gatewayApproval({home,label:() => t('airzone_gateways.title'),waiting:(value) => {awaitingPin=value;},failed:(error) => {failure={error};},current,perform});

  function current(intent) {return alive && intent.revision === revision;}
  async function perform(intent, authenticated = false) {
    if (!current(intent)) return;
    if (!connected) {failure={error:'connection'};return;}
    pending = intent.kind;
    failure = null;
    const result = intent.kind === 'list' ? await listGateways() : await withdrawGateway(intent.instance);
    if (!current(intent)) return;
    pending = null;
    if (!result.ok) {
      if (result.error === 'auth' && !authenticated && !intent.automatic) approval.ask(intent);
      else {
        failure=result;
        uncertain=intent.kind !== 'list' && ['network','response','server'].includes(result.error);
      }
      return;
    }
    uncertain=false;
    if (intent.kind === 'list') gateways=result.data.gateways;
    else {
      gateways=gateways.map((item) => item.instance === intent.instance ? {...item,enabled:false,running:false,read_only:result.data.read_only} : item);
      confirmation=null;
      withdrawn=intent.instance;
      if (intent.replacing) replacement=intent.instance;
    }
  }
  async function load(automatic = false) {
    if (busy || editing || creating || confirmation) return;
    refreshNeeded=false;
    await perform({kind:'list',revision,automatic});
  }
  function choose(instance,kind) {
    if (busy || editing || creating || confirmation || !connected) return;
    const gateway=gateways?.find((item) => item.instance === instance);
    if (!gateway || !['edit','withdraw','replace'].includes(kind)) return;
    revision++;
    failure=null;
    uncertain=false;
    withdrawn=null;
    replacement=null;
    if (kind === 'edit') {editing={...gateway};return;}
    // A disabled TOML entry has no withdrawal override yet. Persist it even
    // when the last list said paused; another tab may also have reactivated it.
    if (kind === 'replace' || gateway.enabled) confirmation={instance,kind};
  }
  async function confirmWithdrawal() {
    if (busy || !confirmation) return;
    await perform({kind:'withdraw',instance:confirmation.instance,replacing:confirmation.kind === 'replace',revision});
  }
  function cancelSelection() {
    if (busy) return;
    revision++;
    confirmation=null;
    uncertain=false;
    failure=null;
  }
  function startReplacement() {
    if (busy || editing || creating || !connected || !replacement || confirmation) return;
    if (!gateways?.some((item) => item.instance === replacement && !item.enabled)) return;
    creating=true;
    revision++;
  }
  async function closeEditor() {
    editing=null;
    creating=false;
    revision++;
    await load();
  }
  onMount(() => {load();});
  onDestroy(() => {alive=false;approval.destroy();});
</script>

<section class="gateways" aria-labelledby="gateways-title">
  <header><div><h2 id="gateways-title">{t('airzone_gateways.title')}</h2><p>{t('airzone_gateways.help')}</p></div><a href="#/airzone/add"><Icon name="plus" size={18} />{t('airzone_gateways.add')}</a></header>
  {#if !connected}<p class="message" role="status">{t('airzone_add.error.connection')}</p>{/if}
  {#if editing || creating}
    <button class="back" type="button" onclick={closeEditor}><Icon name="chevron-left" size={18} />{t('airzone_gateways.back')}</button>
    {#key editing?.instance ?? 'new'}<AirzoneAdd gateway={editing} {connected} drivers={[...drivers,...(gateways ?? [])]} onclose={closeEditor} />{/key}
  {:else}
    <div class="toolbar"><button class="refresh" type="button" disabled={busy || !connected || confirmation !== null} onclick={() => load()}>{t(pending === 'list' ? 'airzone_gateways.loading' : 'airzone_gateways.refresh')}</button></div>
    {#if awaitingPin}<p class="message" role="status">{t('airzone_add.awaiting_pin')}</p>{/if}
    {#if failure}<p class="message error" role="alert">{t(`airzone_add.error.${failure.error}`)}</p>{/if}
    {#if gateways?.length === 0}<p class="empty">{t('airzone_gateways.empty')}</p>{/if}
    {#if gateways}
      <div class="gateway-list" aria-busy={busy}>
        {#each gateways as gateway (gateway.instance)}
          <AirzoneGatewayCard {gateway} {connected} busy={busy || confirmation !== null} onchoose={(kind) => choose(gateway.instance,kind)} />
        {/each}
      </div>
    {/if}
    {#if confirmation}
      <div class="confirmation" aria-labelledby="withdraw-title">
        <h3 id="withdraw-title">{t(confirmation.kind === 'replace' ? 'airzone_gateways.replace_title' : 'airzone_gateways.withdraw_title',{instance:confirmation.instance})}</h3>
        <p>{t('airzone_gateways.withdraw_help')}</p>
        {#if confirmation.kind === 'replace'}<p>{t('airzone_gateways.replace_help')}</p>{/if}
        {#if uncertain}<p role="status">{t('airzone_gateways.uncertain')}</p>{/if}
        <div class="actions"><button class="primary" type="button" disabled={busy || !connected} onclick={confirmWithdrawal}>{t(pending === 'withdraw' ? 'airzone_gateways.withdrawing' : uncertain ? 'airzone_gateways.retry_withdraw' : confirmation.kind === 'replace' ? 'airzone_gateways.withdraw_first' : 'airzone_gateways.withdraw_confirm')}</button><button type="button" disabled={busy} onclick={cancelSelection}>{t('airzone_gateways.cancel')}</button></div>
      </div>
    {/if}
    {#if replacement}
      <div class="confirmation" role="status"><h3>{t('airzone_gateways.replacement_ready')}</h3><p>{t('airzone_gateways.replacement_help',{instance:replacement})}</p><button class="next primary" type="button" disabled={busy || !connected} onclick={startReplacement}>{t('airzone_gateways.add_replacement')}<Icon name="chevron-right" size={18} /></button></div>
    {:else if withdrawn}
      <p class="message" role="status">{t('airzone_gateways.withdrawn',{instance:withdrawn})}</p>
    {/if}
  {/if}
</section>

<style>
  .gateways {min-width:0;background:var(--surface);border-radius:var(--r-lg);padding:26px;box-shadow:var(--shadow);}
  header {display:flex;gap:20px;justify-content:space-between;align-items:flex-start;}
  h2 {font-size:22px;letter-spacing:-.025em;margin:0 0 8px;}
  h3 {font-size:18px;letter-spacing:-.015em;margin:0;overflow-wrap:anywhere;}
  p {font-size:14px;line-height:1.6;color:var(--ink-2);max-width:65ch;}
  header p {margin:0;}
  a,button {min-height:44px;display:inline-flex;align-items:center;justify-content:center;gap:8px;font:inherit;font-size:14px;font-weight:600;cursor:pointer;}
  a {color:var(--cool);text-decoration:none;}
  header a {flex:none;}
  button {border:1px solid var(--line);border-radius:var(--r-sm);padding:10px 14px;background:var(--surface-2);color:var(--ink);transition:background 150ms;}
  .primary {background:var(--cool);color:var(--paper);border-color:transparent;}
  button:disabled {opacity:.55;cursor:not-allowed;}
  .toolbar {display:flex;justify-content:flex-end;margin:16px 0;}
  .gateway-list {display:grid;gap:12px;}
  .message,.confirmation {padding:18px;border:1px solid var(--line);border-radius:var(--r-md);background:var(--surface-2);margin:18px 0 0;}
  .message {font-size:14px;overflow-wrap:anywhere;}
  .error {color:var(--alert);}
  .actions {display:flex;flex-wrap:wrap;gap:10px;}
  .back {margin:20px 0;background:transparent;}
  .next {justify-content:flex-start;}
  :is(button,a):focus-visible {outline:3px solid var(--cool);outline-offset:3px;}
  @media(hover:hover) {button:not(:disabled):hover {filter:brightness(1.06);}a:hover {text-decoration:underline;}}
  @media(max-width:600px) {.gateways {padding:20px;}header {flex-direction:column;gap:8px;}h2 {font-size:20px;}.toolbar {justify-content:flex-start;}.actions button {width:100%;}}
  @media(prefers-reduced-motion:reduce) {button {transition:none;}}
</style>
