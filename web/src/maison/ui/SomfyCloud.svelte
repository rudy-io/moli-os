<script>
  import { onMount } from 'svelte';
  import { home } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import { createCloudObserver } from '../lib/somfy-cloud.js';
  import SomfyCloudCatalog from './SomfyCloudCatalog.svelte';
  import SomfyCloudStatus from './SomfyCloudStatus.svelte';
  import Icon from './Icon.svelte';

  let view = $state({ loading: true, instances: [], auth: null, selected: '', pending: null, error: '', catalog: null, catalogText: '', details: {}, detailBusy: '', reading: false });
  let observer;
  let held;
  let loginOpen = $state(false);
  let username = $state('');
  let password = $state('');
  let site = $state('');
  let gateway = $state('');
  let region = $state('');
  const row = $derived(view.instances.find(i => i.instance === view.selected));
  const connection = $derived(row?.state);
  const candidate = $derived(connection?.candidates?.find(c => c.site_oid === site));
  const effectiveRegion = $derived(candidate?.region ?? region);

  onMount(() => {
    observer = createCloudObserver(next => { if (next.auth !== 'owner') closeLogin(); view = next; });
    observer.open();
    return () => {
      password = ''; username = '';
      observer.stop();
      if (home.held === held) home.held = null;
      held = null;
    };
  });
  function closeLogin() { password = ''; username = ''; loginOpen = false; }
  function unlock() {
    closeLogin();
    held = { label: t('systeme.somfy.title'), reason: t('systeme.somfy.pin_fallback'), custom: observer.open };
    home.held = held;
  }
  function login(event) {
    event.preventDefault();
    const input = { kind: 'login', username: username.trim(), password };
    closeLogin(); // Clear the input before even awaiting the local dispatch.
    site = ''; gateway = ''; region = '';
    observer.send(input);
  }
  function selectSite(event) {
    event.preventDefault();
    observer.send({ kind: 'select_site', generation: connection.generation, site_oid: site, gateway_id: gateway, region: effectiveRegion });
  }
  function selectInstance(event) {
    closeLogin(); site = ''; gateway = ''; region = '';
    observer.select(event.target.value);
    observer.catalog();
  }
  function download() {
    const blob = new Blob([view.catalogText], { type: 'application/json;charset=utf-8' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url; a.download = 'somfy-observation.json'; a.click();
    URL.revokeObjectURL(url);
  }
</script>

<section class="somfy" aria-label={t('systeme.somfy.title')}>
  <div class="section-heading"><div><h2>{t('systeme.somfy.catalog_title')}</h2></div><span class="badge"><Icon name="shield" size={16} />{t('systeme.somfy.read_only_badge')}</span></div>
  {#if view.loading}<p role="status">{t('systeme.somfy.loading')}</p>{/if}
  {#if view.auth === 'owner_account' || view.auth === 'pin_fallback'}
    <p>{t(`systeme.somfy.${view.auth}`)}</p>
    <button type="button" onclick={view.auth === 'pin_fallback' ? unlock : () => observer.open()}>{t(view.auth === 'pin_fallback' ? 'systeme.somfy.unlock' : 'systeme.somfy.reload')}</button>
  {:else if view.auth === 'owner'}
    {#if !view.instances.length}<p>{t('systeme.somfy.none')}</p>{:else}
      {#if connection?.phase !== 'ready' || !row?.online}<SomfyCloudStatus {row} />{/if}
      <p class="permission-note">{t('systeme.somfy.read_only_explanation')}</p>
      {#if connection?.scope}
        <div class="actions">
          <button type="button" disabled={Boolean(view.pending) || view.reading} onclick={() => observer.send({ kind: 'refresh', generation: connection.generation })}><Icon name="repeat" size={18} />{t('systeme.somfy.refresh_provider')}</button>
        </div>
      {/if}
      {#if view.reading}<p role="status">{t('systeme.somfy.loading')}</p>{/if}
      {#if view.catalog}
        <SomfyCloudCatalog observation={view.catalog} details={view.details} busy={view.detailBusy} showDefinition={(kind, index) => observer.definition(kind, index)} />
      {/if}
      <details class="account" open={!connection?.scope}>
        <summary><strong>{t('systeme.somfy.account')}</strong><span>{t('systeme.somfy.account_hint')}</span></summary>
        <SomfyCloudStatus {row} />
        <button type="button" disabled={Boolean(view.pending) || view.reading} onclick={() => observer.open()}>{t('systeme.somfy.reload_view')}</button>
        {#if view.instances.length > 1}
          <label>{t('systeme.somfy.instance')}<select value={view.selected} onchange={selectInstance} disabled={Boolean(view.pending)}>{#each view.instances as instance}<option value={instance.instance}>{instance.instance}</option>{/each}</select></label>
        {/if}
        {#if connection?.scope}
          <p><code>{connection.scope.site_oid}</code> · <code>{connection.scope.gateway_id}</code></p>
          <p>{t('systeme.somfy.scope')}</p>
        {/if}
        <div class="actions">
          <button type="button" disabled={Boolean(view.pending)} onclick={() => { closeLogin(); loginOpen = true; }}>{t('systeme.somfy.login')}</button>
          {#if view.catalog}<button type="button" onclick={download}>{t('systeme.somfy.download')}</button>{/if}
        </div>
      {#if loginOpen}
        <form onsubmit={login} autocomplete="off" class="form">
          <label>{t('systeme.somfy.username')}<input bind:value={username} maxlength="1024" autocomplete="off" spellcheck="false" required /></label>
          <label>{t('systeme.somfy.password')}<input bind:value={password} type="password" maxlength="4096" autocomplete="new-password" required /></label>
          <div class="actions"><button class="primary" disabled={Boolean(view.pending) || !username.trim() || !password}>{t('systeme.somfy.submit')}</button><button type="button" onclick={closeLogin}>{t('systeme.somfy.cancel')}</button></div>
        </form>
      {/if}
      {#if connection?.phase === 'selecting_site'}
        <form onsubmit={selectSite} class="form">
          <label>{t('systeme.somfy.site')}<select bind:value={site} onchange={() => { gateway = ''; region = ''; }} required disabled={Boolean(view.pending)}><option value="">{t('systeme.somfy.choose')}</option>{#each connection.candidates as c}<option value={c.site_oid}>{c.label ?? c.site_oid} · {c.site_oid}</option>{/each}</select></label>
          <label>{t('systeme.somfy.gateway')}<select bind:value={gateway} required disabled={!candidate || Boolean(view.pending)}><option value="">{t('systeme.somfy.choose')}</option>{#each candidate?.gateway_ids ?? [] as id}<option value={id}>{id}</option>{/each}</select></label>
          {#if candidate && !candidate.region}<label>{t('systeme.somfy.region')}<select bind:value={region} required disabled={Boolean(view.pending)}><option value="">{t('systeme.somfy.choose')}</option>{#each ['emea', 'apac', 'snaba'] as r}<option value={r}>{t(`systeme.somfy.${r}`)}</option>{/each}</select></label>{/if}
          <p class="wide">{t('systeme.somfy.scope')}</p>
          <button class="primary" disabled={Boolean(view.pending) || !candidate || !gateway || !effectiveRegion}>{t('systeme.somfy.select')}</button>
        </form>
      {/if}
      {#if view.pending}<p role="status">{t('systeme.somfy.pending')}</p>{/if}
      </details>
    {/if}
  {/if}
  {#if view.error}<p role="alert" class="notice">{t(`systeme.somfy.error.${view.error}`)}</p>{/if}
</section>

<style>
  .somfy { display: grid; gap: 18px; padding: clamp(16px, 3vw, 28px); border-radius: var(--r-md); border: 1px solid var(--line); background: var(--surface-2); min-width: 0; }
  h2 { font-size: 20px; margin: 0 0 6px; }
  p { font-size: 13px; margin: 0; overflow-wrap: anywhere; }
  code { overflow-wrap: anywhere; }
  .section-heading { display: flex; justify-content: space-between; align-items: start; gap: 12px; flex-wrap: wrap; }
  .badge { display: inline-flex; align-items: center; gap: 6px; padding: 7px 10px; border-radius: 999px; background: var(--surface); color: var(--ink-2); font-size: 13px; }
  .permission-note { line-height: 1.5; color: var(--ink-2); }
  .account { border-top: 1px solid var(--line); padding-top: 16px; min-width: 0; }
  .account > :not(summary) { margin-top: 14px; }
  summary { cursor: pointer; min-height: 44px; }
  summary span { display: block; margin-top: 4px; color: var(--ink-2); font-size: 13px; }
  button:focus-visible, summary:focus-visible { outline: 3px solid var(--blue); outline-offset: 3px; }
  .notice { color: var(--warm-ink); }
  .actions { display: flex; gap: 8px; flex-wrap: wrap; }
  button { display: inline-flex; align-items: center; gap: 8px; min-height: 44px; justify-self: start; max-width: 100%; border: 1px solid var(--line); border-radius: 999px; padding: 9px 14px; color: var(--ink); background: var(--surface); cursor: pointer; font: inherit; font-size: 13px; }
  button.primary { background: var(--ink); color: var(--surface); }
  button:disabled { opacity: .5; cursor: default; }
  .form { display: grid; gap: 12px; grid-template-columns: repeat(auto-fit, minmax(min(100%, 220px), 1fr)); align-items: end; }
  label { display: grid; gap: 5px; font-size: 12px; color: var(--ink-2); min-width: 0; }
  input, select { min-width: 0; width: 100%; box-sizing: border-box; font: inherit; font-size: 14px; padding: 10px; border-radius: var(--r-sm); border: 1px solid var(--line); background: var(--surface); color: var(--ink); }
  .wide, .form .actions { grid-column: 1 / -1; }
</style>
