<script>
  import { value } from '../lib/home.svelte.js';
  import { t, locale } from '../../lib/i18n.svelte.js';

  // A heater's week program (`week_program`: for each day, Monday first, its
  // runs from a quarter of an hour on: an order and a setpoint). On four or
  // six orders the runs are orders; on switch wiring, setpoints.
  let { id, wiring, modeLabel } = $props();

  const SLOTS = 96;
  const week = $derived.by(() => {
    try {
      return JSON.parse(value(id, 'week_program') ?? 'null')?.days ?? null;
    } catch {
      return null;
    }
  });
  const bySetpoint = $derived(wiring === 'switch');
  // On four orders the module shows comfort −1 and −2 as comfort.
  const order = (o) => (wiring === 'four' && o?.startsWith('comfort') ? 'comfort' : o);

  const hhmm = (slot) => `${String(Math.floor(slot / 4)).padStart(2, '0')}:${String((slot % 4) * 15).padStart(2, '0')}`;
  // A day's runs with their end, and what each says.
  const spans = (runs) =>
    (runs ?? []).map((run, i) => {
      const end = runs[i + 1]?.at ?? SLOTS;
      const what = bySetpoint ? `${run.celsius}°` : modeLabel(order(run.order));
      return { ...run, end, what, order: order(run.order) };
    });
  // 2024-01-01 was a Monday: the days' short names in the house's language.
  const dayName = (i) => new Date(2024, 0, 1 + i).toLocaleDateString(locale(), { weekday: 'short' });

  // Now: which day row (Monday first), which quarter of an hour; ticks each minute.
  let now = $state(new Date());
  $effect(() => {
    const tick = setInterval(() => (now = new Date()), 60_000);
    return () => clearInterval(tick);
  });
  const today = $derived((now.getDay() + 6) % 7);
  const slot = $derived(now.getHours() * 4 + Math.floor(now.getMinutes() / 15));
  const current = $derived(spans(week?.[today]).find((s) => s.at <= slot && slot < s.end));

  // Setpoints coloured as the room gauge: blue when cool, red when hot.
  const warmth = (c) => (c < 12 ? 'frost' : c < 18 ? 'cold' : c < 20 ? 'cool' : c <= 24 ? 'good' : c <= 27 ? 'warm' : 'hot');
</script>

{#if week}
  <div class="program">
    {#if current}
      <p class="now">{t('commun.clim.programme_maintenant', { what: current.what, until: hhmm(current.end) })}</p>
    {/if}
    <div class="grid" role="table" aria-label={t('commun.piece.mode.programme')}>
      {#each week as runs, day (day)}
        <div class="row" role="row">
          <span class="day" class:today={day === today} role="rowheader">{dayName(day)}</span>
          <div class="bar">
            {#if runs}
              {#each spans(runs) as s (s.at)}
                <i
                  class={bySetpoint ? warmth(s.celsius) : s.order ?? 'unknown'}
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

<style>
  .program {
    display: grid;
    gap: 6px;
    width: 100%;
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
