<script>
  import { onMount } from 'svelte';
  import { hub, home, value } from '../lib/home.svelte.js';
  import { kwh, euros } from '../lib/energy-live.svelte.js';
  import { t, locale } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';
  import { dayHours, totals, peak } from '../lib/day-energy.js';
  import { forecastDevice, parseCurve, forecastHours, producedHours, soFar } from '../lib/solar-forecast.js';
  import { yearOverYear, trendTone } from '../lib/year-compare.js';

  /** The day hour by hour, in one card: each bar what the house used, the
   *  part the sun covered under what was bought; a usual day (7 days) as a
   *  mark; the hours to come at their usual height; with panels, what they
   *  made, and the forecast dashed with its low–high band (the `solcast`
   *  driver). Off-peak hours shaded, the floor dashed.
   *  `week`: /api/energy/series?step=hour&count=168; `windows`: off-peak
   *  hours (minutes); `base`: the floor (W); `cost`: today's cost. */
  let { week, windows = [], base = null, cost = null, currency = '€', foot = '' } = $props();
  const H = 3_600_000;

  let days = $state(null);
  async function loadDays() {
    try {
      const r = await fetch('/api/energy/series?step=day&count=400');
      days = r.ok ? await r.json() : null;
    } catch {
      days = null;
    }
  }
  onMount(() => {
    loadDays();
    const every = setInterval(loadDays, 60 * 60_000);
    return () => clearInterval(every);
  });

  const day = $derived(dayHours(week, home.now));
  const sum = $derived(totals(day.today, day.usual, home.now));
  const top = $derived(peak(day.today));
  const hasSolar = $derived((week?.meters ?? []).some((m) => m.role === 'solar'));

  // The forecast, by hour, and what the panels made (same hours).
  const device = $derived(forecastDevice(hub.devices, value));
  const fv = (k) => (device ? value(device.id, k) : null);
  const slots = $derived(device ? parseCurve(fv('curve')) : null);
  const expected = $derived(slots ? forecastHours(slots, day.start, day.start + 24 * H) : []);
  const byHour = $derived(new Map(expected.map((e) => [new Date(e.start).getHours(), e])));
  const made = $derived(producedHours(week));
  const verdict = $derived(slots ? soFar(expected, made, home.now) : null);
  const yearSolar = $derived(hasSolar ? yearOverYear(days, home.now, 'solar') : null);
  const updated = $derived.by(() => {
    const u = fv('updated');
    const d = u ? new Date(u) : null;
    return d && !Number.isNaN(d.getTime()) ? d.toLocaleTimeString(locale(), { hour: '2-digit', minute: '2-digit' }) : null;
  });

  // ---- the chart ------------------------------------------------------------
  const W = 1120;
  const HGT = 240;
  const L = 36;
  const R = 8;
  const T = 14;
  const B = 26;
  const col = (W - L - R) / 24;
  const bw = col * 0.72;
  const hourNow = $derived(new Date(home.now).getHours());
  const minuteNow = $derived(new Date(home.now).getMinutes());
  const scale = $derived(
    Math.max(
      0.5,
      ...(day.usual ?? []),
      ...(day.today ?? []).map((h) => h?.used ?? 0),
      ...expected.map((e) => e.hi),
      ...[...made.entries()].filter(([s]) => s >= day.start).map(([, k]) => k),
    ) * 1.08,
  );
  const x = (h) => L + h * col;
  const y = (v) => T + (HGT - T - B) * (1 - Math.min(v, scale) / scale);
  const mid = (h) => x(h) + col / 2;
  const bars = $derived(
    Array.from({ length: 24 }, (_, h) => {
      const now = day.today?.[h] ?? null;
      return { h, now, usual: day.usual?.[h] ?? null, future: !now && h >= hourNow };
    }),
  );
  const ticks = $derived(
    [1, 2, 3, 4, 5].filter((v) => v < scale * 0.95).filter((v, i, a) => a.length <= 3 || v % 2 === 0),
  );
  const line = (pts) => pts.map(([a, b], i) => `${i ? 'L' : 'M'}${a.toFixed(1)} ${b.toFixed(1)}`).join(' ');
  // The hours of light: from the hour before the first that produces
  // (forecast or real) to the hour after the last.
  const litHours = $derived([
    ...expected.filter((e) => e.hi > 0.005).map((e) => new Date(e.start).getHours()),
    ...[...made.entries()].filter(([s, k]) => s >= day.start && s < day.start + 24 * H && k > 0.005).map(([s]) => new Date(s).getHours()),
  ]);
  const span = $derived(litHours.length ? [Math.max(0, Math.min(...litHours) - 1), Math.min(23, Math.max(...litHours) + 1)] : null);
  const inLight = (h) => span != null && h >= span[0] && h <= span[1];
  // Only the hours the forecast speaks of (a first answer in the
  // afternoon says nothing of the morning: no line there, not a zero).
  const fcHours = $derived([...byHour.keys()].filter(inLight).sort((a, b) => a - b));
  // Drawn column by column, like the bars: the low–high range behind each
  // hour, the expected value a dashed mark across it.
  const fcCols = $derived(fcHours.map((h) => ({ h, ...byHour.get(h) })).filter((f) => f.hi > 0.005));
  // Made: each past hour at its middle; the current one pro rata, at the
  // middle of what has gone by.
  const madePts = $derived(
    [...made.entries()]
      .filter(([s]) => s >= day.start && s < day.start + 24 * H && s <= home.now && inLight(new Date(s).getHours()))
      .sort((a, b) => a[0] - b[0])
      .map(([s, k]) => {
        const h = new Date(s).getHours();
        const part = Math.max(0.25, Math.min(1, (home.now - s) / H));
        return [x(h) + (col * part) / 2, y(k / part)];
      })
      .filter((_, i, a) => a.length > 1),
  );
  const offAt = (h) => {
    const m = h * 60 + 30;
    return windows.some((w) => (w.from < w.to ? m >= w.from && m < w.to : m >= w.from || m < w.to));
  };

  // ---- the hour read out (hovered, else the busiest) ------------------------
  let hover = $state(null);
  const shown = $derived(hover ?? top ?? (day.usual ? day.usual.indexOf(Math.max(...day.usual)) : null));
  const shownHour = $derived(shown == null ? null : { h: shown, now: day.today?.[shown] ?? null, usual: day.usual?.[shown] ?? null, fc: byHour.get(shown) ?? null });
  const sign = (n) => (n > 0 ? `+${n}` : n < 0 ? `−${-n}` : '0');
