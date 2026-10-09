<script>
  import { kwh, euros } from '../lib/energy-live.svelte.js';
  import { deviceSeries } from '../lib/energy-series.js';
  import { t, locale } from '../../lib/i18n.svelte.js';

  /** Bars over 24 h / 30 days / 12 months, stacked by what drew it
   *  (`by` = 'devices': circuits, appliances, lights, the rest) or as
   *  billed ('tariff': off-peak and peak). Hovering a bar, the legend says
   *  what each part drew in it. */
  let { range = 'day', by = 'devices' } = $props();

  const RANGES = {
    hour: { step: 'hour', count: 24 },
    day: { step: 'day', count: 30 },
    month: { step: 'month', count: 12 },
  };
  const PALETTE = ['#e9a23b', '#5b8def', '#3fae7c', '#c46ad8', '#e0705a', '#2fb3b8'];

  let report = $state(null);
  let error = $state('');
  let hover = $state(null);

  $effect(() => {
    const { step, count } = RANGES[range];
    error = '';
    hover = null;
    fetch(`/api/energy/series?step=${step}&count=${count}`)
      .then(async (r) => {
        if (r.ok) return r.json();
        throw new Error((await r.json().catch(() => ({}))).error ?? `HTTP ${r.status}`);
      })
      .then((r) => (report = r))
      .catch((e) => (error = e.message));
  });

  // Hour by hour, the Linky (whole kWh) is too coarse: circuits then.
  const meters = $derived(report?.meters ?? []);
  const circuits = $derived(meters.filter((m) => m.role === 'circuit'));
  const whole = $derived(meters.find((m) => m.role === 'total'));
  const grid = $derived(meters.filter((m) => m.role === 'grid'));
  const byCircuit = $derived(range === 'hour' || !grid.length);
  const parts = $derived(deviceSeries(meters));

  const series = $derived.by(() => {
    if (by === 'devices' && parts.length) return parts;
    if (byCircuit && circuits.length) {
      const parts = circuits.map((m, i) => ({ id: m.id, name: m.name, color: PALETTE[i % PALETTE.length] }));
      return whole ? [...parts, { id: '__rest', name: t('energie.reste'), color: 'var(--surface-3)', rest: true }] : parts;
    }
    const billed = grid.length ? grid : meters.filter((m) => m.role === 'total');
    return billed.map((m) => ({
      id: m.id,
      name: /hc|creuse/i.test(m.id + m.name) ? t('energie.hc') : /hp|pleine/i.test(m.id + m.name) ? t('energie.hp') : m.name,
      color: /hc|creuse/i.test(m.id + m.name) ? 'var(--cool)' : 'var(--warm)',
    }));
  });

  const amount = (bucket, s) => {
    if (s.kwh) return { kwh: s.kwh(bucket), cost: s.cost(bucket) ?? undefined };
    if (!s.rest) return bucket.meters[s.id] ?? { kwh: 0 };
    const all = bucket.meters[whole.id];
    const parts = circuits.map((m) => bucket.meters[m.id]).filter(Boolean);
    const sum = (k) => parts.reduce((t, a) => t + (a[k] ?? 0), 0);
    return { kwh: Math.max(0, (all?.kwh ?? 0) - sum('kwh')), cost: all?.cost != null ? Math.max(0, all.cost - sum('cost')) : undefined };
  };

  const bars = $derived(
    (report?.buckets ?? []).map((b) => {
      const parts = series.map((s) => ({ s, a: amount(b, s) }));
      return {
        start: b.start,
        parts,
        total: parts.reduce((t, p) => t + p.a.kwh, 0),
        cost: parts.some((p) => p.a.cost != null) ? parts.reduce((t, p) => t + (p.a.cost ?? 0), 0) : null,
      };
    }),
  );

  let W = $state(720);
  const H = 240;
  const B = 24;
  const T = 18;
  const max = $derived(niceMax(Math.max(0, ...bars.map((b) => b.total))));
  const slot = $derived(bars.length ? W / bars.length : 0);
  const y = (v) => T + (H - B - T) * (1 - v / max);

  function niceMax(v) {
    if (v <= 0) return 1;
    const p = 10 ** Math.floor(Math.log10(v));
    return [1, 2, 2.5, 5, 10].map((m) => m * p).find((m) => m >= v);
  }

  const tz = $derived(report?.timezone);
  function tick(ts, i) {
    const d = new Date(ts);
    if (range === 'hour') return i % 4 === 0 ? `${d.toLocaleTimeString(locale(), { hour: '2-digit', timeZone: tz })}` : '';
    if (range === 'day') return i % 6 === 0 ? d.toLocaleDateString(locale(), { day: 'numeric', month: 'short', timeZone: tz }) : '';
    return d.toLocaleDateString(locale(), { month: 'short', timeZone: tz }).replace('.', '');
  }
  function when(ts) {
    const d = new Date(ts);
    const s =
      range === 'hour'
        ? d.toLocaleString(locale(), { weekday: 'long', hour: '2-digit', minute: '2-digit', timeZone: tz })
        : range === 'day'
          ? d.toLocaleDateString(locale(), { weekday: 'long', day: 'numeric', month: 'long', timeZone: tz })
          : d.toLocaleDateString(locale(), { month: 'long', year: 'numeric', timeZone: tz });
    return s.charAt(0).toUpperCase() + s.slice(1);
  }

  const totals = $derived.by(() => {
    const per = series.map((s) => ({
      s,
      kwh: bars.reduce((t, b) => t + (b.parts.find((p) => p.s === s)?.a.kwh ?? 0), 0),
      cost: bars.reduce((t, b) => t + (b.parts.find((p) => p.s === s)?.a.cost ?? 0), 0),
    }));
    return { per, kwh: per.reduce((t, p) => t + p.kwh, 0), cost: bars.some((b) => b.cost != null) ? per.reduce((t, p) => t + p.cost, 0) : null };
  });

  const shown = $derived(hover != null ? bars[hover] : null);
