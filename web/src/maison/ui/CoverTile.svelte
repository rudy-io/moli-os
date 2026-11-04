<script>
  import { onDestroy } from 'svelte';
  import { device, value, act, nameOf, num } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';
  import { canWrite, numericRange, quantize, writableChoices, positionPoint, coverOrder, enumChoice } from '../lib/capabilities.js';
  import { completionGuard } from '../lib/completion-guard.js';

  /** A shutter: open, stop, close (whatever the device offers). */
  let { id, name = null, standalone = false } = $props();

  const d = $derived(device(id));
  const available = $derived(!!d && d.online !== false);
  const label = $derived(nameOf(id, name));
  const posPoint = $derived(positionPoint(d, true));
  const reported = $derived(posPoint ? value(id, posPoint.key) : null);
  const pos = $derived(posPoint?.kind?.type === 'numeric' && Number.isFinite(reported) && reported >= 0 && reported <= 100 ? reported : null);
  const order = $derived(coverOrder(d, true));
  const offers = $derived(writableChoices(d, order?.key));
  const actions = $derived(BUTTONS.filter(([cmd]) => enumChoice(order?.kind?.values, cmd)));
  const writableActions = $derived(actions.filter(([cmd]) => enumChoice(offers, cmd)));
  const range = $derived(posPoint?.semantic === 'position' ? numericRange(d, posPoint.key, 1, { min: 0, max: 100 }) : null);
  const positionWritable = $derived(!!range && range.min >= 0 && range.max <= 100 && canWrite(d, posPoint.key));
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
  const sameTarget = $derived(dragging?.id === id && dragging?.key === posPoint?.key);
  const shown = $derived(positionWritable && sameTarget ? dragging.value : pos);
  const status = $derived([
    shown != null ? t(available ? 'commun.volet.ouvert_a' : 'commun.volet.derniere_position', { pct: num(shown) }) : t('commun.volet.position_inconnue'),
    available ? going : '',
  ].filter(Boolean).join(' · '));
  function previewPosition(event) {
    if (positionWritable && pos != null) dragging = { id, key: posPoint.key, value: quantize(Number(event.currentTarget.value), range) };
  }
  function setPosition(event) {
    const validTarget = !dragging || sameTarget;
    dragging = null;
    if (!validTarget || !positionWritable || pos == null) return;
    const next = quantize(Number(event.currentTarget.value), range);
    if (next != null && next !== pos) return submit(posPoint.key, next, label);
  }
  function sendOrder(command, key) {
    const spelled = enumChoice(offers, command);
    if (spelled) return submit(order.key, spelled, t('commun.action', { label, action: t(key).toLowerCase() }));
  }
  async function submit(key, next, description) {
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
  // [command sent to the device, catalogue key of its button]
  const BUTTONS = [
    ['open', 'commun.volet.ouvrir'],
    ['stop', 'commun.volet.stop'],
    ['close', 'commun.volet.fermer'],
    ['cycle', 'commun.volet.impulsion'],
  ];
</script>

<div class="cover" class:standalone>
  <Icon name={available ? 'shutter' : 'offline'} size={22} />
  <span class="name">{label}<small class="muted">{status}</small></span>
  {#if !available || (!writableActions.length && !positionWritable)}
    <details class="access">
      <summary>{t(available ? 'commun.volet.lecture_seule' : 'commun.piece.injoignable')}</summary>
      <p>{t(available ? 'commun.volet.lecture_seule_aide' : 'commun.volet.connexion_aide')} <a href="#/systeme">{t('commun.volet.voir_connexion')}</a></p>
    </details>
  {/if}
  {#each writableActions as [cmd, key] (cmd)}
    <button disabled={sending?.key === order.key && sending.value === enumChoice(offers, cmd)} onclick={() => sendOrder(cmd, key)}>{t(key)}</button>
  {/each}
  {#if actions.length && !writableActions.length}
    <p class="observed">{t('commun.volet.actions_observees', { actions: actions.map(([, key]) => t(key)).join(' · ') })}</p>
  {/if}
  {#if positionWritable}
    <label class="position">
      <span>{t('commun.volet.position')}</span>
      <input type="range" min={range.first} max={range.last} step={range.step} value={shown ?? range.first} disabled={pos == null} oninput={previewPosition} onchange={setPosition} aria-label={t('commun.volet.ouverture', { label })} />
    </label>
  {/if}
  {#if feedback}<p class="feedback" role="status">{t(`commun.volet.feedback.${feedback}`)}</p>{/if}
</div>

<style>
  .cover {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 12px 14px;
    background: var(--surface-2);
    border-radius: var(--r-md);
    color: var(--ink-2);
    flex-wrap: wrap;
  }

  .name {
    flex: 1;
    display: grid;
    font-weight: 650;
    color: var(--ink);
    min-width: 120px;
  }

  .standalone { background: var(--surface); border: 1px solid var(--line); }

  small {
    font-weight: 500;
  }

  .access { font-size: 12px; max-width: 100%; }
  .access summary { cursor: pointer; color: var(--ink-2); }
  .access p { max-width: 38ch; margin: 8px 0 0; line-height: 1.5; }
  .access a { color: inherit; text-underline-offset: 3px; }
  .observed { width: 100%; margin: 0; color: var(--ink-3); font-size: 12px; line-height: 1.5; }
  .feedback { width: 100%; margin: 0; color: var(--ink-2); font-size: 12px; }

  button {
    border: 0;
    background: var(--surface);
    border-radius: 999px;
    padding: 8px 12px;
    font-size: 13px;
    font-weight: 650;
    box-shadow: var(--shadow);
  }
  button:disabled { opacity: 0.6; cursor: wait; }

  .position { display: flex; align-items: center; gap: 10px; width: 100%; }
  .position input { flex: 1; min-width: 0; }
</style>
