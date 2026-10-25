<script>
  import { kwh, euros } from '../lib/energy-live.svelte.js';
  import { deviceSeries } from '../lib/energy-series.js';
  import { seriesColor, NEUTRAL } from '../lib/palette.js';
  import { t, locale } from '../../lib/i18n.svelte.js';

  /** Bars over 24 h / 30 days / 12 months, stacked by what drew it
   *  (`by` = 'devices': circuits, appliances, lights, the rest) or as
   *  billed ('tariff': off-peak and peak). Each stack puts what drew the
   *  most over the period at the bottom; the table under it lists every
   *  part in that order (the biggest on top, a part that drew nothing at
   *  the bottom with its zeros), off-peak and peak kWh and euros aligned.
   *  Hovering a bar, the table says that bar. */
  let { range = 'day', by = 'devices' } = $props();

  const RANGES = {
    hour: { step: 'hour', count: 24 },
    day: { step: 'day', count: 30 },
    month: { step: 'month', count: 12 },
  };

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

  // Each part keeps its colour whatever the range: given by what it drew
  // over the last twelve months, the biggest first (the stacks, sorted
  // the same way, then mostly lay the palette's neighbours side by side).
  let year = $state(null);
  $effect(() => {
    fetch('/api/energy/series?step=month&count=12')
      .then((r) => (r.ok ? r.json() : null))
      .then((r) => (year = r))
      .catch(() => {});
  });

  // Hour by hour, the Linky (whole kWh) is too coarse: circuits then.
  const meters = $derived(report?.meters ?? []);
  const circuits = $derived(meters.filter((m) => m.role === 'circuit'));
  const whole = $derived(meters.find((m) => m.role === 'total'));
  const grid = $derived(meters.filter((m) => m.role === 'grid'));
  const byCircuit = $derived(range === 'hour' || !grid.length);
  const parts = $derived(deviceSeries(meters));

  const colors = $derived.by(() => {
    const ranked = deviceSeries(year?.meters ?? meters)
      .filter((p) => !p.rest)
      .map((p) => ({ id: p.id, kwh: (year?.buckets ?? []).reduce((t, b) => t + p.kwh(b), 0) }))
      .sort((a, b) => b.kwh - a.kwh);
    return new Map(ranked.map((p, i) => [p.id, seriesColor(i)]));
  });
  const colorOf = (p) => (p.rest ? NEUTRAL : (colors.get(p.id) ?? NEUTRAL));

  // Off-peak and peak as the dated contracts split them, bucket by bucket.
  const periods = $derived((report?.buckets ?? []).some((b) => b.by_period && Object.keys(b.by_period).length));
  const PERIODS = [
    { id: 'HC', name: 'energie.hc', color: 'var(--cool)' },
    { id: 'HP', name: 'energie.hp', color: 'var(--warm)' },
    { id: 'Base', name: 'energie.base', color: 'var(--warm)' },
  ];

  const series = $derived.by(() => {
    if (by === 'devices' && parts.length) return parts.map((p) => ({ ...p, color: colorOf(p) }));
    if (by !== 'devices' && periods) {
      return PERIODS.map((p) => ({
        id: p.id,
        name: t(p.name),
        color: p.color,
        period: true,
        kwh: (b) => b.by_period?.[p.id]?.kwh ?? 0,
        cost: (b) => b.by_period?.[p.id]?.cost ?? null,
      }));
    }
    if (byCircuit && circuits.length) {
      const list = circuits.map((m, i) => ({ id: m.id, name: m.name, color: colors.get(m.id) ?? seriesColor(i) }));
      return whole ? [...list, { id: '__rest', name: t('energie.reste'), color: NEUTRAL, rest: true }] : list;
    }
    const billed = grid.length ? grid : meters.filter((m) => m.role === 'total');
    return billed.map((m) => ({
      id: m.id,
      name: /hc|creuse/i.test(m.id + m.name) ? t('energie.hc') : /hp|pleine/i.test(m.id + m.name) ? t('energie.hp') : m.name,
      color: /hc|creuse/i.test(m.id + m.name) ? 'var(--cool)' : 'var(--warm)',
      period: true,
    }));
  });

  /** What a part drew in a bucket: kWh, euros, and off-peak / peak when
   *  the contract split that meter's hours. */
  const amount = (bucket, s) => {
    if (s.kwh) {
      const split = s.split?.(bucket) ?? null;
      const cost = split ? split.hcCost + split.hpCost : (s.cost(bucket) ?? null);
      return { kwh: s.kwh(bucket), cost, split };
    }
    if (!s.rest) {
      const a = bucket.meters[s.id] ?? { kwh: 0 };
      return { kwh: a.kwh, cost: a.cost ?? null, split: null };
    }
    const all = bucket.meters[whole.id];
    const list = circuits.map((m) => bucket.meters[m.id]).filter(Boolean);
    const sum = (k) => list.reduce((t, a) => t + (a[k] ?? 0), 0);
    return { kwh: Math.max(0, (all?.kwh ?? 0) - sum('kwh')), cost: all?.cost != null ? Math.max(0, all.cost - sum('cost')) : null, split: null };
  };

  const bars = $derived(
    (report?.buckets ?? []).map((b) => {
      const amounts = new Map(series.map((s) => [s.id, amount(b, s)]));
      const list = [...amounts.values()];
      return {
        start: b.start,
        amounts,
        total: list.reduce((t, a) => t + a.kwh, 0),
        cost: list.some((a) => a.cost != null) ? list.reduce((t, a) => t + (a.cost ?? 0), 0) : null,
      };
    }),
  );

  // Over the period, each part: kWh, euros, off-peak and peak.
  const sumOf = (list) => {
    const priced = list.some((a) => a.cost != null);
    const split = list.length > 0 && list.every((a) => a.split || a.kwh < 0.0005);
    const add = (f) => list.reduce((t, a) => t + (f(a) ?? 0), 0);
    return {
      kwh: add((a) => a.kwh),
      cost: priced ? add((a) => a.cost) : null,
      split: split ? { hc: add((a) => a.split?.hc), hp: add((a) => a.split?.hp) } : null,
    };
  };
  const totals = $derived(new Map(series.map((s) => [s.id, sumOf(bars.map((b) => b.amounts.get(s.id)))])));

  // The order of the period: most at the bottom of each stack, at the top
  // of the table; nothing drawn last (« le reste » left out then).
  const order = $derived(
    [...series]
      .filter((s) => !(s.rest || s.period) || (totals.get(s.id)?.kwh ?? 0) > 0.0005)
      .sort((a, b) => (totals.get(b.id)?.kwh ?? 0) - (totals.get(a.id)?.kwh ?? 0)),
  );
  // Off-peak and peak columns when every part knows its split.
  const splitShown = $derived(by === 'devices' && order.length > 0 && order.every((s) => totals.get(s.id)?.split));

  let W = $state(720);
  const H = 240;
  const B = 24;
  const T = 18;
  const GAP = 2;
  const max = $derived(niceMax(Math.max(0, ...bars.map((b) => b.total))));
  const slot = $derived(bars.length ? W / bars.length : 0);
  const barW = $derived(Math.max(3, Math.min(slot * 0.72, 28)));
  const y = (v) => T + (H - B - T) * (1 - v / max);

  /** A bar's segments, the biggest part of the period at the bottom; each
   *  but the top one leaves a thin gap of the card's colour above it. */
  function stack(bar) {
    let base = 0;
    const segs = [];
    for (const s of order) {
      const v = bar.amounts.get(s.id)?.kwh ?? 0;
      if (v <= 0) continue;
      segs.push({ s, y0: y(base), y1: y(base + v) });
      base += v;
    }
    return segs.map((g, i) => ({ ...g, h: Math.max(0, g.y0 - g.y1 - (i < segs.length - 1 ? GAP : 0)) }));
  }

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

  const shown = $derived(hover != null ? bars[hover] : null);
  // The table: the hovered bar's figures, else the period's, in the period's order.
  const rows = $derived(order.map((s) => ({ s, a: shown ? sumOf([shown.amounts.get(s.id)]) : totals.get(s.id) })));
  const sumRow = $derived(sumOf(rows.map((r) => ({ kwh: r.a.kwh, cost: r.a.cost, split: r.a.split }))));
  const cur = $derived(report?.currency ?? '€');
  const zero = (v) => v == null || v < 0.0005;
  // In the table: the column says the unit; nothing drawn is a plain 0.
  const k = (v) => (zero(v) ? '0' : kwh(v));
  const money = (v) => (v == null ? '—' : v < 0.005 ? '0' : v.toLocaleString(locale(), { minimumFractionDigits: 2, maximumFractionDigits: 2 }));
