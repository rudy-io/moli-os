<script>
  import { onDestroy } from 'svelte';
  import { device, value, reachable, act, num } from '../lib/home.svelte.js';
  import { t, locale } from '../../lib/i18n.svelte.js';
  import { DAYS, GRID, scheduleSpec, parseDay, formatDay, sameDay, hhmm } from '../lib/schedule.js';
  import { bands, warmth, minuteAt, dragBoundary, now, MINUTES_IN_DAY } from '../lib/week.js';

  // A thermostat's whole week, one row a day across 24 h, a colour per setpoint.
  // A boundary between two steps is dragged (or moved with the arrow keys) and the
  // day is written when it is let go: an order like any other, through `act`.
  // Choosing a row tells the editor below which day to open (`day`).
  let { id, name, day = $bindable() } = $props();

  const spec = $derived(scheduleSpec(device(id)));
  const days = $derived(spec ? DAYS.filter((d) => spec.days.includes(d)) : []);
  const stored = (d) => value(id, `weekly_schedule_${d}`);

  // A day being moved, shown as it will be until the thermostat says it is.
  let drafts = $state({});
  let drag = null;
  const timers = {};
  onDestroy(() => Object.values(timers).forEach(clearTimeout));

  const stepsOf = (d) => drafts[d] ?? parseDay(spec, stored(d));
  const bandsOf = (d) => bands(spec, formatDay(spec, stepsOf(d)));

  // The clock moves the "now" line once a minute.
  let clock = $state(now());
  const tick = setInterval(() => (clock = now()), 60_000);
  onDestroy(() => clearInterval(tick));

  $effect(() => {
    for (const d of Object.keys(drafts)) {
      if (sameDay(spec, drafts[d], parseDay(spec, stored(d)))) delete drafts[d];
    }
  });

  const dayName = (key) =>
    new Date(2024, 0, 1 + DAYS.indexOf(key)).toLocaleDateString(locale(), { weekday: 'short' }).replace('.', '');
  const deg = (v) => `${num(v, v % 1 ? 1 : 0)}°`;
  const pct = (min) => `${(min / MINUTES_IN_DAY) * 100}%`;
  const fill = (v) => `color-mix(in oklab, color-mix(in oklab, var(--warm) ${warmth(v)}%, var(--cool)) 62%, var(--surface))`;

  async function commit(d) {
    const steps = drafts[d];
    if (!steps || !spec) return;
    if (sameDay(spec, steps, parseDay(spec, stored(d))) || !reachable(id)) {
      delete drafts[d];
      return;
    }
    const outcome = await act(`${id}/weekly_schedule_${d}`, formatDay(spec, steps), t('commun.vanne.prog.label', { name }));
    if (outcome !== 'ok') delete drafts[d];
    else timers[d] = setTimeout(() => delete drafts[d], 15_000);
  }

  function grab(e, d, index) {
    if (!reachable(id)) return;
    e.preventDefault();
    e.stopPropagation();
    e.currentTarget.setPointerCapture(e.pointerId);
    clearTimeout(timers[d]);
    drag = { day: d, index, bar: e.currentTarget.parentElement, from: stepsOf(d) };
    day = d;
  }

  function move(e) {
    if (!drag) return;
    const box = drag.bar.getBoundingClientRect();
    drafts[drag.day] = dragBoundary(spec, drag.from, drag.index, minuteAt(e.clientX - box.left, box.width));
  }

  function release() {
    if (!drag) return;
    const d = drag.day;
    drag = null;
    commit(d);
  }

  // Arrow keys move a boundary by the grid; the day leaves once the keys rest.
  function key(e, d, index) {
    const delta = { ArrowLeft: -GRID, ArrowRight: GRID, ArrowDown: -GRID, ArrowUp: GRID }[e.key];
    if (!delta || !reachable(id)) return;
    e.preventDefault();
    const steps = stepsOf(d);
    drafts[d] = dragBoundary(spec, steps, index, steps[index].min + delta);
    clearTimeout(timers[d]);
    timers[d] = setTimeout(() => commit(d), 900);
  }
</script>

