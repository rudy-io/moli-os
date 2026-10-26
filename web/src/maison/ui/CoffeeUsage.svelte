<script>
  import { device, value, num, home } from '../lib/home.svelte.js';
  import { drinkName } from '../lib/coffee.js';
  import { cupCounters, cupHistory, daily, hourly, byDrink, average, drinkColors } from '../lib/coffee-usage.js';
  import { NEUTRAL } from '../lib/palette.js';
  import { t, locale } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';
  import DrinkIcon from './DrinkIcon.svelte';
  import StackedBars from './StackedBars.svelte';

  /** The coffee drunk, in the electricity chart's bars: today hour by hour,
   *  or a bar a day over 7 or 30 days, each stacked by drink in the colours
   *  the drinks keep everywhere (`drinkColors`); the cups of today (or of
   *  the bar touched), the average a day, and the cups by drink. From the
   *  machine's counters in the history (`lib/coffee-usage.js`); read again
   *  when a drink is counted. `compact` (the home page's coffee card): today
   *  only, a smaller chart, the drinks in a line. */
  let { id, compact = false } = $props();

  let span = $state('today');
  const today = $derived(compact || span === 'today');
  let series = $state(null);
  let error = $state(null);
  const counters = $derived(cupCounters(device(id)?.points));
  // Read at once; a drink counted, read again once the history has it (it
  // writes every 2 s).
  const total = $derived(value(id, 'brews_total'));
  let readOnce = false;
  $effect(() => {
    const keys = counters;
    // Thirty-one days: every span from one reading (today's needs midnight's count).
    const hours = compact ? 26 : 31 * 24 + 24;
    void total;
    if (!keys.length) return;
    const wait = readOnce ? 3000 : 0;
    readOnce = true;
    let gone = false;
    const late = setTimeout(async () => {
      try {
        const read = await cupHistory(id, keys, hours);
        if (!gone) (series = read), (error = null);
      } catch (e) {
        if (!gone) error = e.status === 404 ? t('salon.cafe.conso.sans_historique') : e.message;
      }
    }, wait);
    return () => ((gone = true), clearTimeout(late));
  });

  // The buckets of the span, read again each minute (midnight starts a day).
  const now = $derived(home.now);
  const week = $derived(series ? daily(series, 7, now) : null);
  const counted = $derived(!series ? null : today ? { buckets: hourly(series, now).hours, since: week.since } : { buckets: daily(series, span, now).days, since: week.since });
  const buckets = $derived(counted?.buckets ?? []);
  const cupsToday = $derived(week?.days.at(-1)?.total ?? 0);
  const days = $derived(today ? week?.days ?? [] : buckets);
  const mean = $derived(counted ? average(days, counted.since) : null);
  const sinceWord = $derived(counted?.since ? new Date(counted.since).toLocaleDateString(locale(), { day: 'numeric', month: 'long' }) : null);
  const DAY = 86_400_000;
  // Counting began within the days shown: the earlier ones are not zero, just unknown.
  const unread = (d) => counted?.since == null || d.start + DAY <= counted.since;
  const partial = $derived(!!days[0] && unread(days[0]));

  let hover = $state(null);
  $effect(() => {
    void span;
    hover = null;
  });
  const shown = $derived(hover != null ? buckets[hover] : null);
  const drinks = $derived(byDrink(shown ? [shown] : buckets));

  // Each drink its colour, whatever the period; each stack the period's most
  // drunk at the bottom.
  const colors = $derived(
    drinkColors(Object.fromEntries((device(id)?.points ?? []).filter((p) => p.key.startsWith('brews.')).map((p) => [p.key.slice(6), Number(value(id, p.key) ?? 0)]))),
  );
  const order = $derived(byDrink(buckets).map(({ drink }) => ({ id: drink, color: colors.get(drink) ?? NEUTRAL })));

  const hourWord = (start) => new Date(start).toLocaleTimeString(locale(), { hour: '2-digit' });
  const dayWord = (start) => new Date(start).toLocaleDateString(locale(), { weekday: 'long', day: 'numeric', month: 'long' });
  const when = (b) => (today ? t('salon.cafe.conso.heure', { from: hourWord(b.start), to: hourWord(b.start + 3_600_000) }) : dayWord(b.start));
  function tick(b, i) {
    const date = new Date(b.start);
    if (today) return date.getHours() % 3 === 0 ? hourWord(b.start) : '';
    if (span === 7) return date.toLocaleDateString(locale(), { weekday: 'narrow' });
    // Every fifth day, counted back from today.
    return (buckets.length - 1 - i) % 5 === 0 ? String(date.getDate()) : '';
  }
  const above = (b, i) => (today || span === 7 || hover === i ? String(b.total) : '');
  const label = $derived(today ? t('salon.cafe.conso.graphe_jour') : t('salon.cafe.conso.graphe', { count: span }));
