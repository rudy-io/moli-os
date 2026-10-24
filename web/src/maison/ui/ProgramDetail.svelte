<script>
  // A heater's week program in full, opened from its card's strip: what runs
  // now, what each colour means, the week in large bars that say their order
  // (its glyph, as on the card's buttons) or their setpoint, and a day's runs
  // written out.
  import { onMount } from 'svelte';
  import { value, num } from '../lib/home.svelte.js';
  import { parseWeek, spans, hhmm, warmth, nowIn, dayName, SLOTS } from '../lib/program.js';
  import { t, locale } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';

  let { id, name, wiring, modeLabel, modes, onclose } = $props();

  let dialog;
  onMount(() => dialog.showModal());

  const week = $derived(parseWeek(value(id, 'week_program')));
  const bySetpoint = $derived(wiring === 'switch');
  const deg = (c) => num(c, c % 1 ? 1 : 0);
  // What a run says, and the class that colours it.
  const what = (s) => (s.celsius != null ? `${deg(s.celsius)} °C` : modeLabel(s.order));
  const tint = (s) => (s.celsius != null ? warmth(s.celsius) : (s.order ?? 'unknown'));

  // The orders a program holds on this wiring, warmest first, and what each
  // means on a pilot wire.
  const ORDERS = { four: ['comfort', 'eco', 'hors'], six: ['comfort', 'comfort_1', 'comfort_2', 'eco', 'hors'] };
  const HINTS = {
    comfort_1: 'commun.programme.aide_confort_1',
    comfort_2: 'commun.programme.aide_confort_2',
    eco: 'commun.programme.aide_eco',
    hors: 'commun.programme.aide_hors',
  };
  // On switch wiring: the setpoints the week holds, warmest first.
  const setpoints = $derived(
    bySetpoint
      ? [...new Set((week ?? []).flatMap((runs) => spans(runs, wiring).map((s) => s.celsius)).filter((c) => c != null))].sort((a, b) => b - a)
      : [],
  );

  let now = $state(new Date());
  $effect(() => {
    const tick = setInterval(() => (now = new Date()), 60_000);
    return () => clearInterval(tick);
  });
  const today = $derived(nowIn(now).day);
  const slot = $derived(nowIn(now).slot);
  const current = $derived(spans(week?.[today], wiring).find((s) => s.at <= slot && slot < s.end));
  // The day written out: today until another is tapped.
  let picked = $state(null);
  const shown = $derived(picked ?? today);

  // A run wide enough says what it is inside its bar (a setpoint needs more room).
  const roomy = (s) => s.end - s.at >= (s.celsius != null ? 8 : 5);
</script>