{#if spec}
  <div class="week" class:unreachable={!reachable(id)}>
    <div class="hours" aria-hidden="true">
      <span></span>
      <div class="ticks">
        {#each [0, 6, 12, 18, 24] as h (h)}
          <small style:left={pct(h * 60)}>{h}h</small>
        {/each}
      </div>
    </div>
    {#each days as d (d)}
      <div class="row" class:chosen={d === day}>
        <button class="label" aria-pressed={d === day} onclick={() => (day = d)}>{dayName(d)}</button>
        <div class="bar">
          {#each bandsOf(d) as b (b.start)}
            <div
              class="band"
              style:left={pct(b.start)}
              style:width={pct(b.end - b.start)}
              style:background={fill(b.deg)}
              title={t('maison.chauffage.semaine.creneau', { from: hhmm(b.start), to: hhmm(b.end % MINUTES_IN_DAY), temp: deg(b.deg) })}
            >
              {#if b.end - b.start >= 110}<span class="num">{deg(b.deg)}</span>{/if}
            </div>
          {/each}
          {#if d === clock.day}
            <div class="now" style:left={pct(clock.minute)} title={t('maison.chauffage.semaine.maintenant')}></div>
          {/if}
          {#each stepsOf(d) as s, i (i)}
            {#if i > 0}
              <button
                class="edge"
                style:left={pct(s.min)}
                aria-label={t('maison.chauffage.semaine.limite', { day: dayName(d), time: hhmm(s.min), temp: deg(s.deg) })}
                onpointerdown={(e) => grab(e, d, i)}
                onpointermove={move}
                onpointerup={release}
                onpointercancel={release}
                onkeydown={(e) => key(e, d, i)}
              ><span class="num">{hhmm(s.min)}</span></button>
            {/if}
          {/each}
        </div>
      </div>
    {/each}
    <p class="hint">{t('maison.chauffage.semaine.aide')}</p>
  </div>
{/if}

<style>
  .week {
    display: grid;
    gap: 6px;
  }

  .week.unreachable {
    opacity: 0.55;
  }

  .hours,
  .row {
    display: grid;
    grid-template-columns: 44px minmax(0, 1fr);
    align-items: center;
    gap: 8px;
  }

  .ticks {
    position: relative;
    height: 14px;
  }

  .ticks small {
    position: absolute;
    transform: translateX(-50%);
    font-size: 10.5px;
    color: var(--ink-3);
  }

  .ticks small:first-child {
    transform: none;
  }

  .ticks small:last-child {
    transform: translateX(-100%);
  }

  .label {
    border: none;
    background: none;
    color: var(--ink-2);
    font: inherit;
    font-size: 12.5px;
    font-weight: 650;
    text-transform: capitalize;
    text-align: left;
    padding: 4px 0;
    cursor: pointer;
  }

  .row.chosen .label {
    color: var(--ink);
  }

  .bar {
    position: relative;
    height: 34px;
    border-radius: 10px;
    overflow: hidden;
    background: var(--surface);
    touch-action: none;
  }

  .row.chosen .bar {
    outline: 2px solid var(--ink);
    outline-offset: 1px;
  }

  .band {
    position: absolute;
    top: 0;
    bottom: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    overflow: hidden;
    color: var(--ink);
    font-size: 12px;
    font-weight: 650;
  }

  .now {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 2px;
    margin-left: -1px;
    background: var(--ink);
    pointer-events: none;
  }

  .edge {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 18px;
    margin-left: -9px;
    padding: 0;
    border: none;
    background: none;
    cursor: ew-resize;
    touch-action: none;
  }

  .edge::before {
    content: '';
    position: absolute;
    top: 6px;
    bottom: 6px;
    left: 8px;
    width: 2px;
    border-radius: 1px;
    background: var(--surface);
    box-shadow: 0 0 0 1px color-mix(in oklab, var(--ink) 25%, transparent);
  }

  .edge span {
    position: absolute;
    left: 50%;
    bottom: 100%;
    transform: translateX(-50%);
    font-size: 10.5px;
    color: var(--ink);
    background: var(--surface);
    padding: 0 3px;
    border-radius: 4px;
    opacity: 0;
    pointer-events: none;
  }

  .edge:hover span,
  .edge:focus-visible span,
  .edge:active span {
    opacity: 1;
    bottom: auto;
    top: 2px;
  }

  .edge:focus-visible {
    outline: 2px solid var(--ink);
    outline-offset: -2px;
  }

  .hint {
    margin: 2px 0 0;
    font-size: 12px;
    color: var(--ink-3);
  }
</style>
