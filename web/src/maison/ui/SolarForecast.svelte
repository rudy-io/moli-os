<script>
  import { onMount } from 'svelte';
  import { hub, home, value } from '../lib/home.svelte.js';
  import { kwh } from '../lib/energy-live.svelte.js';
  import { t, locale } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';
  import { forecastDevice, parseCurve, forecastHours, producedHours, dayOf, lightWindow, soFar } from '../lib/solar-forecast.js';
  import { yearOverYear, trendTone } from '../lib/year-compare.js';

  /** Today's solar production against the forecast (the `solcast` driver):
   *  the expected hours dashed with their low–high band, what the panels
   *  made over them, and whether the day runs above or below. Shown only
   *  where a forecast exists. */
  const H = 3_600_000;
  let series = $state(null);
  // The month's production against the same days a year ago.
  let days = $state(null);

  async function load() {
    try {
      const r = await fetch('/api/energy/series?step=hour&count=48');
      series = r.ok ? await r.json() : null;
    } catch {
      series = null;
    }
  }

  async function loadDays() {
    try {
      const r = await fetch('/api/energy/series?step=day&count=400');
      days = r.ok ? await r.json() : null;
    } catch {
      days = null;
    }
  }

  onMount(() => {
    load();
    loadDays();
    const every = setInterval(load, 5 * 60_000);
    const hourly = setInterval(loadDays, 60 * 60_000);
    return () => {
      clearInterval(every);
      clearInterval(hourly);
    };
  });

  const yearSolar = $derived(yearOverYear(days, home.now, 'solar'));

  const device = $derived(forecastDevice(hub.devices, value));
  const v = (k) => (device ? value(device.id, k) : null);
  const slots = $derived(device ? parseCurve(v('curve')) : null);
  const day = $derived(dayOf(home.now));
  const hours = $derived(slots ? forecastHours(slots, day[0], day[1]) : []);
  const made = $derived(producedHours(series));
  const span = $derived(lightWindow(hours, made, day));
  const verdict = $derived(soFar(hours, made, home.now));
  const madeToday = $derived([...made.entries()].filter(([s]) => s >= day[0] && s < day[1]).reduce((a, [, k]) => a + k, 0));

  // ---- the chart ----------------------------------------------------------
  const W = 640;
  const HGT = 200;
  const PAD = { l: 34, r: 12, t: 12, b: 26 };
  const top = $derived(
    Math.max(0.5, ...hours.map((h) => h.hi), ...[...made.entries()].filter(([s]) => span && s >= span[0] && s < span[1]).map(([, k]) => k)) * 1.1,
  );
  const x = (ms) => (span ? PAD.l + ((ms - span[0]) / (span[1] - span[0])) * (W - PAD.l - PAD.r) : 0);
  const y = (kw) => HGT - PAD.b - (kw / top) * (HGT - PAD.t - PAD.b);
  const inSpan = $derived(span ? hours.filter((h) => h.start >= span[0] && h.start < span[1]) : []);
  // Each hour drawn at its middle: kWh in an hour = mean kW over it.
  const mid = (h) => h.start + H / 2;
  const line = (pts) => pts.map(([a, b], i) => `${i ? 'L' : 'M'}${a.toFixed(1)} ${b.toFixed(1)}`).join(' ');
  const expectedPath = $derived(line(inSpan.map((h) => [x(mid(h)), y(h.kwh)])));
  const bandPath = $derived(
    inSpan.length ? `${line(inSpan.map((h) => [x(mid(h)), y(h.hi)]))} ${[...inSpan].reverse().map((h) => `L${x(mid(h)).toFixed(1)} ${y(h.lo).toFixed(1)}`).join(' ')} Z` : '',
  );
  const madePoints = $derived(
    span
      ? [...made.entries()]
          .filter(([s]) => s >= span[0] && s < span[1] && s < home.now)
          .sort((a, b) => a[0] - b[0])
          .map(([s, k]) => [x(s + Math.min(H, home.now - s) / 2), y(k / Math.max(0.25, Math.min(1, (home.now - s) / H)))])
      : [],
  );
  const madePath = $derived(line(madePoints));
  const ticks = $derived.by(() => {
    if (!span) return [];
    const out = [];
    for (let s = Math.ceil(span[0] / (2 * H)) * 2 * H; s <= span[1]; s += 2 * H) out.push(s);
    return out;
  });
  const hh = (ms) => new Date(ms).toLocaleTimeString(locale(), { hour: '2-digit' });
  const updated = $derived.by(() => {
    const u = v('updated');
    const d = u ? new Date(u) : null;
    return d && !Number.isNaN(d.getTime()) ? d.toLocaleTimeString(locale(), { hour: '2-digit', minute: '2-digit' }) : null;
  });
  const fmt = (x) => (typeof x === 'number' ? kwh(x) : '—');
