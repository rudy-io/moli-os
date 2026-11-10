<script>
  import { onDestroy } from 'svelte';
  import { mdiSpeedometerMedium, mdiSpeedometerSlow } from '@mdi/js';
  import { device, value, act, nameOf, num } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import { hub } from '../../lib/hub.svelte.js';
  import Icon from './Icon.svelte';
  import { canWrite, quantize, positionPoint } from '../lib/capabilities.js';
  import { coverCommands, coverPositionTarget, hasSlowCoverControl } from '../lib/cover-controls.js';
  import { completionGuard } from '../lib/completion-guard.js';

  /** A shutter: advertised actions, with optional icons-only room presentation. */
  let { id, name = null, standalone = false, icons = false } = $props();

  const d = $derived(device(id));
  const available = $derived(!!d && d.online !== false);
  const connected = $derived(hub.connected === true);
  const live = $derived(available && connected);
  let speedChoice = $state(null);
  const speed = $derived(speedChoice?.id === id ? speedChoice.speed : 'normal');
  const hasSlow = $derived(hasSlowCoverControl(d));
  const label = $derived(nameOf(id, name));
  const posPoint = $derived(positionPoint(d, true));
  const reported = $derived(posPoint ? value(id, posPoint.key) : null);
  const pos = $derived(posPoint?.kind?.type === 'numeric' && Number.isFinite(reported) && reported >= 0 && reported <= 100 ? reported : null);
  const actions = $derived(coverCommands(d, speed).map((action) => ({ ...action, ...BUTTONS[action.command] })));
  const writableActions = $derived(actions.filter(({ key, value }) => value && connected && canWrite(d, key)));
  const positionTarget = $derived(coverPositionTarget(d, speed));
  const range = $derived(positionTarget?.range);
  const positionWritable = $derived(connected && !!positionTarget && canWrite(d, positionTarget.point.key));
  const moving = $derived(value(id, 'moving'));
  const going = $derived(
    moving === 'UP' ? t('commun.volet.monte')
      : moving === 'DOWN' ? t('commun.volet.descend')
        : moving && String(moving).toLowerCase() !== 'stop' ? t('commun.volet.mouvement') : '',
  );
  let dragging = $state(null);
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
  const sameTarget = $derived(dragging?.id === id && dragging?.key === positionTarget?.point.key && dragging.speed === speed);
  const shown = $derived(positionWritable && sameTarget ? dragging.value : pos);
  const status = $derived([
    pos == null ? t('commun.volet.position_inconnue')
      : !live ? t('commun.volet.derniere_position', { pct: num(pos) })
        : pos === 0 ? t('systeme.format.binaire.contact.on')
          : pos === 100 ? t('systeme.format.binaire.contact.off')
            : t('commun.volet.ouvert_a', { pct: num(pos) }),
    live ? going : '',
  ].filter(Boolean).join(' · '));
  const accessReason = $derived(!connected ? 'reconnexion' : !available ? 'injoignable'
    : !writableActions.length && !positionWritable ? 'lecture_seule' : null);
  function selectSpeed(next) {
    if (next === 'normal' || next === 'slow' && hasSlow) speedChoice = { id, speed: next };
  }
  function previewPosition(event) {
    if (positionWritable && pos != null) dragging = { id, key: positionTarget.point.key, speed, value: quantize(Number(event.currentTarget.value), range) };
  }
  function setPosition(event) {
    const validTarget = !dragging || sameTarget;
    dragging = null;
    if (!validTarget || !positionWritable || pos == null) return;
    const next = quantize(Number(event.currentTarget.value), range);
    if (next != null && next !== pos) return submit(positionTarget.point.key, next, label);
  }
  function sendOrder(command, key) {
    const choice = coverCommands(d, speed).find((item) => item.command === command);
    if (choice?.value && connected && canWrite(d, choice.key)) return submit(choice.key, choice.value, t('commun.action', { label, action: t(key).toLowerCase() }));
  }
  async function submit(key, next, description) {
    if (!connected || !canWrite(d, key)) return;
    if (sending?.key === key && sending.value === next) return;
    // A new intent (including Stop) supersedes feedback, never the device's reading.
    completion.change();
    const owns = completion.capture();
    sending = { key, value: next };
    feedback = 'sending';
    const result = await act(`${id}/${key}`, next, description);
    if (!owns()) return;
    sending = null;
    feedback = result === 'ok' ? 'sent' : result;
  }
  const BUTTONS = {
    open: { labelKey: 'commun.volet.ouvrir', icon: 'chevron-up' },
    stop: { labelKey: 'commun.volet.stop', icon: 'stop' },
    close: { labelKey: 'commun.volet.fermer', icon: 'chevron-down' },
    cycle: { labelKey: 'commun.volet.impulsion', icon: 'repeat' },
  };