</script>

{#if error}
  <p class="muted">{t('energie.chart.indisponible', { error })}</p>
{:else if !report}
  <p class="muted">{t('energie.chargement')}</p>
{:else}
  <div class="readout" aria-live="polite">
    {#if shown}
      <span class="when">{when(shown.start)}</span>
      <strong class="num">{kwh(shown.total)} kWh</strong>
      {#if shown.cost != null}<span class="num">{euros(shown.cost, report.currency)}</span>{/if}
    {:else}
      <span class="when">{t('energie.chart.periode')}</span>
      <strong class="num">{kwh(totals.kwh)} kWh</strong>
      {#if totals.cost != null}<span class="num">{euros(totals.cost, report.currency)}</span>{/if}
    {/if}
  </div>

  <div class="plot" bind:clientWidth={W}>
  <svg viewBox="0 0 {W} {H}" height={H} role="img" aria-label={t('energie.chart.conso')} onpointerleave={() => (hover = null)}>
    {#each [0.5, 1] as f (f)}
      <line x1="0" x2={W} y1={y(max * f)} y2={y(max * f)} class="rule" />
    {/each}
    {#each bars as bar, i (bar.start)}
      {@const x = i * slot + slot * 0.16}
      {@const w = slot * 0.68}
      <g class:dim={hover != null && hover !== i}>
        <rect x={i * slot} y="0" width={slot} height={H} class="hit" role="presentation" onpointerenter={() => (hover = i)} />
        {#each bar.parts.reduce((acc, p) => [...acc, { p, base: acc.at(-1)?.top ?? 0, top: (acc.at(-1)?.top ?? 0) + p.a.kwh }], []) as seg, k (seg.p.s.id)}
          {#if seg.p.a.kwh > 0}
            <rect {x} width={w} y={y(seg.top)} height={Math.max(0, y(seg.base) - y(seg.top) - (k ? 1.5 : 0))} rx={Math.min(5, w / 3)} fill={seg.p.s.color} />
          {/if}
        {/each}
      </g>
      <text x={x + w / 2} y={H - 4} class="axis" text-anchor="middle">{tick(bar.start, i)}</text>
    {/each}
    {#each [0.5, 1] as f (f)}
      <text x="2" y={y(max * f) - 5} class="axis halo">{kwh(max * f)} kWh</text>
    {/each}
  </svg>
  </div>

  <ul class="legend" class:hovering={shown}>
    {#each shown ? shown.parts.map((p) => ({ s: p.s, kwh: p.a.kwh, cost: p.a.cost ?? 0 })) : totals.per as row (row.s.id)}
      {#if row.kwh > 0.0005}
        <li>
          <i style="background:{row.s.color}"></i>
          <span>{row.s.name}{#if row.s.estimated} <small class="muted">≈</small>{/if}</span>
          <b class="num">{kwh(row.kwh)} kWh</b>
          {#if row.cost > 0}<small class="muted num">{euros(row.cost, report.currency)}</small>{/if}
        </li>
      {/if}
    {/each}
  </ul>
{/if}

<style>
  .readout {
    display: flex;
    align-items: baseline;
    flex-wrap: wrap;
    gap: 6px 14px;
    min-height: 36px;
    margin-bottom: 8px;
  }

  .when {
    color: var(--ink-3);
    font-weight: 600;
    font-size: 14px;
  }

  .readout strong {
    font-size: 24px;
    font-weight: 650;
    letter-spacing: -0.02em;
  }

  .plot {
    width: 100%;
    min-width: 0;
    overflow: hidden;
  }

  /* The width follows the card (never the other way round). */
  svg {
    display: block;
    width: 100%;
    touch-action: pan-y;
  }

  .rule {
    stroke: var(--line);
    stroke-dasharray: 3 5;
  }

  .axis {
    font-size: 11px;
    fill: var(--ink-3);
    font-weight: 600;
  }

  .hit {
    fill: transparent;
  }

  .halo {
    paint-order: stroke;
    stroke: var(--surface);
    stroke-width: 4px;
    stroke-linejoin: round;
  }

  g {
    transition: opacity 0.2s;
  }

  g.dim {
    opacity: 0.4;
  }

  .legend {
    list-style: none;
    margin: 16px 0 0;
    padding: 0;
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(220px, 1fr));
    gap: 8px 20px;
  }

  .legend li {
    display: grid;
    grid-template-columns: auto 1fr auto;
    align-items: baseline;
    gap: 2px 10px;
    font-size: 14px;
  }

  .legend i {
    width: 10px;
    height: 10px;
    border-radius: 4px;
    align-self: center;
  }

  .legend small {
    grid-column: 3;
    justify-self: end;
    font-size: 12px;
  }

  .legend b {
    font-weight: 650;
  }
</style>
