<script>
  import { onDestroy, untrack } from 'svelte';
  import { t } from '../../lib/i18n.svelte.js';
  import { deviceName } from '../../lib/format.js';
  import { home } from '../lib/home.svelte.js';
  import { saveDeviceLabels, setDeviceHidden } from '../lib/device-management.js';
  import AirzoneReadings from './AirzoneReadings.svelte';
  import Icon from './Icon.svelte';

  let { devices = [], hidden = [], connected = true, selectedId = null, onchanged } = $props();
  let editing = $state(null);
  let draft = $state({ name: '', room: '' });
  let confirmId = $state(null);
  let pending = $state(false);
  let awaitingPin = $state(false);
  let error = $state(null);
  let success = $state(null);
  let warning = $state(null);
  let query = $state('');
  let filter = $state('all');
  const hiddenIds = $derived(new Set(hidden));
  const busy = $derived(pending || awaitingPin);
  const visibleDevices = $derived.by(() => {
    const needle = query.trim().toLocaleLowerCase();
    const matches = devices.filter((device) => {
      if (filter === 'hidden' && !hiddenIds.has(device.id)) return false;
      if (filter === 'visible' && hiddenIds.has(device.id)) return false;
      return !needle || [deviceName(device), device.native_name, device.label?.room, device.native_room, device.id]
        .some((value) => typeof value === 'string' && value.toLocaleLowerCase().includes(needle));
    });
    // A link from a zone should open its editor in view, without dropping the
    // rest of the inventory or changing the stable device order underneath.
    const selectedIndex = matches.findIndex((device) => device.id === selectedId);
    if (selectedIndex > 0) matches.unshift(matches.splice(selectedIndex, 1)[0]);
    return matches;
  });

  let alive = true;
  let heldRetry = null;
  let previousSelection = null;
  $effect(() => {
    const selected = devices.find((device) => device.id === selectedId);
    if (!busy && selected && selectedId !== previousSelection) {
      untrack(() => {
        if (edit(selected)) {
          query = '';
          filter = 'all';
          previousSelection = selected.id;
        }
      });
    }
    if (!selectedId) previousSelection = null;
  });
  onDestroy(() => {
    alive = false;
    if (heldRetry && home.held?.custom === heldRetry) home.held = null;
    heldRetry = null;
  });

  function clearFeedback() {
    error = null;
    success = null;
    warning = null;
  }

  function edit(device) {
    if (busy || !devices.some((item) => item.id === device.id)) return false;
    clearFeedback();
    editing = device.id;
    draft = { name: device.label?.name ?? '', room: device.label?.room ?? '' };
    confirmId = null;
    return true;
  }

  function cancelEdit() {
    if (busy) return;
    editing = null;
    clearFeedback();
  }

  function requestHide(id) {
    if (busy || !devices.some((device) => device.id === id)) return;
    clearFeedback();
    confirmId = id;
    editing = null;
  }

  function cancelHide() {
    if (busy) return;
    confirmId = null;
    clearFeedback();
  }

  function askHuman(change) {
    const device = devices.find((item) => item.id === change.id);
    if (!device) { error = 'missing'; return; }
    if (!connected) { error = 'connection'; return; }
    // Never replace another action's approval, and never retain a PIN here.
    if (home.held) { error = 'auth_busy'; return; }
    awaitingPin = true;
    const retry = async () => {
      if (!alive || heldRetry !== retry) return;
      heldRetry = null;
      awaitingPin = false;
      await perform(change, true);
    };
    heldRetry = retry;
    home.held = {
      label: deviceName(device) || change.id,
      reason: 'configuration',
      custom: retry,
      onCancel: () => {
        if (!alive || heldRetry !== retry) return;
        heldRetry = null;
        awaitingPin = false;
        error = 'cancelled';
      },
    };
  }

  async function perform(change, authenticated = false) {
    if (!alive) return;
    if (!connected) { error = 'connection'; return; }
    if (!devices.some((device) => device.id === change.id)) { error = 'missing'; return; }
    pending = true;
    clearFeedback();
    const result = change.kind === 'labels'
      ? await saveDeviceLabels(change.id, change.data)
      : await setDeviceHidden(change.id, change.data.hidden);
    if (!alive) return;
    if (!result.ok) {
      pending = false;
      if (result.error === 'auth' && !authenticated) askHuman(change);
      else error = result.status === 404 ? 'missing' : result.status === 429 ? 'rate_limit' : result.error;
      return;
    }
    if (editing === change.id) editing = null;
    if (confirmId === change.id) confirmId = null;
    success = change.kind === 'labels' ? 'labels_saved' : change.data.hidden ? 'hidden_saved' : 'restored';
    try {
      await onchanged?.({ id: change.id, kind: change.kind, data: result.data });
    } catch {
      // The write is confirmed. A stale view is different from a failed save.
      if (alive) warning = 'refresh';
    } finally {
      if (alive) pending = false;
    }
  }

  async function saveLabels(event) {
    event?.preventDefault();
    if (busy || !editing) return;
    await perform({ id: editing, kind: 'labels', data: { name: draft.name, room: draft.room } });
  }

  async function changeVisibility(id, hidden) {
    if (busy || (hidden && confirmId !== id)) return;
    await perform({ id, kind: 'visibility', data: { hidden } });
  }