</script>

{#snippet chart(height)}
  <StackedBars
    bars={buckets}
    {order}
    amount={(b, drink) => b.by[drink] ?? 0}
    {tick}
    {above}
    unknown={today ? null : unread}
    title={(b) => `${when(b)} : ${t('salon.cafe.conso.tasses', { count: b.total })}`}
    floor={today ? 2 : 4}
    {height}
    {label}
    bind:hover />
{/snippet}

{#if compact}
  {#if series && !error}
    <div class="today" aria-live="polite">
      <p class="line">
        <span class="muted">{shown ? when(shown) : t('salon.cafe.conso.aujourdhui')}</span>
        <b class="num">{t('salon.cafe.conso.tasses', { count: shown ? shown.total : cupsToday })}</b>
      </p>
      {@render chart(96)}
      {#if drinks.length}
        <ul class="chips">
          {#each drinks as { drink, n } (drink)}
            <li><i class="swatch" style:background={colors.get(drink) ?? NEUTRAL}></i>{drinkName(drink)} <b class="num">{n}</b></li>
          {/each}
        </ul>
      {/if}
    </div>
  {/if}
{:else}
  <section class="card usage">
    <div class="card-head">
      <h2><Icon name="coffee" size={18} />{t('salon.cafe.conso.titre')}</h2>
      <div class="spans" role="radiogroup" aria-label={t('salon.cafe.conso.periode')}>
        {#each ['today', 7, 30] as n (n)}
          <button role="radio" aria-checked={span === n} class:active={span === n} onclick={() => (span = n)}>
            {n === 'today' ? t('salon.cafe.conso.aujourdhui') : t('salon.cafe.conso.jours', { count: n })}
          </button>
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
          <small>{shown ? when(shown) : today ? t('salon.cafe.conso.aujourdhui') : t('salon.cafe.conso.sur_periode', { count: span })}</small>
          <strong class="num">{t('salon.cafe.conso.tasses', { count: shown ? shown.total : today ? cupsToday : buckets.reduce((sum, b) => sum + b.total, 0) })}</strong>
        </div>
        {#if mean != null}
          <div>
            <small>{partial ? t('salon.cafe.conso.moyenne_depuis', { date: sinceWord }) : t('salon.cafe.conso.moyenne', { count: days.length })}</small>
            <strong class="num">{t('salon.cafe.conso.par_jour', { n: num(mean, mean < 10 ? 1 : 0) })}</strong>
          </div>
        {/if}
      </div>

      {@render chart(140)}

      {#if drinks.length}
        <ul class="drinks">
          {#each drinks as { drink, n } (drink)}
            <li><i class="swatch" style:background={colors.get(drink) ?? NEUTRAL}></i><DrinkIcon {drink} size={28} /><span>{drinkName(drink)}</span><b class="num">{n}</b></li>
          {/each}
        </ul>
      {:else}
        <p class="muted">{t(shown ? (today ? 'salon.cafe.conso.aucune_cette_heure' : 'salon.cafe.conso.aucune_ce_jour') : today ? 'salon.cafe.conso.aucune_aujourdhui' : 'salon.cafe.conso.aucune')}</p>
      {/if}
      {#if sinceWord}<p class="muted small">{t('salon.cafe.conso.depuis', { date: sinceWord })}</p>{/if}
    {/if}
  </section>
{/if}

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

  .swatch {
    flex: none;
    width: 12px;
    height: 12px;
    border-radius: 3px;
  }

  .drinks {
    list-style: none;
    margin: 12px 0 0;
    padding: 0;
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(min(100%, 190px), 1fr));
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

  .today {
    display: grid;
    gap: 6px;
  }

  .line {
    display: flex;
    justify-content: space-between;
    gap: 10px;
    margin: 0;
    font-size: 13.5px;
  }

  .chips {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-wrap: wrap;
    gap: 4px 14px;
    font-size: 12.5px;
    color: var(--ink-2);
  }

  .chips li {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }

  .chips .swatch {
    width: 10px;
    height: 10px;
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
