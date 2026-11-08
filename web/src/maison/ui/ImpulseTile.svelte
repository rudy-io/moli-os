<script>
  import { onDestroy } from 'svelte';
  import { device, act, nameOf } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import { hub } from '../../lib/hub.svelte.js';
  import Icon from './Icon.svelte';
  import { canWrite, impulsePoints } from '../lib/capabilities.js';
  import { completionGuard } from '../lib/completion-guard.js';

  /** A device driven by trigger relays (a gate, a garage door): one « Impulsion »
   *  button per relay. A trigger has no state to show, only an order to send. */
  let { id, name = null, icons = false } = $props();

  const d = $derived(device(id));
  const connected = $derived(hub.connected === true);
  const live = $derived(!!d && d.online !== false && connected);
  const label = $derived(nameOf(id, name));
  const triggers = $derived(impulsePoints(d));
  let sending = $state(null);
  let feedback = $state('');
  const completion = completionGuard();
  onDestroy(() => completion.change());
  $effect(() => {
    void id;
    completion.change();
    sending = null;
    feedback = '';
  });

  /** « Relais 1 »: what the relay is wired to is the owner's to say (the
   *  device's name), so the relays stay numbered as the bridge numbers them. */
  function relayName(point) {
    const n = /(\d+)$/.exec(point.key)?.[1];
    return n ? t('commun.impulsion.relais', { n }) : point.label;
  }
  const caption = (point) => t('commun.action', { label, action: `${t('commun.volet.impulsion')} · ${relayName(point)}` });

  async function fire(point) {
    if (!live || !canWrite(d, point.key) || sending === point.key) return;
    // A new order supersedes the previous feedback.
    completion.change();
    const owns = completion.capture();
    sending = point.key;
    feedback = 'sending';
    const result = await act(`${id}/${point.key}`, 'cycle', caption(point));
    if (!owns()) return;
    sending = null;
    feedback = result === 'ok' ? 'sent' : result;
  }
</script>

<div class="impulse" class:icons title={icons ? label : undefined}>
  <div class="heading">
    <span class="device-icon"><Icon name={live ? 'gate' : 'offline'} size={24} /></span>
    {#if !icons}<span class="name">{label}</span>{/if}
  </div>
  <div class="actions">
    {#each triggers as point (point.key)}
      <button type="button" disabled={!live || !canWrite(d, point.key) || sending === point.key}
        title={icons ? caption(point) : undefined} aria-label={caption(point)} onclick={() => fire(point)}>
        <Icon name="repeat" size={20} />
        {#if !icons}<span>{t('commun.impulsion.bouton', { relais: relayName(point) })}</span>{/if}
      </button>
    {/each}
  </div>
  {#if feedback}
    <p class="feedback" role="status" title={icons ? t(`commun.volet.feedback.${feedback}`) : undefined} aria-label={icons ? t(`commun.volet.feedback.${feedback}`) : undefined}>
      {#if icons}<Icon name={feedback === 'sending' ? 'clock' : feedback === 'sent' ? 'ok' : feedback === 'held' ? 'lock' : 'warning'} size={20} />{:else}{t(`commun.volet.feedback.${feedback}`)}{/if}
    </p>
  {/if}
</div>

<style>
  .impulse { display: grid; gap: 12px; min-width: 0; padding: 14px; background: var(--surface-2); border-radius: var(--r-md); color: var(--ink-2); }
  .heading { display: flex; align-items: center; gap: 10px; min-width: 0; }
  .device-icon { display: grid; place-items: center; width: 44px; height: 44px; flex: none; border-radius: var(--r-sm); background: var(--surface-3); color: var(--ink-2); }
  .name { flex: 1; font-weight: 650; color: var(--ink); min-width: 0; overflow-wrap: anywhere; line-height: 1.35; }
  .actions { display: flex; flex-wrap: wrap; gap: 6px; }
  .actions button { flex: 1 1 120px; display: grid; justify-items: center; align-content: center; gap: 2px; min-width: 0; min-height: 52px; }
  button {
    border: 1px solid var(--line);
    background: var(--surface);
    color: var(--ink);
    border-radius: var(--r-sm);
    padding: 6px 8px;
    font-size: 13px;
    font-weight: 650;
    cursor: pointer;
    touch-action: manipulation;
    transition: background-color 160ms ease, transform 160ms ease;
  }
  button:disabled { color: var(--ink-2); background: var(--surface-2); cursor: default; }
  button:not(:disabled):active { transform: scale(0.97); background: var(--surface-3); }
  button:focus-visible { outline: 3px solid var(--cool); outline-offset: 3px; }
  @media (hover: hover) { button:not(:disabled):hover { background: var(--surface-3); } }
  .feedback { margin: 0; color: var(--ink-2); font-size: 13px; line-height: 1.5; }
  /* Room preference: compact icons without shrinking the touch targets. */
  .icons { display: flex; flex-wrap: wrap; align-items: center; padding: 8px 10px; gap: 8px; }
  .icons .actions { flex-wrap: wrap; }
  .icons button { display: grid; place-items: center; flex: 0 0 44px; width: 44px; min-width: 44px; min-height: 44px; padding: 0; }
  @media (prefers-reduced-motion: reduce) {
    button { transition: none; }
    button:not(:disabled):active { transform: none; }
  }
</style>
