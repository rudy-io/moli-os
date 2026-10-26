<script>
  import { device, value, num } from '../lib/home.svelte.js';
  import { drinkName } from '../lib/coffee.js';
  import { cupCounters, daily, byDrink, average } from '../lib/coffee-usage.js';
  import { t, locale } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';
  import DrinkIcon from './DrinkIcon.svelte';

  /** The coffee drunk, as the electricity page does it but simpler: the
   *  cups of today (or of the day touched), the average a day, a bar a day
   *  over 7 or 30 days, and the cups by drink. From the machine's counters
   *  in the history (`lib/coffee-usage.js`); read again when a drink is
   *  counted. */
  let { id } = $props();

  let span = $state(7);
  let series = $state(null);
  let error = $state(null);
  const counters = $derived(cupCounters(device(id)?.points));
  // A drink counted: read again (the history has it a moment later).
  const total = $derived(value(id, 'brews_total'));
  $effect(() => {
    const keys = counters;
    const hours = span * 24 + 24;
    void total;
    let gone = false;
    const late = setTimeout(async () => {
      try {
        const read = await Promise.all(
          keys.map(async (key) => {
            const r = await fetch(`/api/history?point=${encodeURIComponent(`${id}/${key}`)}&hours=${hours}&points=2000`);
            if (!r.ok) throw new Error(r.status === 404 ? t('salon.cafe.conso.sans_historique') : `HTTP ${r.status}`);
            return [key.slice(6), await r.json()];
          }),
        );
        if (!gone) (series = Object.fromEntries(read)), (error = null);
      } catch (e) {
        if (!gone) error = e.message;
      }
    }, 1500);
    return () => ((gone = true), clearTimeout(late));
  });

  const counted = $derived(series ? daily(series, span) : null);
  const days = $derived(counted?.days ?? []);
  const today = $derived(days.at(-1));
  const mean = $derived(counted ? average(days, counted.since) : null);
  let hover = $state(null);
  const shown = $derived(hover != null ? days[hover] : null);
  const drinks = $derived(byDrink(shown ? [shown] : days));
  const dayWord = (start) => new Date(start).toLocaleDateString(locale(), { weekday: 'long', day: 'numeric', month: 'long' });
  const sinceWord = $derived(counted?.since ? new Date(counted.since).toLocaleDateString(locale(), { day: 'numeric', month: 'long' }) : null);
  // Counting began within the days shown: the earlier ones are not zero, just unknown.
  const unread = (d) => counted?.since == null || d.start + 86_400_000 <= counted.since;
  // The average counts from then when counting began within the days shown.
  const partial = $derived(!!days[0] && unread(days[0]));

  // The bars: one a day, a little room between them.
  let W = $state(320);
  const H = 120;
  const top = $derived(Math.max(4, ...days.map((d) => d.total)));
  const slot = $derived(W / Math.max(1, days.length));
  const barW = $derived(Math.max(3, Math.min(28, slot - 2)));
  const y = (n) => H - 18 - (n / top) * (H - 30);
  // A bar's path: rounded at its top, square on the baseline.
  const bar = (x, n) => {
    const h = y(0) - y(n);
    const r = Math.min(4, barW / 2, h);
    return `M${x} ${y(0)}v${-(h - r)}a${r} ${r} 0 0 1 ${r} ${-r}h${barW - 2 * r}a${r} ${r} 0 0 1 ${r} ${r}v${h - r}z`;
  };
  const tick = (start, i) => {
    const date = new Date(start);
    if (span === 7) return date.toLocaleDateString(locale(), { weekday: 'narrow' });
    // Every fifth day, counted back from today.
    return (days.length - 1 - i) % 5 === 0 ? String(date.getDate()) : '';
  };
</script>

