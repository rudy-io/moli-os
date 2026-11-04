<script>
  import { tick } from 'svelte';
  import { t } from '../../lib/i18n.svelte.js';
  import { act, hub } from '../lib/home.svelte.js';
  import { scenarioControl, launchScenario } from '../lib/somfy-scenario-controls.js';
  import Icon from './Icon.svelte';
  let { observation, definition, generation, pending = false, permission } = $props();
  const control = $derived(scenarioControl(observation, definition.id, hub.devices, hub.connected));
  const identity = $derived(`${generation}:${observation.revision}:${definition.id}:${control.permitted}`);
  let opened = $state(false);
  let pin = $state('');
  let consent = $state(false);
  let intent = $state(null);
  let busy = $state(false);
  let outcome = $state('');
  let error = $state('');
  let pinInput;
  $effect(() => { identity; opened = false; pin = ''; consent = false; intent = null; outcome = ''; error = ''; });
  $effect(() => { if (opened && !control.permitted && control.reason !== 'permission_required') { opened = false; pin = ''; consent = false; } });
  async function configure() {
    opened = !opened; pin = ''; consent = false; outcome = ''; error = '';
    if (opened) { intent = !control.permitted; await tick(); pinInput?.focus(); }
  }
  async function save(event) {
    event.preventDefault();
    if (busy || pending || !consent || !pin) return;
    if (intent !== !control.permitted) { opened = false; pin = ''; consent = false; return; }
    busy = true;
    const input = { generation, revision: observation.revision, scenario_id: definition.id, enabled: intent, pin };
    pin = '';
    try { error = await permission(input); if (!error) opened = false; }
    finally { input.pin = ''; busy = false; }
  }
  async function launch() {
    if (busy || pending || !control.executable) return;
    busy = true; outcome = '';
    const before = identity;
    try {
      const result = await launchScenario(observation, definition.id, hub.devices, hub.connected, act, definition.label ?? t('systeme.somfy.scenario.label'));
      if (identity === before) outcome = ['ok', 'held', 'error', 'unavailable'].includes(result) ? result : 'error';
    } finally { busy = false; }
  }
</script>

<div class="scenario-control">
  {#if control.executable}
    <button class="launch" onclick={launch} disabled={busy || pending}><Icon name="play" size={20} />{t(busy ? 'systeme.somfy.scenario.sending' : 'systeme.somfy.scenario.launch')}</button>
  {:else}
    <p class="restriction"><Icon name="lock" size={16} />{t(`systeme.somfy.scenario.reason.${control.reason ?? 'not_supported'}`)}</p>
  {/if}
  {#if control.permitted || control.reason === 'permission_required'}
    <button class="permission" onclick={configure} disabled={busy || pending} aria-expanded={opened}>{t(control.permitted ? (observation.execution.read_only ? 'systeme.somfy.scenario.revoke_title' : 'systeme.somfy.scenario.revoke') : 'systeme.somfy.scenario.authorize')}</button>
  {/if}
  {#if outcome}<p role="status">{t(`systeme.somfy.scenario.outcome.${outcome}`)}</p>{/if}
  {#if opened}
    <form onsubmit={save}>
      <strong>{t(intent ? 'systeme.somfy.scenario.authorize_title' : 'systeme.somfy.scenario.revoke_title')}</strong>
      <p>{t(intent ? 'systeme.somfy.scenario.scope' : 'systeme.somfy.scenario.revoke_scope')}</p>
      <label>{t('systeme.somfy.scenario.pin')}<input bind:this={pinInput} bind:value={pin} type="password" inputmode="numeric" autocomplete="off" minlength="4" maxlength="8" required /></label>
      <label class="consent"><input type="checkbox" bind:checked={consent} required />{t(intent ? 'systeme.somfy.scenario.confirm_authorize' : 'systeme.somfy.scenario.confirm_revoke')}</label>
      <div class="buttons"><button type="submit" disabled={busy || pending || !consent || pin.length < 4}>{t('systeme.somfy.scenario.confirm')}</button><button type="button" disabled={busy || pending} onclick={() => { opened = false; pin = ''; }}>{t('systeme.somfy.cancel')}</button></div>
    </form>
  {/if}
</div>

<style>
  .scenario-control { display: flex; flex-wrap: wrap; align-items: center; gap: 10px; border-top: 1px solid var(--line); padding-top: 14px; }
  p { margin: 0; font-size: 13px; line-height: 1.5; color: var(--ink-2); }
  .restriction { display: flex; gap: 7px; align-items: start; flex: 1 1 100%; }
  button { display: inline-flex; align-items: center; justify-content: center; gap: 8px; min-height: 44px; padding: 10px 15px; border: 1px solid var(--line); border-radius: 999px; background: var(--surface); color: var(--ink); font: inherit; font-size: 14px; font-weight: 600; cursor: pointer; }
  .launch { color: var(--surface); background: var(--ink); border-color: var(--ink); }
  .permission { font-size: 13px; font-weight: 500; }
  button:disabled { opacity: .5; cursor: default; }
  button:active:not(:disabled) { transform: scale(.98); }
  button:focus-visible, input:focus-visible { outline: 3px solid var(--blue); outline-offset: 3px; }
  form { flex: 1 1 100%; display: grid; gap: 12px; padding: 16px; background: var(--surface-2); border-radius: var(--r-md); min-width: 0; }
  label { display: grid; gap: 6px; font-size: 14px; }
  input[type=password] { min-width: 0; width: 100%; box-sizing: border-box; min-height: 44px; border: 1px solid var(--line); border-radius: var(--r-sm); padding: 10px; background: var(--surface); color: var(--ink); font: inherit; }
  .consent { display: flex; align-items: center; min-height: 44px; gap: 10px; line-height: 1.5; }
  .consent input { width: 20px; height: 20px; flex: none; }
  .buttons { display: flex; flex-wrap: wrap; gap: 8px; }
  @media (prefers-reduced-motion: reduce) { button:active:not(:disabled) { transform: none; } }
</style>
