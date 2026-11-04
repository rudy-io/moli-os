<script>
  import { device, value, act, nameOf, num } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';
  import { canWrite, numericRange, quantize, writableChoices, positionPoint, coverOrder, enumChoice } from '../lib/capabilities.js';

  /** A shutter: open, stop, close (whatever the device offers). `icons`: the
   *  names and words give way to icons (the rooms page's « icons only » look). */
  let { id, name = null, icons = false } = $props();

  const d = $derived(device(id));
  const label = $derived(nameOf(id, name));
  const posPoint = $derived(positionPoint(d, true));
  const reported = $derived(posPoint ? value(id, posPoint.key) : null);
  const pos = $derived(posPoint?.kind?.type === 'numeric' && Number.isFinite(reported) && reported >= 0 && reported <= 100 ? reported : null);
  const order = $derived(coverOrder(d, true));
  const offers = $derived(writableChoices(d, order?.key));
  const range = $derived(posPoint?.semantic === 'position' ? numericRange(d, posPoint.key, 1, { min: 0, max: 100 }) : null);
  const positionWritable = $derived(!!range && range.min >= 0 && range.max <= 100 && canWrite(d, posPoint.key));
  const moving = $derived(value(id, 'moving'));
  const going = $derived(
    moving === 'UP' ? t('commun.volet.monte')
      : moving === 'DOWN' ? t('commun.volet.descend')
        : moving && String(moving).toLowerCase() !== 'stop' ? t('commun.volet.mouvement') : '',
  );
  let dragging = $state(null);
  const sameTarget = $derived(dragging?.id === id && dragging?.key === posPoint?.key);
  const shown = $derived(positionWritable && sameTarget ? dragging.value : pos);
  const status = $derived([shown != null ? t('commun.volet.ouvert_a', { pct: num(shown) }) : '', going].filter(Boolean).join(' · '));
  function previewPosition(event) {
    if (positionWritable && pos != null) dragging = { id, key: posPoint.key, value: quantize(Number(event.currentTarget.value), range) };
  }
  function setPosition(event) {
    const validTarget = !dragging || sameTarget;
    dragging = null;
    if (!validTarget || !positionWritable || pos == null) return;
    const next = quantize(Number(event.currentTarget.value), range);
    if (next != null && next !== pos) act(`${id}/${posPoint.key}`, next, label);
  }
  function sendOrder(command, key) {
    const spelled = enumChoice(offers, command);
    if (spelled) act(`${id}/${order.key}`, spelled, t('commun.action', { label, action: t(key).toLowerCase() }));
  }
  // [command sent to the device, catalogue key of its button, its icon]
  const BUTTONS = [
    ['open', 'commun.volet.ouvrir', 'chevron-up'],
    ['stop', 'commun.volet.stop', 'stop'],
    ['close', 'commun.volet.fermer', 'chevron-down'],
    ['cycle', 'commun.volet.impulsion', 'power'],
  ];
</script>

<div class="cover" class:icons title={icons ? [label, status].filter(Boolean).join(' · ') : undefined}>
  <Icon name="shutter" size={22} />
  {#if !icons}<span class="name">{label}<small class="muted">{status}</small></span>{/if}
  {#each BUTTONS.filter(([cmd]) => enumChoice(offers, cmd)) as [cmd, key, ico] (cmd)}
    <button onclick={() => sendOrder(cmd, key)} title={icons ? t(key) : undefined} aria-label={icons ? t(key) : undefined}>
      {#if icons}<Icon name={ico} size={18} />{:else}{t(key)}{/if}
    </button>
  {/each}
  {#if positionWritable}
    <label class="position">
      {#if !icons}<span>{t('commun.volet.position')}</span>{/if}
      <input type="range" min={range.first} max={range.last} step={range.step} value={shown ?? range.first} disabled={pos == null} oninput={previewPosition} onchange={setPosition} aria-label={t('commun.volet.ouverture', { label })} />
    </label>
  {/if}
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

  small {
    font-weight: 500;
  }

  button {
    border: 0;
    background: var(--surface);
    border-radius: 999px;
    padding: 8px 12px;
    font-size: 13px;
    font-weight: 650;
    box-shadow: var(--shadow);
  }

  .position { display: flex; align-items: center; gap: 10px; width: 100%; }
  .position input { flex: 1; min-width: 0; }

  /* Icons only: the same row, without a word. */
  .icons {
    padding: 8px 10px;
    gap: 8px;
  }
  .icons button {
    display: grid;
    place-items: center;
    width: 34px;
    height: 34px;
    padding: 0;
  }
  .icons .position {
    flex: 1 1 110px;
    width: auto;
  }
</style>