<section class="card usage">
  <div class="card-head">
    <h2><Icon name="coffee" size={18} />{t('salon.cafe.conso.titre')}</h2>
    <div class="spans" role="radiogroup" aria-label={t('salon.cafe.conso.periode')}>
      {#each [7, 30] as n (n)}
        <button role="radio" aria-checked={span === n} class:active={span === n} onclick={() => ((span = n), (hover = null))}>{t('salon.cafe.conso.jours', { count: n })}</button>
      {/each}
    </div>
  </div>

  {#if error}
    <p class="muted">{t('salon.cafe.conso.indisponible', { error })}</p>
  {:else if !counted}
    <p class="muted">{t('salon.cafe.conso.chargement')}</p>
  {:else}
    <div class="readout" aria-live="polite">
      <div>
        <small>{shown ? dayWord(shown.start) : t('salon.cafe.conso.aujourdhui')}</small>
        <strong class="num">{t('salon.cafe.conso.tasses', { count: (shown ?? today)?.total ?? 0 })}</strong>
      </div>
      {#if mean != null}
        <div>
          <small>{partial ? t('salon.cafe.conso.moyenne_depuis', { date: sinceWord }) : t('salon.cafe.conso.moyenne', { count: span })}</small>
          <strong class="num">{t('salon.cafe.conso.par_jour', { n: num(mean, mean < 10 ? 1 : 0) })}</strong>
        </div>
      {/if}
    </div>

    <div class="plot" bind:clientWidth={W}>
      <svg viewBox="0 0 {W} {H}" height={H} role="img" aria-label={t('salon.cafe.conso.graphe', { count: span })} onpointerleave={() => (hover = null)}>
        <line x1="0" x2={W} y1={y(0) + 0.5} y2={y(0) + 0.5} class="base" />
        {#each days as d, i (d.start)}
          {@const x = i * slot + (slot - barW) / 2}
          <g class:dim={hover != null && hover !== i}>
            <rect x={i * slot} y="0" width={slot} height={H} class="hit" role="presentation" onpointerenter={() => (hover = i)} onpointerdown={() => (hover = i)}>
              <title>{dayWord(d.start)} : {t('salon.cafe.conso.tasses', { count: d.total })}</title>
            </rect>
            {#if d.total > 0}
              <path d={bar(x, d.total)} class="cups" />
              {#if span === 7 || hover === i}<text x={x + barW / 2} y={y(d.total) - 4} class="value" text-anchor="middle">{d.total}</text>{/if}
            {:else if unread(d)}
              <line x1={x} x2={x + barW} y1={y(0) - 1.5} y2={y(0) - 1.5} class="unknown" />
            {/if}
          </g>
          <text x={x + barW / 2} y={H - 3} class="axis" text-anchor="middle">{tick(d.start, i)}</text>
        {/each}
      </svg>
    </div>

    {#if drinks.length}
      <ul class="drinks">
        {#each drinks as { drink, n } (drink)}
          <li><DrinkIcon {drink} size={28} /><span>{drinkName(drink)}</span><b class="num">{n}</b></li>
        {/each}
      </ul>
    {:else}
      <p class="muted">{t(shown ? 'salon.cafe.conso.aucune_ce_jour' : 'salon.cafe.conso.aucune')}</p>
    {/if}
    {#if sinceWord}<p class="muted small">{t('salon.cafe.conso.depuis', { date: sinceWord })}</p>{/if}
  {/if}
</section>

<style>
  .usage .card-head {
    flex-wrap: wrap;
  }

  .spans {
    display: flex;
    gap: 4px;
    padding: 3px;
    border-radius: 999px;
    background: var(--surface-2);
  }

  .spans button {
    white-space: nowrap;
    border: 0;
    border-radius: 999px;
    padding: 5px 12px;
    background: none;
    color: var(--ink-2);
    font: inherit;
    font-size: 13px;
    font-weight: 600;
    cursor: pointer;
  }

  .spans button.active {
    background: var(--ink);
    color: var(--surface);
  }

  .readout {
    display: flex;
    flex-wrap: wrap;
    gap: 6px 28px;
    margin-bottom: 10px;
  }

  .readout div {
    display: grid;
  }

  .readout small {
    font-size: 12.5px;
    color: var(--ink-3);
  }

  .readout strong {
    font-size: 22px;
    font-weight: 750;
  }

  svg {
    display: block;
    width: 100%;
    touch-action: pan-y;
  }

  .base {
    stroke: var(--line);
    stroke-width: 1;
  }

  .hit {
    fill: transparent;
  }

  .cups {
    fill: var(--warm);
  }

  .unknown {
    stroke: var(--ink-3);
    stroke-width: 1.5;
    stroke-dasharray: 2 2;
    opacity: 0.5;
  }

  .dim {
    opacity: 0.4;
  }

  .axis,
  .value {
    font-size: 11px;
    fill: var(--ink-3);
  }

  .value {
    fill: var(--ink-2);
    font-weight: 650;
  }

  .drinks {
    list-style: none;
    margin: 12px 0 0;
    padding: 0;
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(min(100%, 170px), 1fr));
    gap: 6px 16px;
  }

  .drinks li {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 14px;
  }

  .drinks span {
    flex: 1;
  }

  .muted {
    font-size: 13px;
    color: var(--ink-3);
  }

  .small {
    margin-top: 10px;
    font-size: 12px;
  }
</style>
