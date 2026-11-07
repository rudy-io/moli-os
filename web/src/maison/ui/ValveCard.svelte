<script>
  import { onDestroy } from 'svelte';
  import { device, value, reachable, num, act } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';
  import ScheduleEditor from './ScheduleEditor.svelte';
  import ValveChart from './ValveChart.svelte';
  import { scheduleSpec } from '../lib/schedule.js';
  import { pointOf, canWrite, numericRange, quantize, writableChoices, climateKeys } from '../lib/capabilities.js';
  import { completionGuard } from '../lib/completion-guard.js';

  // A Zigbee thermostatic valve: its setpoint and its mode, with the temperature
  // it measures for context. Every order goes through `act`, the hub's normal path and the guard.
  let { id, name } = $props();

  const d = $derived(device(id));
  const keys = $derived(climateKeys(d));
  const unit = $derived(pointOf(d, keys.target)?.unit);
  const range = $derived(numericRange(d, keys.target, 0.5));
  const modes = $derived(writableChoices(d, keys.mode));
  const temp = $derived(value(id, keys.temperature));
  const target = $derived(value(id, keys.target));
  const mode = $derived(value(id, keys.mode));
  const writable = $derived(!!range && canWrite(d, keys.target));
  const hasSchedule = $derived(!!scheduleSpec(d));
  let editing = $state(false);
  let charting = $state(false);

  // Taps on − / + add up; one order leaves once the finger rests.
  let wanted = $state(null);
  let timer;
  let resetTimer;
  const completion = completionGuard();
  onDestroy(() => { completion.change(); clearTimeout(timer); clearTimeout(resetTimer); });
  const shown = $derived(wanted ?? target);

  function nudge(delta) {
    if (!writable || !reachable(id)) return;
    const base = wanted ?? target;
    if (!Number.isFinite(base)) return;
    completion.change();
    wanted = quantize(base + delta, range);
    clearTimeout(timer);
    const forId = id;
    timer = setTimeout(() => (id === forId ? flush() : (wanted = null)), 900);
  }

  async function flush() {
    const v = wanted;
    if (!writable || !reachable(id) || v == null || v === target || quantize(v, range) !== v) {
      wanted = null;
      return;
    }
    const owns = completion.capture();
    const done = await act(`${id}/${keys.target}`, v, t('commun.clim.label_consigne', { name }));
    if (!owns()) return;
    // Shown until the valve reports it (or gives up on it).
    if (done !== 'ok') wanted = null;
    else {
      clearTimeout(resetTimer);
      resetTimer = setTimeout(() => owns() && wanted === v && (wanted = null), 15_000);
    }
  }

  $effect(() => {
    if (wanted != null && target === wanted) wanted = null;
  });

  $effect(() => {
    // A pending gesture belongs to the valve it was made on.
    void id;
    completion.change();
    clearTimeout(timer);
    clearTimeout(resetTimer);
    wanted = null;
  });

  /** A mode said in the house's language (an unknown one: as the valve says it). */
  const MODE_LABELS = { off: 'commun.vanne.mode.off', auto: 'commun.vanne.mode.auto', heat: 'commun.vanne.mode.heat' };
  const modeLabel = (key) => (MODE_LABELS[key] ? t(MODE_LABELS[key]) : (key ?? '—'));
  const setMode = (key) => modes.includes(key) && act(`${id}/${keys.mode}`, key, t('commun.action', { label: name, action: modeLabel(key).toLowerCase() }));
</script>

<div class="valve" class:unreachable={!reachable(id)}>
  <div class="head">
    <h3><Icon name="fire" size={16} />{name}</h3>
    {#if Number.isFinite(temp)}
      <b class="num temp" title={t('commun.clim.temp_piece')}>{num(temp, 1)}{pointOf(d, keys.temperature)?.unit ?? ''}</b>
    {/if}
  </div>
  {#if Number.isFinite(shown) && pointOf(d, keys.target)}
    {#if writable}
      <div class="setpoint" role="group" aria-label={t('commun.clim.consigne')}>
        <button onclick={() => nudge(-range.step)} aria-label={t('commun.clim.moins_chaud')} disabled={shown <= range.first}>−</button>
        <b class="num">{num(shown, shown % 1 ? 1 : 0)}{unit ?? ''}</b>
        <button onclick={() => nudge(range.step)} aria-label={t('commun.clim.plus_chaud')} disabled={shown >= range.last}>+</button>
      </div>
    {:else}
      <p class="muted">{t('commun.clim.consigne_val', { temp: num(shown, shown % 1 ? 1 : 0), unit: unit ?? '' })}</p>
    {/if}
  {/if}
  {#if modes.length}
    <div class="modes" role="group" aria-label={t('commun.clim.mode')}>
      {#each modes as key (key)}
        <button class:active={mode === key} onclick={() => setMode(key)} aria-pressed={mode === key}>{modeLabel(key)}</button>
      {/each}
    </div>
  {:else if mode != null}
    <p class="muted">{t('commun.clim.mode')} <b>{modeLabel(mode)}</b></p>
  {/if}
  {#if pointOf(d, keys.temperature) || pointOf(d, keys.target)}
    <button class="fold" onclick={() => (charting = !charting)} aria-expanded={charting}>{t('commun.vanne.courbe.titre')}</button>
    {#if charting}
      <ValveChart {id} {name} />
    {/if}
  {/if}
  {#if hasSchedule}
    <button class="fold" onclick={() => (editing = !editing)} aria-expanded={editing}>{t('commun.vanne.prog.titre')}</button>
    {#if editing}
      <ScheduleEditor {id} {name} />
    {/if}
  {/if}
</div>

<style>
  .valve {
    display: grid;
    gap: 8px;
    padding: 6px 4px;
  }

  .valve.unreachable {
    opacity: 0.55;
  }

  .head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 10px;
  }

  h3 {
    display: flex;
    align-items: center;
    gap: 6px;
    margin: 0;
    font-size: 15px;
  }

  .temp {
    font-size: 22px;
  }

  .setpoint {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .setpoint button,
  .modes button {
    border: none;
    background: var(--surface-2);
    color: var(--ink);
    font: inherit;
    cursor: pointer;
  }

  .setpoint button {
    width: 34px;
    height: 34px;
    border-radius: 50%;
    font-size: 20px;
    line-height: 1;
  }

  .setpoint button:disabled {
    opacity: 0.35;
    cursor: default;
  }

  .setpoint b {
    min-width: 52px;
    text-align: center;
    font-size: 20px;
  }

  .modes {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }

  .modes button {
    padding: 6px 12px;
    border-radius: 999px;
    font-size: 13px;
    color: var(--ink-3);
  }

  .modes button.active {
    background: var(--ink);
    color: var(--surface);
  }

  .fold {
    justify-self: start;
    border: none;
    background: none;
    padding: 0;
    color: var(--ink-2);
    font: inherit;
    font-size: 12.5px;
    text-decoration: underline;
    cursor: pointer;
  }

  .muted {
    margin: 0;
    font-size: 12.5px;
    color: var(--ink-2);
  }
</style>