<dialog bind:this={dialog} {onclose} onclick={(e) => e.target === dialog && dialog.close()} aria-label={t('commun.programme.titre')}>
  <header>
    <div>
      <h2>{name}</h2>
      <p class="sub">{t('commun.programme.titre')}</p>
    </div>
    <button class="round" onclick={() => dialog.close()} aria-label={t('commun.fermer')}><Icon name="close" size={22} /></button>
  </header>

  {#if current}
    <p class="now">
      <i class="swatch {tint(current)}"></i>
      {t('commun.programme.maintenant', { what: what(current), until: hhmm(current.end) })}
    </p>
  {/if}

  <section class="legend" aria-label={t('commun.programme.legende')}>
    {#if bySetpoint}
      <p class="muted">{t('commun.programme.couleur_temp')}</p>
      <ul>
        {#each setpoints as c (c)}
          <li><i class="swatch {warmth(c)}"></i><b class="num">{deg(c)} °C</b></li>
        {/each}
      </ul>
    {:else}
      <ul>
        {#each ORDERS[wiring] ?? [] as key (key)}
          <li>
            <i class="swatch {key}">
              {#if modes[key]?.text}<span class="num">{modes[key].text}</span>{:else}<Icon name={modes[key]?.icon ?? 'tune'} size={13} />{/if}
            </i>
            <b>{modeLabel(key)}</b>
            {#if HINTS[key]}<small>{t(HINTS[key])}</small>{/if}
          </li>
        {/each}
      </ul>
    {/if}
  </section>

  {#if week}
    <div class="week">
      {#each week as runs, day (day)}
        <button class="row" class:picked={day === shown} onclick={() => (picked = day)} aria-pressed={day === shown}>
          <span class="day" class:today={day === today}><span class="long">{dayName(day, locale(), 'long')}</span><span class="short">{dayName(day, locale())}</span></span>
          <span class="bar">
            {#each spans(runs, wiring) as s (s.at)}
              <i class="run {tint(s)}" style="width:{((s.end - s.at) / SLOTS) * 100}%" title="{hhmm(s.at)}–{hhmm(s.end)} · {what(s)}">
                {#if roomy(s)}
                  {#if s.celsius != null}<span class="num">{deg(s.celsius)}°</span>
                  {:else if modes[s.order]?.text}<span class="num">{modes[s.order].text}</span>
                  {:else if modes[s.order]}<Icon name={modes[s.order].icon} size={14} />{/if}
                {/if}
              </i>
            {/each}
            {#if day === today}<b class="here" style="left:{(slot / SLOTS) * 100}%"></b>{/if}
          </span>
        </button>
      {/each}
      <div class="row hours" aria-hidden="true">
        <span class="day"></span>
        <span class="ticks">
          {#each [0, 3, 6, 9, 12, 15, 18, 21, 24] as h (h)}<span class:minor={h % 6} style="left:{(h / 24) * 100}%">{h}h</span>{/each}
        </span>
      </div>
    </div>

    <section class="runs">
      <h3>{dayName(shown, locale(), 'long')}</h3>
      {#if week[shown]}
        <ol>
          {#each spans(week[shown], wiring) as s (s.at)}
            <li class:live={shown === today && s.at <= slot && slot < s.end}>
              <span class="num">{hhmm(s.at)} – {hhmm(s.end)}</span>
              <i class="swatch {tint(s)}"></i>
              <b>{what(s)}</b>
            </li>
          {/each}
        </ol>
      {:else}
        <p class="muted">{t('commun.programme.jour_vide')}</p>
      {/if}
      <p class="muted hint">{t('commun.programme.toucher_jour')}</p>
    </section>
  {/if}
</dialog>

<style>
  dialog {
    border: 0;
    padding: 18px;
    width: min(760px, 96vw);
    max-height: 94dvh;
    overflow-y: auto;
    border-radius: var(--r-xl);
    background: var(--surface);
    color: var(--ink);
    box-shadow: 0 30px 80px rgb(0 0 0 / 40%);
  }

  dialog::backdrop {
    background: rgb(4 6 12 / 60%);
    backdrop-filter: blur(4px);
  }

  header {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 12px;
    margin-bottom: 14px;
  }

  h2 {
    font-size: 22px;
    font-weight: 750;
  }

  .sub {
    font-size: 13px;
    color: var(--ink-3);
    font-weight: 600;
  }

  .round {
    width: 40px;
    height: 40px;
    flex: none;
    border: 0;
    border-radius: 50%;
    display: grid;
    place-items: center;
    background: var(--surface-2);
    color: var(--ink);
    cursor: pointer;
  }

  .now {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 10px 14px;
    margin-bottom: 14px;
    border-radius: var(--r-lg);
    background: var(--surface-2);
    font-weight: 650;
  }

  .legend {
    display: grid;
    gap: 6px;
    margin-bottom: 16px;
  }

  .legend ul {
    display: flex;
    flex-wrap: wrap;
    gap: 8px 16px;
    list-style: none;
    padding: 0;
    margin: 0;
  }

  .legend li {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 13.5px;
  }

  .legend small {
    color: var(--ink-3);
  }

  .muted {
    font-size: 12.5px;
    color: var(--ink-3);
  }

  .swatch {
    display: inline-grid;
    place-items: center;
    width: 26px;
    height: 20px;
    flex: none;
    border-radius: 6px;
    color: #fff;
    font-style: normal;
    font-size: 11.5px;
    font-weight: 750;
  }

  .now .swatch {
    width: 14px;
    height: 14px;
    border-radius: 50%;
  }

  .week {
    display: grid;
    gap: 4px;
  }

  .row {
    display: grid;
    grid-template-columns: 52px 1fr;
    align-items: center;
    gap: 8px;
    padding: 0;
    border: 0;
    background: none;
    color: inherit;
    font: inherit;
    text-align: left;
    cursor: pointer;
  }

  .day {
    font-size: 12.5px;
    color: var(--ink-3);
    text-transform: capitalize;
  }

  .day.today {
    color: var(--ink);
    font-weight: 750;
  }

  .row.picked .bar {
    outline: 2px solid var(--ink-2);
    outline-offset: 1px;
  }

  .bar {
    position: relative;
    display: flex;
    height: 30px;
    border-radius: 8px;
    overflow: hidden;
    background: var(--surface-2);
  }

  .run {
    display: grid;
    place-items: center;
    height: 100%;
    color: #fff;
    font-style: normal;
    font-size: 12px;
    font-weight: 750;
    box-shadow: inset -1px 0 0 rgb(0 0 0 / 12%);
    overflow: hidden;
  }

  /* Orders (four or six), as the card's strip. */
  .comfort {
    background: var(--warm);
  }

  .comfort_1 {
    background: color-mix(in srgb, var(--warm) 70%, var(--surface-2));
  }

  .comfort_2 {
    background: color-mix(in srgb, var(--warm) 45%, var(--surface-2));
    color: var(--warm-ink);
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

  /* Setpoints (switch), as the card's strip. */
  .frost {
    background: #4f8cff;
  }

  .cold {
    background: #3fc1d9;
  }

  .cool {
    background: #7fd3b0;
    color: #0d3b2c;
  }

  .good {
    background: #4cc26a;
  }

  .warm {
    background: #ffb340;
    color: #5a3300;
  }

  .hot {
    background: #ff5a3c;
  }

  .here {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 3px;
    background: var(--ink);
    box-shadow: 0 0 0 1px var(--surface);
    transform: translateX(-50%);
  }

  .ticks {
    position: relative;
    height: 16px;
  }

  .ticks span {
    position: absolute;
    font-size: 11px;
    color: var(--ink-3);
    transform: translateX(-50%);
  }

  /* On a phone the hours every six only. */
  @media (max-width: 520px) {
    .ticks span.minor {
      display: none;
    }

    .row {
      grid-template-columns: 38px 1fr;
    }

    .day .long {
      display: none;
    }
  }

  @media (min-width: 521px) {
    .day .short {
      display: none;
    }
  }

  .runs {
    margin-top: 18px;
    display: grid;
    gap: 8px;
  }

  h3 {
    font-size: 16px;
    font-weight: 750;
    text-transform: capitalize;
  }

  ol {
    display: grid;
    gap: 4px;
    list-style: none;
    padding: 0;
    margin: 0;
  }

  ol li {
    display: grid;
    grid-template-columns: 112px 26px 1fr;
    align-items: center;
    gap: 10px;
    padding: 6px 10px;
    border-radius: 10px;
    font-size: 14px;
  }

  ol li.live {
    background: var(--surface-2);
    font-weight: 650;
  }

  .hint {
    margin-top: 4px;
  }
</style>