</script>

{#if error}
  <p class="muted">{t('energie.chart.indisponible', { error })}</p>
{:else if !report}
  <p class="muted">{t('energie.chargement')}</p>
{:else}
  <div class="readout" aria-live="polite">
    <span class="when">{shown ? when(shown.start) : t('energie.chart.periode')}</span>
    <strong class="num">{kwh(shown ? shown.total : sumRow.kwh)} kWh</strong>
    {#if (shown ? shown.cost : sumRow.cost) != null}<span class="num">{euros(shown ? shown.cost : sumRow.cost, cur)}</span>{/if}
  </div>

  <div class="plot" bind:clientWidth={W}>
    <svg viewBox="0 0 {W} {H}" height={H} role="img" aria-label={t('energie.chart.conso')} onpointerleave={() => (hover = null)}>
      {#each [0.5, 1] as f (f)}
        <line x1="0" x2={W} y1={y(max * f)} y2={y(max * f)} class="rule" />
      {/each}
      <line x1="0" x2={W} y1={y(0) + 0.5} y2={y(0) + 0.5} class="base" />
      {#each bars as bar, i (bar.start)}
        {@const x = i * slot + (slot - barW) / 2}
        <g class:dim={hover != null && hover !== i}>
          <rect x={i * slot} y="0" width={slot} height={H} class="hit" role="presentation" onpointerenter={() => (hover = i)} />
          {#each stack(bar) as seg (seg.s.id)}
            {#if seg.h > 0}<rect {x} width={barW} y={seg.y1} height={seg.h} fill={seg.s.color} />{/if}
          {/each}
        </g>
        <text x={x + barW / 2} y={H - 4} class="axis" text-anchor="middle">{tick(bar.start, i)}</text>
      {/each}
      {#each [0.5, 1] as f (f)}
        <text x="2" y={y(max * f) - 5} class="axis halo">{kwh(max * f)} kWh</text>
      {/each}
    </svg>
  </div>

  <table class="legend">
    <thead>
      <tr>
        <th scope="col" class="name">{by === 'devices' ? t('energie.chart.appareil') : t('energie.chart.tarif')}</th>
        {#if splitShown}
          <th scope="col"><i class="tag cool"></i>{t('energie.chart.creuses')}<small>kWh</small></th>
          <th scope="col"><i class="tag warm"></i>{t('energie.chart.pleines')}<small>kWh</small></th>
        {:else}
          <th scope="col">{t('energie.chart.energie')}<small>kWh</small></th>
        {/if}
        <th scope="col">{t('energie.chart.cout')}<small>{cur}</small></th>
      </tr>
    </thead>
    <tbody>
      {#each rows as { s, a } (s.id)}
        <tr class:none={zero(a.kwh)}>
          <th scope="row" class="name"><span class="who"><i class="swatch" style:background={s.color}></i><span>{s.name}{#if s.estimated} <small class="muted">≈</small>{/if}</span></span></th>
          {#if splitShown}
            <td class="num">{k(a.split?.hc)}</td>
            <td class="num">{k(a.split?.hp)}</td>
          {:else}
            <td class="num">{k(a.kwh)}</td>
          {/if}
          <td class="num">{money(a.cost)}</td>
        </tr>
      {/each}
    </tbody>
    {#if rows.length > 1}
      <tfoot>
        <tr>
          <th scope="row" class="name"><span>{t('energie.chart.total')}</span></th>
          {#if splitShown}
            <td class="num">{k(sumRow.split?.hc)}</td>
            <td class="num">{k(sumRow.split?.hp)}</td>
          {:else}
            <td class="num">{k(sumRow.kwh)}</td>
          {/if}
          <td class="num">{money(sumRow.cost)}</td>
        </tr>
      </tfoot>
    {/if}
  </table>
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
    stroke-width: 1;
  }

  .base {
    stroke: var(--ink-3);
    stroke-width: 1;
    opacity: 0.5;
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
    opacity: 0.35;
  }

  /* The table: one line per part, figures in aligned columns. */
  .legend {
    width: 100%;
    margin-top: 16px;
    border-collapse: collapse;
    font-size: 14px;
  }

  .legend th,
  .legend td {
    padding: 6px 0 6px 14px;
    text-align: right;
    font-weight: 600;
    white-space: nowrap;
  }

  .legend thead th {
    padding-top: 0;
    font-size: 11.5px;
    font-weight: 700;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--ink-3);
    border-bottom: 1px solid var(--line);
  }

  .legend .name {
    padding-left: 0;
    text-align: left;
    width: 100%;
    white-space: normal;
  }

  .who {
    display: flex;
    align-items: center;
    gap: 10px;
  }

  .legend tbody tr + tr th,
  .legend tbody tr + tr td {
    border-top: 1px solid color-mix(in srgb, var(--line) 60%, transparent);
  }

  .legend td {
    font-variant-numeric: tabular-nums;
    color: var(--ink);
  }

  .legend tr.none th,
  .legend tr.none td {
    color: var(--ink-3);
  }

  .legend tfoot th,
  .legend tfoot td {
    border-top: 1px solid var(--line);
    font-weight: 750;
  }

  .swatch {
    flex: none;
    width: 12px;
    height: 12px;
    border-radius: 3px;
  }

  .tag {
    display: inline-block;
    width: 8px;
    height: 8px;
    border-radius: 2px;
    margin-right: 6px;
    vertical-align: 1px;
  }

  .tag.cool {
    background: var(--cool);
  }

  .tag.warm {
    background: var(--warm);
  }

  /* The unit under its column's name. */
  .legend thead small {
    display: block;
    font-size: 10.5px;
    font-weight: 600;
    letter-spacing: 0;
    text-transform: none;
  }

  .legend thead th {
    vertical-align: bottom;
  }

  @media (max-width: 560px) {
    .legend {
      font-size: 13px;
    }

    .legend th,
    .legend td {
      padding-left: 10px;
    }

    .legend thead th {
      font-size: 10.5px;
      letter-spacing: 0.03em;
    }

    .who {
      gap: 8px;
    }
  }
</style>