</script>

{#if day.today || day.usual}
  <section class="card day" aria-label={t('energie.jour.titre')}>
    <div class="card-head">
      <h2><Icon name="clock" size={18} />{t('energie.jour.titre')}</h2>
      {#if updated}<span class="muted small">{t('energie.prevision.mise_a_jour', { heure: updated })}</span>{/if}
    </div>

    {#if sum}
      <div class="numbers">
        <div>
          <small>{t('energie.jour.consomme', { h: hourNow, m: String(minuteNow).padStart(2, '0') })}</small>
          <b class="num">{kwh(sum.used)} kWh</b>
          <span class="muted">
            {#if sum.usual != null}{t('energie.jour.habitude', { kwh: kwh(sum.usual) })}{/if}
            {#if sum.gap != null}<span class="chip {trendTone(sum.gap)}">{t('commun.pourcent', { n: sign(sum.gap) })}</span>{/if}
          </span>
        </div>
        {#if hasSolar}
          <div>
            <small class="sun">{t('energie.jour.soleil')}</small>
            <b class="num">{kwh(sum.sun)} kWh</b>
            <span class="muted">
              {#if sum.share != null}{t('energie.jour.part', { pct: sum.share })}{/if}
              {#if typeof fv('today_kwh') === 'number'} · {t('energie.jour.prevu', { kwh: kwh(fv('today_kwh')) })}{/if}
              {#if typeof fv('remaining_kwh') === 'number' && fv('remaining_kwh') > 0.05} {t('energie.jour.encore', { kwh: kwh(fv('remaining_kwh')) })}{/if}
            </span>
          </div>
        {/if}
        <div>
          <small class="grid">{t('energie.jour.achete')}</small>
          <b class="num">{kwh(sum.bought)} kWh</b>
          <span class="muted">
            {#if cost != null}{euros(cost, currency)}{/if}
            {#if top != null} · {t('energie.jour.pic', { from: top, to: top + 1, kwh: kwh(day.today[top].used) })}{/if}
          </span>
        </div>
        {#if typeof fv('tomorrow_kwh') === 'number'}
          <div>
            <small>{t('energie.jour.demain')}</small>
            <b class="num">{kwh(fv('tomorrow_kwh'))} kWh</b>
            {#if typeof fv('tomorrow_low_kwh') === 'number'}<span class="muted">{t('energie.prevision.fourchette', { bas: kwh(fv('tomorrow_low_kwh')), haut: kwh(fv('tomorrow_high_kwh')) })}</span>{/if}
          </div>
        {/if}
      </div>
    {/if}

    {#if verdict || yearSolar}
      <div class="chips">
        {#if verdict}
          <span class="chip {verdict.verdict === 'above' ? 'good' : verdict.verdict === 'below' ? 'warm' : ''}">
            <Icon name={verdict.verdict === 'below' ? 'trending-down' : verdict.verdict === 'above' ? 'trending-up' : 'ok'} size={15} />
            {verdict.verdict === 'above'
              ? t('energie.jour.verdict_dessus', { pct: sign(verdict.pct) })
              : verdict.verdict === 'below'
                ? t('energie.jour.verdict_dessous', { pct: verdict.pct })
                : Math.abs(verdict.pct) < 10
                  ? t('energie.jour.verdict_comme')
                  : t('energie.jour.verdict_fourchette', { pct: sign(verdict.pct) })}
          </span>
        {/if}
        {#if yearSolar}
          <span class="chip {trendTone(yearSolar.pct, 'better')}">
            {t('energie.prevision.mois_vs_an', {
              kwh: kwh(yearSolar.now),
              n: sign(yearSolar.pct),
              mois: yearSolar.month.toLocaleDateString(locale(), { month: 'short', year: 'numeric' }),
            })}
          </span>
        {/if}
      </div>
    {/if}

    <div class="readout">
      {#if shownHour}
        <span class="muted">{hover == null ? t('energie.jour.plus_chargee') : t('energie.jour.cette_heure')}</span>
        <strong class="num">{t('commun.jour.plage', { from: shownHour.h, to: shownHour.h + 1, kwh: kwh(shownHour.now?.used ?? shownHour.usual) })}</strong>
        {#if shownHour.now && hasSolar}<span class="muted">{t('energie.jour.detail', { soleil: kwh(shownHour.now.sun), achete: kwh(shownHour.now.bought) })}</span>{/if}
        {#if shownHour.now && shownHour.usual != null}<span class="muted">{t('energie.jour.habitude', { kwh: kwh(shownHour.usual) })}</span>{/if}
        {#if shownHour.fc && shownHour.fc.kwh > 0.005}<span class="muted">{t('energie.jour.prevu_heure', { kwh: kwh(shownHour.fc.kwh) })}</span>{/if}
        {#if offAt(shownHour.h)}<span class="chip cool">{t('commun.jour.creuse')}</span>{:else if windows.length}<span class="chip warm">{t('commun.jour.pleine')}</span>{/if}
      {/if}
    </div>

    <div class="plot">
      <svg viewBox="0 0 {W} {HGT}" role="img" aria-label={t('energie.jour.aria', { conso: kwh(sum?.used), soleil: kwh(sum?.sun), achete: kwh(sum?.bought) })}>
        {#each bars as b (b.h)}
          {#if offAt(b.h)}<rect class="off" x={x(b.h)} y={T} width={col} height={HGT - T - B} />{/if}
        {/each}
        {#each ticks as v (v)}
          <line class="grid-line" x1={L} x2={W - R} y1={y(v)} y2={y(v)} />
          <text class="axis" x={L - 6} y={y(v) + 4} text-anchor="end">{v}</text>
        {/each}
        {#if base}<line class="floor" x1={L} x2={W - R} y1={y(base / 1000)} y2={y(base / 1000)} />{/if}
        {#each fcCols as f (f.h)}
          <rect class="band" x={x(f.h) + (col - bw) / 2 - 4} y={y(f.hi)} width={bw + 8} height={Math.max(1, y(f.lo) - y(f.hi))} rx="3" />
        {/each}
        {#each bars as b (b.h)}
          {@const cx = x(b.h) + (col - bw) / 2}
          {#if b.now}
            {#if b.now.sun > 0.002}<rect class="sun" x={cx} y={y(b.now.sun)} width={bw} height={y(0) - y(b.now.sun)} rx="2" />{/if}
            {#if b.now.used - b.now.sun > 0.002}<rect class="bought" x={cx} y={y(b.now.used)} width={bw} height={y(b.now.sun) - y(b.now.used)} rx="3" />{/if}
            {#if b.usual != null}<line class="usual" x1={cx - 3} x2={cx + bw + 3} y1={y(b.usual)} y2={y(b.usual)} />{/if}
          {:else if b.future && b.usual != null}
            <rect class="ahead" x={cx} y={y(b.usual)} width={bw} height={y(0) - y(b.usual)} rx="3" />
          {/if}
          {#if b.h === top && b.now}
            <rect class="peak" x={cx - 3} y={y(b.now.used) - 3} width={bw + 6} height={y(0) - y(b.now.used) + 3} rx="5" />
          {/if}
        {/each}
        {#each fcCols as f (f.h)}
          <line class="expected" x1={x(f.h) + (col - bw) / 2 - 4} x2={x(f.h) + (col + bw) / 2 + 4} y1={y(f.kwh)} y2={y(f.kwh)} />
        {/each}
        {#if madePts.length}
          <path class="made" d={line(madePts)} />
          {#each madePts as [a, c], i (i)}<circle class="dot" cx={a} cy={c} r="3.5" />{/each}
        {/if}
        <line class="now" x1={x(hourNow + minuteNow / 60)} x2={x(hourNow + minuteNow / 60)} y1={T} y2={y(0)} />
        <line class="base" x1={L} x2={W - R} y1={y(0)} y2={y(0)} />
        {#each [0, 3, 6, 9, 12, 15, 18, 21] as h (h)}
          <text class="axis" x={x(h)} y={HGT - 8}>{t('commun.jour.heure', { h })}</text>
        {/each}
      </svg>
      <div class="hits" style:left="{(L / W) * 100}%" style:right="{(R / W) * 100}%" onpointerleave={() => (hover = null)}>
        {#each bars as b (b.h)}
          <button
            class:on={shown === b.h}
            onpointerenter={() => (hover = b.h)}
            onfocus={() => (hover = b.h)}
            onblur={() => (hover = null)}
            aria-label={t('commun.jour.heure_kwh', { h: b.h, kwh: kwh(b.now?.used ?? b.usual) })}
          ></button>
        {/each}
      </div>
    </div>

    <p class="legend">
      {#if hasSolar}<span><i class="sw sun"></i>{t('energie.jour.legende_soleil')}</span>{/if}
      <span><i class="sw bought"></i>{t('energie.jour.legende_achete')}</span>
      <span><i class="sw usual"></i>{t('energie.jour.legende_habitude')}</span>
      <span><i class="sw ahead"></i>{t('energie.jour.legende_a_venir')}</span>
      {#if madePts.length}<span><i class="sw made"></i>{t('energie.prevision.legende_produit')}</span>{/if}
      {#if fcCols.length}<span><i class="sw expected"></i>{t('energie.jour.legende_prevision')}</span><span><i class="sw band"></i>{t('energie.prevision.legende_fourchette')}</span>{/if}
      {#if windows.length}<span><i class="sw off"></i>{t('commun.jour.heures_creuses')}</span>{/if}
      {#if base}<span><i class="sw floor"></i>{t('commun.jour.talon_legende')}</span>{/if}
      <span class="muted">{t('energie.jour.unite')}</span>
    </p>
    {#if foot}<p class="foot muted">{foot}</p>{/if}
  </section>
{/if}

<style>
  .day {
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
    grid-template-columns: repeat(auto-fit, minmax(170px, 1fr));
    gap: 14px;
  }

  .numbers div {
    display: grid;
    gap: 3px;
    align-content: start;
  }

  .numbers small {
    color: var(--ink-2);
    font-size: 12px;
    font-weight: 700;
    text-transform: uppercase;
    letter-spacing: 0.06em;
  }

  .numbers small.sun {
    color: var(--warm-ink);
  }

  .numbers small.grid {
    color: var(--cool);
  }

  .numbers b {
    font-size: 24px;
    font-weight: 700;
  }

  .numbers .muted {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 4px 8px;
    font-size: 13px;
  }

  .numbers .chip {
    padding: 2px 10px;
  }

  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }

  .chips .chip {
    white-space: normal;
  }

  .readout {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 6px 10px;
    min-height: 24px;
    font-size: 13px;
  }

  .readout strong {
    font-size: 16px;
  }

  .plot {
    position: relative;
  }

  svg {
    width: 100%;
    height: auto;
    display: block;
    font-variant-numeric: tabular-nums;
  }

  .hits {
    position: absolute;
    top: 0;
    bottom: 26px;
    display: grid;
    grid-template-columns: repeat(24, minmax(0, 1fr));
  }

  .hits button {
    border: 0;
    background: transparent;
    padding: 0;
    cursor: pointer;
  }

  .hits button.on {
    background: rgb(29 36 51 / 4%);
    border-radius: 6px;
  }

  .off {
    fill: var(--cool-soft);
    opacity: 0.55;
  }

  .grid-line {
    stroke: var(--line);
    stroke-width: 1;
  }

  .axis {
    fill: var(--ink-3);
    font-size: 11px;
  }

  .floor {
    stroke: var(--ink-3);
    stroke-width: 1;
    stroke-dasharray: 4 4;
  }

  .sun {
    fill: var(--warm);
  }

  .bought {
    fill: var(--cool);
  }

  .usual {
    stroke: var(--ink);
    stroke-width: 2.5;
    stroke-linecap: round;
  }

  /* Consumption to come: an outline in the colour of what is bought,
     never a filled bar that could pass for the sun. */
  .ahead {
    fill: var(--cool);
    fill-opacity: 0.06;
    stroke: var(--cool);
    stroke-opacity: 0.6;
    stroke-width: 1.5;
    stroke-dasharray: 4 3;
  }

  .peak {
    fill: none;
    stroke: var(--ink);
    stroke-width: 1.5;
  }

  .band {
    fill: var(--warm);
    fill-opacity: 0.18;
  }

  .expected {
    stroke: var(--warm-ink);
    stroke-width: 2.5;
    stroke-dasharray: 5 3;
    stroke-linecap: butt;
  }

  .made {
    fill: none;
    stroke: var(--warm-ink);
    stroke-width: 3;
    stroke-linejoin: round;
  }

  .dot {
    fill: var(--warm-ink);
    stroke: var(--surface);
    stroke-width: 1.5;
  }

  .now {
    stroke: var(--ink);
    stroke-width: 1;
    stroke-dasharray: 2 4;
  }

  .base {
    stroke: var(--ink-3);
    stroke-width: 1;
    opacity: 0.6;
  }

  .foot {
    margin: 0;
    font-size: 13px;
  }

  .legend {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
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

  .sw {
    display: inline-block;
    width: 12px;
    height: 12px;
    border-radius: 3px;
  }

  .sw.sun {
    background: var(--warm);
  }

  .sw.bought {
    background: var(--cool);
  }

  .sw.usual,
  .sw.made,
  .sw.expected,
  .sw.floor {
    width: 16px;
    height: 0;
    border-radius: 0;
    border-top: 2.5px solid var(--ink);
  }

  .sw.made {
    border-top: 3px solid var(--warm-ink);
  }

  .sw.expected {
    border-top: 2px dashed var(--warm-ink);
  }

  .sw.floor {
    border-top: 1px dashed var(--ink-3);
  }

  .sw.ahead {
    border: 1.5px dashed var(--cool);
    background: var(--cool-soft);
  }

  .sw.band {
    width: 14px;
    height: 10px;
    background: var(--warm);
    opacity: 0.25;
  }

  .sw.off {
    background: var(--cool-soft);
  }
</style>