</script>

<section class="management" aria-label={t('airzone_management.inventory')}>
  <p class="intro">{t('airzone_management.intro')}</p>
  {#if !connected}<p class="notice" role="status"><Icon name="offline" size={18} />{t('airzone_management.error.connection')}</p>{/if}
  <div class="filters">
    <label class="search">
      <span>{t('airzone_management.search')}</span>
      <input type="search" bind:value={query} disabled={busy} placeholder={t('airzone_management.search_hint')} />
    </label>
    <label class="visibility-filter">
      <span>{t('airzone_management.visibility')}</span>
      <select bind:value={filter} disabled={busy}>
        <option value="all">{t('airzone_management.all')}</option>
        <option value="visible">{t('airzone_management.visible')}</option>
        <option value="hidden">{t('airzone_management.hidden')}</option>
      </select>
    </label>
  </div>
  <div class="feedback" aria-live="polite">
    {#if success}<p class="success" role="status"><Icon name="ok" size={18} />{t(`airzone_management.success.${success}`)}</p>{/if}
    {#if warning}<p class="notice" role="status">{t(`airzone_management.error.${warning}`)}</p>{/if}
    {#if error}<p class="error" role="alert">{t(`airzone_management.error.${error}`)}</p>{/if}
    {#if awaitingPin}<p class="notice" role="status">{t('airzone_management.awaiting_pin')}</p>{/if}
  </div>
  {#if selectedId && !devices.some((device) => device.id === selectedId)}
    <p class="notice" role="status">{t('airzone_management.error.missing')}</p>
  {/if}
  <p class="count">{t('airzone_management.count', { count: visibleDevices.length })}</p>
  <ul class="inventory">
    {#each visibleDevices as device (device.id)}
      {@const isHidden = hiddenIds.has(device.id)}
      <li class:selected={selectedId === device.id}>
        <div class="device-heading">
          <div class="identity">
            <h2>{deviceName(device) || device.id}</h2>
            <p class="room">{device.label?.room || device.native_room || t('airzone_management.no_room')}</p>
            <p class="stable-id"><span>{t('airzone_management.identity')}</span><code>{device.id}</code></p>
          </div>
          <span class="badge" class:muted={isHidden}>{t(isHidden ? 'airzone_management.hidden' : 'airzone_management.visible')}</span>
        </div>
        <div class="actions">
          <button type="button" class="secondary" disabled={busy} onclick={() => edit(device)} aria-expanded={editing === device.id}>
            <Icon name="tune" size={18} />{t('airzone_management.edit')}
          </button>
          {#if isHidden}
            <button type="button" class="secondary" disabled={busy || !connected} onclick={() => changeVisibility(device.id, false)}>{t('airzone_management.restore')}</button>
          {:else}
            <button type="button" class="quiet" disabled={busy || !connected} onclick={() => requestHide(device.id)} aria-expanded={confirmId === device.id}>{t('airzone_management.hide')}</button>
          {/if}
        </div>
        {#if editing === device.id}
          <form class="editor" onsubmit={saveLabels} aria-label={t('airzone_management.edit_device', { name: deviceName(device) || device.id })} aria-busy={pending}>
            <p class="help">{t('airzone_management.label_help')}</p>
            <div class="fields">
              <label><span>{t('airzone_management.name')}</span><input bind:value={draft.name} maxlength="60" disabled={busy} placeholder={device.native_name || ''} /></label>
              <label><span>{t('airzone_management.room')}</span><input bind:value={draft.room} maxlength="60" disabled={busy} placeholder={device.native_room || ''} /></label>
            </div>
            <p class="help">{t('airzone_management.blank_help')}</p>
            <div class="actions">
              <button type="submit" class="primary" disabled={busy || !connected}>{t(pending ? 'airzone_management.saving' : 'airzone_management.save')}</button>
              <button type="button" class="quiet" disabled={busy} onclick={cancelEdit}>{t('airzone_management.cancel')}</button>
            </div>
          </form>
        {/if}
        {#if confirmId === device.id}
          <div class="confirmation" role="group" aria-label={t('airzone_management.confirm_title', { name: deviceName(device) || device.id })}>
            <h3>{t('airzone_management.confirm_title', { name: deviceName(device) || device.id })}</h3>
            <p>{t('airzone_management.confirm_help')}</p>
            <div class="actions">
              <button type="button" class="primary" disabled={busy || !connected} onclick={() => changeVisibility(device.id, true)}>{t(pending ? 'airzone_management.saving' : 'airzone_management.confirm_hide')}</button>
              <button type="button" class="quiet" disabled={busy} onclick={cancelHide}>{t('airzone_management.cancel')}</button>
            </div>
          </div>
        {/if}
        <details class="values">
          <summary><span>{t('airzone_management.readings')}</span><Icon name="chevron-down" size={18} /></summary>
          <AirzoneReadings {device} />
        </details>
      </li>
    {:else}
      <li class="empty">{t('airzone_management.no_results')}</li>
    {/each}
  </ul>
</section>

<style>
  .management { min-width: 0; }
  .intro { margin: 0 0 22px; max-width: 74ch; color: var(--ink-2); font-size: 14px; line-height: 1.65; }
  .filters { display: flex; gap: 14px; flex-wrap: wrap; margin-bottom: 14px; }
  label { display: grid; gap: 7px; min-width: 0; color: var(--ink-2); font-size: 12px; font-weight: 600; }
  .search { flex: 1 1 240px; }
  .visibility-filter { flex: 0 1 180px; }
  input, select { min-width: 0; width: 100%; min-height: 44px; padding: 10px 12px; border: 1px solid var(--line); border-radius: var(--r-sm); background: var(--surface); color: var(--ink); font: inherit; font-size: 14px; font-weight: 400; }
  .count { margin: 16px 0 12px; color: var(--ink-3); font-size: 12px; }
  .inventory { display: grid; gap: 14px; padding: 0; margin: 0; list-style: none; }
  li { min-width: 0; padding: 20px; border: 1px solid var(--line); border-radius: var(--r-md); background: var(--surface); }
  li.selected { border-color: var(--cool); }
  .device-heading { display: flex; align-items: flex-start; justify-content: space-between; gap: 12px; }
  .identity { min-width: 0; }
  h2 { margin: 0; font-size: 17px; font-weight: 650; line-height: 1.4; overflow-wrap: anywhere; }
  .room { margin: 5px 0 0; color: var(--ink-2); font-size: 13px; overflow-wrap: anywhere; }
  .stable-id { display: flex; flex-wrap: wrap; gap: 4px 8px; margin: 10px 0 0; color: var(--ink-3); font-size: 11px; line-height: 1.5; }
  code { min-width: 0; overflow-wrap: anywhere; font-size: 11px; }
  .badge { flex: none; padding: 5px 9px; border-radius: 999px; background: var(--good-soft); color: var(--good); font-size: 11px; font-weight: 650; }
  .badge.muted { color: var(--ink-2); background: var(--surface-2); }
  .actions { display: flex; flex-wrap: wrap; gap: 8px; margin-top: 16px; }
  button { display: inline-flex; align-items: center; justify-content: center; gap: 7px; min-height: 44px; max-width: 100%; padding: 10px 14px; border: 1px solid transparent; border-radius: var(--r-sm); font: inherit; font-size: 13px; font-weight: 600; line-height: 1.4; cursor: pointer; overflow-wrap: anywhere; transition: background 140ms var(--ease); }
  .primary { background: var(--cool); color: var(--surface); }
  .secondary { background: var(--surface-2); color: var(--ink); border-color: var(--line); }
  .quiet { background: transparent; color: var(--ink-2); }
  button:hover:not(:disabled) { filter: brightness(.96); }
  button:disabled, input:disabled, select:disabled { opacity: .55; cursor: default; }
  button:focus-visible, input:focus-visible, select:focus-visible, summary:focus-visible { outline: 3px solid var(--cool); outline-offset: 3px; }
  .editor, .confirmation { margin-top: 16px; padding: 16px; border-radius: var(--r-sm); background: var(--surface-2); }
  .fields { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 14px; margin-top: 14px; }
  .help, .confirmation p { margin: 0; color: var(--ink-2); font-size: 12px; line-height: 1.6; }
  .fields + .help { margin-top: 10px; }
  h3 { margin: 0 0 8px; color: var(--ink); font-size: 14px; font-weight: 650; overflow-wrap: anywhere; }
  .notice, .error, .success { display: flex; align-items: flex-start; gap: 9px; margin: 12px 0; padding: 12px 14px; border-radius: var(--r-sm); font-size: 13px; line-height: 1.6; }
  .notice { background: var(--surface-2); color: var(--ink-2); }
  .error { background: var(--alert-soft); color: var(--alert); }
  .success { background: var(--good-soft); color: var(--good); }
  .values { margin-top: 16px; border-top: 1px solid var(--line); }
  summary { display: flex; justify-content: space-between; align-items: center; gap: 12px; min-height: 48px; list-style: none; color: var(--ink-2); font-size: 12px; cursor: pointer; }
  summary::-webkit-details-marker { display: none; }
  .empty { text-align: center; padding: 28px 20px; color: var(--ink-2); font-size: 14px; }
  @media (max-width: 520px) {
    li { padding: 16px; }
    .fields { grid-template-columns: minmax(0, 1fr); }
    .visibility-filter { flex-grow: 1; }
    .editor, .confirmation { padding: 14px; }
  }
  @media (prefers-reduced-motion: reduce) { button { transition: none; } }
</style>
