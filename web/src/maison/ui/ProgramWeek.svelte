<script>
  import { value, num } from '../lib/home.svelte.js';
  import { parseWeek, spans as spansOf, hhmm, warmth, nowIn, dayName, SLOTS } from '../lib/program.js';
  import { t, locale } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';
  import ProgramDetail from './ProgramDetail.svelte';

  // A heater's week program, as a strip under its modes, or only a link to
  // it (`compact`: the heater runs another mode); the magnifier opens it in
  // full (`modes`: the card's, for their names and glyphs).
  let { id, name, wiring, modeLabel, modes, compact = false } = $props();

  const week = $derived(parseWeek(value(id, 'week_program')));
  const bySetpoint = $derived(wiring === 'switch');
  const spans = (runs) =>
    spansOf(runs, wiring).map((s) => ({ ...s, what: s.celsius != null ? `${num(s.celsius, s.celsius % 1 ? 1 : 0)}°` : modeLabel(s.order) }));
  let open = $state(false);

  // Now: which day row (Monday first), which quarter of an hour; ticks each minute.
  let now = $state(new Date());
  $effect(() => {
    const tick = setInterval(() => (now = new Date()), 60_000);
    return () => clearInterval(tick);
  });
  const today = $derived(nowIn(now).day);
  const slot = $derived(nowIn(now).slot);
  const current = $derived(spans(week?.[today]).find((s) => s.at <= slot && slot < s.end));
</script>

{#if week && compact}
  <button class="link" onclick={() => (open = true)}>
    <Icon name="magnify" size={16} />{t('commun.programme.titre')}
  </button>
{:else if week}
  <div class="program">
    <div class="head">
      {#if current}
        <p class="now">{t('commun.clim.programme_maintenant', { what: current.what, until: hhmm(current.end) })}</p>
      {/if}
      <button class="zoom" onclick={() => (open = true)} title={t('commun.programme.detail')} aria-label={t('commun.programme.detail')}>
        <Icon name="magnify" size={18} />
      </button>
    </div>
    <div class="grid" role="table" aria-label={t('commun.piece.mode.programme')}>
      {#each week as runs, day (day)}
        <div class="row" role="row">
          <span class="day" class:today={day === today} role="rowheader">{dayName(day, locale())}</span>
          <div class="bar">
            {#if runs}
              {#each spans(runs) as s (s.at)}
                <i
                  class={bySetpoint && s.celsius != null ? warmth(s.celsius) : (s.order ?? 'unknown')}
                  style="width:{((s.end - s.at) / SLOTS) * 100}%"
                  title="{hhmm(s.at)}–{hhmm(s.end)} · {s.what}"
                  role="cell"></i>
              {/each}
            {/if}
            {#if day === today}
              <b class="here" style="left:{(slot / SLOTS) * 100}%"></b>
            {/if}
          </div>
        </div>
      {/each}
      <div class="row hours" aria-hidden="true">
        <span class="day"></span>
        <div class="ticks">
          {#each [0, 6, 12, 18, 24] as h (h)}<span style="left:{(h / 24) * 100}%">{h}</span>{/each}
        </div>
      </div>
    </div>
  </div>
{/if}
{#if week && open}
  <ProgramDetail {id} {name} {wiring} {modeLabel} {modes} onclose={() => (open = false)} />
{/if}

<style>
  .program {
    display: grid;
    gap: 6px;
    width: 100%;
  }

  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
  }

  .link {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 5px 12px 5px 9px;
    border: none;
    border-radius: 999px;
    background: var(--surface-2);
    color: var(--ink-2);
    font: inherit;
    font-size: 12.5px;
    font-weight: 650;
    cursor: pointer;
  }

  .zoom {
    display: grid;
    place-items: center;
    width: 30px;
    height: 30px;
    flex: none;
    margin-left: auto;
    border: none;
    border-radius: 50%;
    background: var(--surface-2);
    color: var(--ink-2);
    cursor: pointer;
  }

  .now {
    font-size: 12.5px;
    font-weight: 650;
    color: var(--ink-2);
  }

  .grid {
    display: grid;
    gap: 3px;
  }

  .row {
    display: grid;
    grid-template-columns: 34px 1fr;
    align-items: center;
    gap: 6px;
  }

  .day {
    font-size: 11px;
    color: var(--ink-3);
    text-transform: capitalize;
  }

  .day.today {
    color: var(--ink);
    font-weight: 700;
  }

  .bar {
    position: relative;
    display: flex;
    height: 12px;
    border-radius: 4px;
    overflow: hidden;
    background: var(--surface-2);
  }

  .bar i {
    height: 100%;
  }

  /* Orders (four or six). */
  .comfort {
    background: var(--warm);
  }

  .comfort_1 {
    background: color-mix(in srgb, var(--warm) 70%, var(--surface-2));
  }

  .comfort_2 {
    background: color-mix(in srgb, var(--warm) 45%, var(--surface-2));
  }

  .eco {
    background: var(--good);
  }

  .hors {
    background: var(--cool);
  }

  .off,
  .unknown {
    background: var(--ink-3);
  }

  /* Setpoints (switch). */
  .frost {
    background: #4f8cff;
  }

  .cold {
    background: #3fc1d9;
  }

  .cool {
    background: #7fd3b0;
  }

  .good {
    background: #4cc26a;
  }

  .warm {
    background: #ffb340;
  }

  .hot {
    background: #ff5a3c;
  }

  .here {
    position: absolute;
    top: -2px;
    bottom: -2px;
    width: 2px;
    background: var(--ink);
    transform: translateX(-50%);
  }

  .ticks {
    position: relative;
    height: 12px;
  }

  .ticks span {
    position: absolute;
    font-size: 10px;
    color: var(--ink-3);
    transform: translateX(-50%);
  }
</style>