</script>

{#if device && slots && span}
  <section class="card forecast" aria-label={t('energie.prevision.titre')}>
    <div class="card-head">
      <h2><Icon name="sun" size={18} />{t('energie.prevision.titre')}</h2>
      {#if updated}<span class="muted small">{t('energie.prevision.mise_a_jour', { heure: updated })}</span>{/if}
    </div>

    <div class="numbers">
      <div>
        <small>{t('energie.prevision.prevu')}</small>
        <b>{fmt(v('today_kwh'))} kWh</b>
        {#if typeof v('today_low_kwh') === 'number'}<span class="muted">{t('energie.prevision.fourchette', { bas: fmt(v('today_low_kwh')), haut: fmt(v('today_high_kwh')) })}</span>{/if}
      </div>
      <div>
        <small>{t('energie.prevision.produit')}</small>
        <b>{fmt(madeToday)} kWh</b>
        {#if typeof v('remaining_kwh') === 'number' && v('remaining_kwh') > 0.05}<span class="muted">{t('energie.prevision.encore', { kwh: fmt(v('remaining_kwh')) })}</span>{/if}
      </div>
      <div>
        <small>{t('energie.prevision.demain')}</small>
        <b>{fmt(v('tomorrow_kwh'))} kWh</b>
        {#if typeof v('tomorrow_low_kwh') === 'number'}<span class="muted">{t('energie.prevision.fourchette', { bas: fmt(v('tomorrow_low_kwh')), haut: fmt(v('tomorrow_high_kwh')) })}</span>{/if}
      </div>
    </div>

    {#if yearSolar}
      <p class="year chip {trendTone(yearSolar.pct, 'better')}">
        {t('energie.prevision.mois_vs_an', {
          kwh: kwh(yearSolar.now),
          n: `${yearSolar.pct > 0 ? '+' : ''}${yearSolar.pct}`,
          mois: yearSolar.month.toLocaleDateString(locale(), { month: 'short', year: 'numeric' }),
        })}
      </p>
    {/if}

    {#if verdict}
      <p class="verdict {verdict.verdict}">
        <Icon name={verdict.verdict === 'below' ? 'trending-down' : verdict.verdict === 'above' ? 'trending-up' : 'ok'} size={16} />
        {verdict.verdict === 'within'
          ? t('energie.prevision.dans', { pct: verdict.pct > 0 ? `+${verdict.pct}` : `${verdict.pct}` })
          : verdict.verdict === 'above'
            ? t('energie.prevision.au_dessus', { pct: verdict.pct })
            : t('energie.prevision.en_dessous', { pct: -verdict.pct })}
      </p>
    {/if}

    <svg viewBox="0 0 {W} {HGT}" role="img" aria-label={t('energie.prevision.aria', { prevu: fmt(v('today_kwh')), produit: fmt(madeToday) })}>
      {#each [0.5, 1] as f (f)}
        <line class="grid" x1={PAD.l} x2={W - PAD.r} y1={y(top * f / 1.1)} y2={y(top * f / 1.1)} />
        <text class="axis" x={PAD.l - 6} y={y(top * f / 1.1) + 4} text-anchor="end">{(top * f / 1.1).toFixed(1)}</text>
      {/each}
      <line class="base" x1={PAD.l} x2={W - PAD.r} y1={y(0)} y2={y(0)} />
      {#each ticks as s (s)}
        <text class="axis" x={x(s)} y={HGT - 8} text-anchor="middle">{hh(s)}</text>
      {/each}
      {#if bandPath}<path class="band" d={bandPath} />{/if}
      <path class="expected" d={expectedPath} />
      {#if madePath}<path class="made" d={madePath} />{/if}
      {#each madePoints as [a, b], i (i)}<circle class="dot" cx={a} cy={b} r="3" />{/each}
      {#if home.now > span[0] && home.now < span[1]}
        <line class="now" x1={x(home.now)} x2={x(home.now)} y1={PAD.t} y2={y(0)} />
      {/if}
    </svg>
    <p class="legend">
      <span><i class="k made"></i>{t('energie.prevision.legende_produit')}</span>
      <span><i class="k expected"></i>{t('energie.prevision.legende_prevu')}</span>
      <span><i class="k band"></i>{t('energie.prevision.legende_fourchette')}</span>
      <span class="muted">kW</span>
    </p>
  </section>
{/if}

<style>
  .forecast {
    display: grid;
    gap: 14px;
  }

  .card-head {
    flex-wrap: wrap;
    gap: 6px 12px;
  }

  .small {
    font-size: 13px;
  }

  .numbers {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(150px, 1fr));
    gap: 12px;
  }

  .numbers div {
    display: grid;
    gap: 2px;
  }

  .numbers small {
    color: var(--ink-2);
    font-size: 12px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }

  .numbers b {
    font-size: 22px;
    font-weight: 700;
    font-variant-numeric: tabular-nums;
  }

  .numbers .muted {
    font-size: 13px;
  }

  .year {
    justify-self: start;
    margin: 0;
    white-space: normal;
  }

  .verdict {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    justify-self: start;
    margin: 0;
    padding: 6px 12px;
    border-radius: 999px;
    font-size: 13.5px;
    font-weight: 650;
    background: var(--surface-2);
  }

  .verdict.above {
    background: var(--good-soft);
    color: var(--good);
  }

  .verdict.below {
    background: var(--warm-soft);
    color: var(--warm-ink);
  }

  svg {
    width: 100%;
    height: auto;
    display: block;
    font-variant-numeric: tabular-nums;
  }

  .grid {
    stroke: var(--line);
    stroke-width: 1;
  }

  .base {
    stroke: var(--ink-3);
    stroke-width: 1;
  }

  .axis {
    fill: var(--ink-3);
    font-size: 11px;
  }

  .band {
    fill: var(--warm-soft);
    opacity: 0.9;
  }

  .expected {
    fill: none;
    stroke: var(--warm);
    stroke-width: 2;
    stroke-dasharray: 5 5;
  }

  .made {
    fill: none;
    stroke: var(--good);
    stroke-width: 2.5;
    stroke-linejoin: round;
  }

  .dot {
    fill: var(--good);
    stroke: var(--surface);
    stroke-width: 1.5;
  }

  .now {
    stroke: var(--ink-3);
    stroke-width: 1;
    stroke-dasharray: 2 4;
  }

  .legend {
    display: flex;
    flex-wrap: wrap;
    gap: 6px 16px;
    margin: 0;
    font-size: 12.5px;
    color: var(--ink-2);
  }

  .legend span {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }

  .k {
    display: inline-block;
    width: 18px;
    height: 0;
    border-top: 2.5px solid var(--good);
  }

  .k.expected {
    border-top: 2px dashed var(--warm);
  }

  .k.band {
    height: 10px;
    border: 0;
    border-radius: 3px;
    background: var(--warm-soft);
  }
</style>
