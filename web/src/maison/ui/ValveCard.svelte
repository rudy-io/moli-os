<script>
  import { onDestroy } from 'svelte';
  import { device, value, reachable, num, act, clock, home } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import ScheduleEditor from './ScheduleEditor.svelte';
  import ValveChart from './ValveChart.svelte';
  import { scheduleSpec } from '../lib/schedule.js';
  import { pointOf, canWrite, numericRange, quantize, writableChoices, climateKeys } from '../lib/capabilities.js';
  import { completionGuard } from '../lib/completion-guard.js';
  import RadiatorTile from './RadiatorTile.svelte';
  import { temporaryOf, boostChoices, hoursMinutes } from '../lib/temporary.js';

  // A thermostat that only heats: a Zigbee valve, a valve on another protocol,
  // or a room's thermostat kept by Moli (it drives the room's valves). Its
  // radiator drawn on top (`RadiatorTile`), then its setpoint and its mode. Every order goes through `act`, the hub's normal path and the guard.
  // `schedule`: false where the schedule is edited beside the card (the Chauffage page).
  // `bare`: under a radiator's tile, which already says its name, what it does and the hold.
  let { id, name, schedule = true, bare = false } = $props();

  const d = $derived(device(id));
  const keys = $derived(climateKeys(d));
  const unit = $derived(pointOf(d, keys.target)?.unit);
  const range = $derived(numericRange(d, keys.target, 0.5));
  const modes = $derived(writableChoices(d, keys.mode));
  const target = $derived(value(id, keys.target));
  const mode = $derived(value(id, keys.mode));
  const writable = $derived(!!range && canWrite(d, keys.target));
  const spec = $derived(scheduleSpec(d));
  const hasSchedule = $derived(!!spec);
  // Without a schedule, `heat` is just on.
  const scheduled = $derived(!!pointOf(d, keys.mode)?.kind?.values?.includes('auto'));
  // A thermostat that states hundredths (Matter) is still nudged by half a degree.
  const step = $derived(range ? Math.max(range.step, 0.5) : 0.5);

  // A room thermostat of Moli: a valve turned by hand is left as it is for a while.
  const override = $derived(pointOf(d, 'valve_override') ? value(id, 'valve_override') : null);
  // A room thermostat of Moli: a boost for a while, a setpoint held for a while.
  const boostable = $derived(canWrite(d, 'boost') && mode !== 'off');
  const boosts = $derived(boostChoices(pointOf(d, 'boost')?.kind?.max));
  const temporary = $derived(temporaryOf((key) => value(id, key), home.now));
  const left = (minutes) => {
    const { h, m } = hoursMinutes(minutes);
    return h ? t('commun.vanne.temporaire.reste_h', { h, m }) : t('commun.vanne.temporaire.reste_min', { m });
  };
  const boost = (by) => act(`${id}/boost`, by, t('commun.vanne.boost.action', { name }));
  const boostFor = (hours) => act(`${id}/boost_minutes`, hours * 60, t('commun.vanne.boost.action', { name }));
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
  const modeLabel = (key) => (key === 'heat' && !scheduled ? t('commun.vanne.mode.marche') : MODE_LABELS[key] ? t(MODE_LABELS[key]) : (key ?? '—'));
  const setMode = (key) => modes.includes(key) && act(`${id}/${keys.mode}`, key, t('commun.action', { label: name, action: modeLabel(key).toLowerCase() }));
</script>

<div class="valve" class:unreachable={!reachable(id)}>
  {#if !bare}<RadiatorTile {id} />{/if}
  {#if Number.isFinite(shown) && pointOf(d, keys.target)}
    {#if writable}
      <div class="setpoint" role="group" aria-label={t('commun.clim.consigne')}>
        <button onclick={() => nudge(-step)} aria-label={t('commun.clim.moins_chaud')} disabled={shown <= range.first}>−</button>
        <b class="num">{num(shown, shown % 1 ? 1 : 0)}{unit ?? ''}</b>
        <button onclick={() => nudge(step)} aria-label={t('commun.clim.plus_chaud')} disabled={shown >= range.last}>+</button>
      </div>
    {:else}
      <p class="muted">{t('commun.clim.consigne_val', { temp: num(shown, shown % 1 ? 1 : 0), unit: unit ?? '' })}</p>
    {/if}
  {/if}
  {#if boostable && reachable(id)}
    <div class="boost" role="group" aria-label={t('commun.vanne.boost.titre')}>
      {#each boosts as by (by)}
        <button class:active={temporary.boost?.by === by} onclick={() => boost(by)} aria-pressed={temporary.boost?.by === by} aria-label={t('commun.vanne.boost.label', { deg: by })}>{t('commun.vanne.boost.plus', { deg: by })}</button>
      {/each}
      {#if temporary.boost}
        {#each [1, 2] as hours (hours)}
          <button onclick={() => boostFor(hours)} aria-label={t('commun.vanne.boost.pendant', { h: hours })}>{t('commun.vanne.boost.heures', { h: hours })}</button>
        {/each}
        <button class="cancel" onclick={() => boost(0)}>{t('commun.vanne.boost.annuler')}</button>
      {/if}
    </div>
    {#if temporary.boost}
      <p class="muted num">{t('commun.vanne.boost.en_cours', { deg: num(temporary.boost.by, temporary.boost.by % 1 ? 1 : 0), heure: clock(temporary.boost.until), reste: left(temporary.boost.left) })}</p>
    {/if}
  {/if}
  {#if temporary.hold && mode === 'auto'}
    <p class="muted num">
      {t('commun.vanne.maintien.en_cours', { heure: clock(temporary.hold.until), reste: left(temporary.hold.left) })}
      <button class="fold" onclick={() => setMode('auto')}>{t('commun.vanne.reglage.reprendre')}</button>
    </p>
  {/if}
  {#if override === 'none' && mode === 'auto' && !temporary.hold}
    <p class="muted">{t('commun.vanne.reglage.programme')}</p>
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
  {#if hasSchedule && schedule}
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

  .setpoint {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .setpoint button,
  .modes button,
  .boost button {
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

  .boost {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }

  .boost button {
    padding: 6px 12px;
    border-radius: 999px;
    font-size: 13px;
    color: var(--ink-2);
  }

  .boost button.cancel {
    background: none;
    text-decoration: underline;
  }

  .boost button.active,
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