</script>

<div class="cover" class:standalone class:icons title={icons ? [label, status].filter(Boolean).join(' · ') : undefined}>
  <div class="heading">
    <span class="device-icon" class:open={live && pos > 0} class:moving={live && !!going}>
      <Icon name={live ? 'shutter' : 'offline'} size={24} />
    </span>
    {#if !icons}<span class="name">{label}<small>{status}</small></span>{/if}
  </div>
  {#if accessReason}
    {@const accessLabel = t(accessReason === 'injoignable' ? 'commun.piece.injoignable' : `commun.volet.${accessReason}`)}
    <details class="access">
      <summary title={icons ? accessLabel : undefined} aria-label={icons ? accessLabel : undefined}>
        {#if icons}<Icon name={accessReason === 'lecture_seule' ? 'lock' : 'offline'} size={20} />{:else}{accessLabel}{/if}
      </summary>
      <p>{t(!connected ? 'commun.hors_ligne' : available ? 'commun.volet.lecture_seule_aide' : 'commun.volet.connexion_aide')}
        <a href="#/systeme">{t('commun.volet.voir_connexion')}</a></p>
    </details>
  {/if}
  {#if actions.length}
    <div class="actions" role="group" aria-label={t('commun.volet.commandes_de', { label })}>
      {#each actions as action (action.command)}
        <button type="button" data-cover-command={action.command}
          title={icons ? t(action.labelKey) : undefined} aria-label={icons ? t(action.labelKey) : undefined}
          disabled={!live || !action.value || !canWrite(d, action.key) || sending?.key === action.key && sending.value === action.value}
          onclick={() => sendOrder(action.command, action.labelKey)}>
          <Icon name={action.icon} size={20} />
          {#if !icons}<span>{t(action.labelKey)}</span>{/if}
        </button>
      {/each}
    </div>
  {/if}
  {#if hasSlow || speed === 'slow'}
    <div class="speed" role="group" aria-label={t('commun.volet.vitesse_prochaine')}>
      {#if !icons}<span>{t('commun.volet.vitesse')}</span>{/if}
      <div class="segments">
        <button type="button" aria-pressed={speed === 'normal'} onclick={() => selectSpeed('normal')}
          title={icons ? t('commun.volet.vitesse_normale') : undefined} aria-label={icons ? t('commun.volet.vitesse_normale') : undefined}>
          {#if icons}<Icon path={mdiSpeedometerMedium} size={20} />{:else}{t('commun.volet.vitesse_normale')}{/if}
        </button>
        <button type="button" aria-pressed={speed === 'slow'} disabled={!hasSlow} onclick={() => selectSpeed('slow')}
          title={icons ? t('commun.volet.vitesse_lente') : undefined} aria-label={icons ? t('commun.volet.vitesse_lente') : undefined}>
          {#if icons}<Icon path={mdiSpeedometerSlow} size={20} />{:else}{t('commun.volet.vitesse_lente')}{/if}
        </button>
      </div>
    </div>
  {/if}
  {#if positionWritable}
    <label class="position">
      {#if !icons}<span>{t('commun.volet.position')}</span>{/if}
      <input type="range" min={range.first} max={range.last} step={range.step} value={shown ?? range.first} disabled={pos == null} oninput={previewPosition} onchange={setPosition} aria-label={t('commun.volet.ouverture', { label })} />
    </label>
  {/if}
  {#if feedback}
    <p class="feedback" role="status" title={icons ? t(`commun.volet.feedback.${feedback}`) : undefined} aria-label={icons ? t(`commun.volet.feedback.${feedback}`) : undefined}>
      {#if icons}<Icon name={feedback === 'sending' ? 'clock' : feedback === 'sent' ? 'ok' : feedback === 'held' ? 'lock' : 'warning'} size={20} />{:else}{t(`commun.volet.feedback.${feedback}`)}{/if}
    </p>
  {/if}
</div>

<style>
  .cover {
    display: grid;
    gap: 12px;
    min-width: 0;
    padding: 14px;
    background: var(--surface-2);
    border-radius: var(--r-md);
    color: var(--ink-2);
  }
  .heading { display: flex; align-items: center; gap: 10px; min-width: 0; }
  .device-icon { display: grid; place-items: center; width: 44px; height: 44px; flex: none; border-radius: var(--r-sm); background: var(--surface-3); color: var(--ink-2); }
  .device-icon.open { background: var(--warm-soft); color: var(--warm-ink); }
  .device-icon.moving { animation: moving 1.6s ease-in-out infinite alternate; }
  .name {
    flex: 1;
    display: grid;
    font-weight: 650;
    color: var(--ink);
    min-width: 0;
    overflow-wrap: anywhere;
    line-height: 1.35;
    gap: 2px;
  }

  .standalone { background: var(--surface); border: 1px solid var(--line); }

  small {
    font-weight: 500;
    color: var(--ink-2);
    font-size: 13px;
  }

  .access { font-size: 13px; max-width: 100%; }
  .access summary { cursor: pointer; color: var(--ink-2); min-height: 44px; line-height: 24px; padding: 10px 0; }
  .access p { max-width: 38ch; margin: 8px 0 0; line-height: 1.5; }
  .access a { color: inherit; text-underline-offset: 3px; }
  .feedback { margin: 0; color: var(--ink-2); font-size: 13px; line-height: 1.5; }

  .actions { display: flex; gap: 6px; }
  .actions button { flex: 1; display: grid; justify-items: center; align-content: center; gap: 2px; min-width: 0; min-height: 52px; }
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
  button:focus-visible, summary:focus-visible { outline: 3px solid var(--cool); outline-offset: 3px; }
  @media (hover: hover) { button:not(:disabled):hover { background: var(--surface-3); } }

  .position { display: flex; align-items: center; gap: 10px; font-size: 13px; }
  .position input { flex: 1; min-width: 0; min-height: 44px; accent-color: var(--cool); }
  .speed { display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: 8px; }
  .speed > span { font-size: 12px; color: var(--ink-2); }
  .segments { display: flex; gap: 2px; padding: 2px; border-radius: var(--r-sm); background: var(--surface-3); }
  .segments button { min-height: 44px; min-width: 60px; border-color: transparent; background: transparent; }
  .segments button[aria-pressed="true"] { background: var(--surface); box-shadow: 0 1px 3px rgb(0 0 0 / 12%); }
  /* Room preference: compact icons without shrinking the touch targets. */
  .icons { display: flex; flex-wrap: wrap; align-items: center; padding: 8px 10px; gap: 8px; }
  .icons .actions { flex-wrap: wrap; }
  .icons button { display: grid; place-items: center; flex: 0 0 44px; width: 44px; min-width: 44px; min-height: 44px; padding: 0; }
  .icons .access summary { display: grid; place-items: center; width: 44px; padding: 0; list-style: none; }
  .icons .access summary::-webkit-details-marker { display: none; }
  .icons .access[open] { flex-basis: 100%; }
  .icons .position { flex: 1 1 110px; width: auto; }
  @keyframes moving { to { opacity: 0.6; } }
  @media (prefers-reduced-motion: reduce) {
    button { transition: none; }
    button:not(:disabled):active { transform: none; }
    .device-icon.moving { animation: none; }
  }
</style>
