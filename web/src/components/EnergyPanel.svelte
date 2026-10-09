<script>
  import { kwh, money, meterColor } from '../lib/energy.js';
  import { t, locale } from '../lib/i18n.svelte.js';

  let { onclose } = $props();

  const RANGES = [
    { label: 'systeme.energie.plage_24h', step: 'hour', count: 24 },
    { label: 'systeme.energie.plage_30j', step: 'day', count: 30 },
    { label: 'systeme.energie.plage_12m', step: 'month', count: 12 },
  ];
  let range = $state(RANGES[0]);
  // bill: billed meters (grid) · circuits: breakdown. Hour by hour, the
  // Linky (whole kWh) is too coarse: the fine measure reads better.
  let mode = $state('circuits');
  function pick(r) {
    range = r;
    mode = r.step === 'hour' ? 'circuits' : 'bill';
  }
  let report = $state(null);
  let error = $state('');

  $effect(() => {
    const { step, count } = range;
    error = '';
    fetch(`/api/energy/series?step=${step}&count=${count}`)
      .then(async (r) => {
        if (r.ok) return r.json();
        const text = await r.text();
        let message = text;
        try {
          message = JSON.parse(text).error ?? text;
        } catch {
          /* plain text */
        }
        throw new Error(message || `HTTP ${r.status}`);
      })
      .then((r) => (report = r))
      .catch((e) => (error = e.message));
  });

  const meters = $derived((report?.meters ?? []).map((m, i) => ({ ...m, color: meterColor(m, i) })));
  const hasGrid = $derived(meters.some((m) => m.role === 'grid'));
  const hasCircuits = $derived(meters.some((m) => m.role === 'circuit'));
  const whole = $derived(meters.find((m) => m.role === 'total'));

  // What one bar is made of, in the chosen mode.
  const series = $derived.by(() => {
    if (mode === 'circuits' && hasCircuits) {
      const parts = meters.filter((m) => m.role === 'circuit');
      return whole ? [...parts, { id: '__rest', name: t('systeme.energie.autres'), color: 'var(--line)', rest: true }] : parts;
    }
    const grid = meters.filter((m) => m.role === 'grid');
    return grid.length ? grid : meters.filter((m) => m.role === 'total');
  });

  // « Autres »: the whole-home measure minus the circuits, cost included.
  const amount = (bucket, s) => {
    if (!s.rest) return bucket.meters[s.id] ?? { kwh: 0 };
    const all = bucket.meters[whole.id];
    const circuits = meters.filter((m) => m.role === 'circuit').map((m) => bucket.meters[m.id]).filter(Boolean);
    const sum = (key) => circuits.reduce((t, a) => t + (a[key] ?? 0), 0);
    return {
      kwh: Math.max(0, (all?.kwh ?? 0) - sum('kwh')),
      cost: all?.cost != null ? Math.max(0, all.cost - sum('cost')) : undefined,
    };
  };

  const bars = $derived(
    (report?.buckets ?? []).map((b) => {
      const parts = series.map((s) => ({ s, a: amount(b, s) }));
      const total = parts.reduce((t, p) => t + p.a.kwh, 0);
      const cost = parts.some((p) => p.a.cost != null) ? parts.reduce((t, p) => t + (p.a.cost ?? 0), 0) : null;
      return { start: b.start, parts, total, cost };
    }),
  );

  const W = 760;
  const H = 230;
  const PAD = { l: 52, r: 8, t: 10, b: 24 };
  const axis = (v) => v.toLocaleString(locale(), { maximumFractionDigits: v < 10 ? 1 : 0 });
  const max = $derived(niceMax(Math.max(0, ...bars.map((b) => b.total))));
  const slot = $derived(bars.length ? (W - PAD.l - PAD.r) / bars.length : 0);
  const y = (v) => PAD.t + (H - PAD.t - PAD.b) * (1 - v / max);

  function niceMax(v) {
    if (v <= 0) return 1;
    const p = 10 ** Math.floor(Math.log10(v));
    return [1, 2, 2.5, 5, 10].map((m) => m * p).find((m) => m >= v);
  }

  const tz = $derived(report?.timezone);
  function label(ts, step, i) {
    const d = new Date(ts);
    if (step === 'hour') return i % 3 === 0 ? d.toLocaleTimeString(locale(), { hour: '2-digit', timeZone: tz }) : '';
    if (step === 'day') return i % 5 === 0 ? d.toLocaleDateString(locale(), { day: 'numeric', month: 'short', timeZone: tz }) : '';
    return d.toLocaleDateString(locale(), { month: 'short', timeZone: tz });
  }
  function title(bar, step) {
    const d = new Date(bar.start);
    const when =
      step === 'hour'
        ? d.toLocaleString(locale(), { weekday: 'short', hour: '2-digit', minute: '2-digit', timeZone: tz })
        : step === 'day'
          ? d.toLocaleDateString(locale(), { weekday: 'long', day: 'numeric', month: 'long', timeZone: tz })
          : d.toLocaleDateString(locale(), { month: 'long', year: 'numeric', timeZone: tz });
    const lines = bar.parts.filter((p) => p.a.kwh > 0).map((p) => t('systeme.energie.ligne', { name: p.s.name, kwh: kwh(p.a.kwh) }));
    return [`${when} · ${kwh(bar.total)} ${money(bar.cost, report.currency)}`, ...lines].join('\n');
  }

  const totals = $derived.by(() => {
    const per = series.map((s) => ({
      s,
      kwh: bars.reduce((t, b) => t + (b.parts.find((p) => p.s === s)?.a.kwh ?? 0), 0),
      cost: bars.reduce((t, b) => t + (b.parts.find((p) => p.s === s)?.a.cost ?? 0), 0),
    }));
    const priced = bars.some((b) => b.cost != null);
    return {
      per,
      kwh: per.reduce((t, p) => t + p.kwh, 0),
      cost: priced ? per.reduce((t, p) => t + p.cost, 0) : null,
    };
  });

  function onkey(e) {
    if (e.key === 'Escape') onclose();
  }
</script>

<svelte:window onkeydown={onkey} />

<div class="backdrop" onclick={onclose} role="presentation"></div>
<div class="panel" role="dialog" aria-modal="true" aria-label={t('systeme.energie.titre')}>
  <header>
    <h3>{t('systeme.energie.titre')}</h3>
    <button class="close" onclick={onclose} aria-label={t('commun.fermer')}>×</button>
  </header>

  <div class="controls">
    <div class="seg" role="group" aria-label={t('systeme.energie.periode')}>
      {#each RANGES as r (r.step)}
        <button class:on={range === r} onclick={() => pick(r)}>{t(r.label)}</button>
      {/each}
    </div>
    {#if hasGrid && hasCircuits}
      <div class="seg" role="group" aria-label={t('systeme.energie.vue')}>
        <button class:on={mode === 'bill'} onclick={() => (mode = 'bill')}>{t('systeme.energie.facture')}</button>
        <button class:on={mode === 'circuits'} onclick={() => (mode = 'circuits')}>{t('systeme.energie.circuits')}</button>
      </div>
    {/if}
  </div>

  {#if error}
    <p class="muted">{error}</p>
  {:else if !report}
    <p class="muted">{t('systeme.chargement')}</p>
  {:else}
    <svg viewBox="0 0 {W} {H}" role="img" aria-label={t('systeme.energie.graphique.' + range.step)}>
      {#each [0, 0.5, 1] as f (f)}
        <line x1={PAD.l} x2={W - PAD.r} y1={y(max * f)} y2={y(max * f)} class="grid" />
        <text x={PAD.l - 6} y={y(max * f) + 4} class="axis" text-anchor="end">{axis(max * f)}</text>
      {/each}
      {#each bars as bar, i (bar.start)}
        {@const x = PAD.l + i * slot + slot * 0.12}
        {@const w = slot * 0.76}
        <g>
          <title>{title(bar, range.step)}</title>
          <rect x={PAD.l + i * slot} y={PAD.t} width={slot} height={H - PAD.t - PAD.b} class="hit" />
          {#each bar.parts.reduce((acc, p) => [...acc, { p, base: (acc.at(-1)?.top ?? 0), top: (acc.at(-1)?.top ?? 0) + p.a.kwh }], []) as seg (seg.p.s.id)}
            {#if seg.p.a.kwh > 0}
              <rect {x} width={w} y={y(seg.top)} height={Math.max(0, y(seg.base) - y(seg.top))} fill={seg.p.s.color} />
            {/if}
          {/each}
        </g>
        <text x={x + w / 2} y={H - 6} class="axis" text-anchor="middle">{label(bar.start, range.step, i)}</text>
      {/each}
    </svg>

    <div class="totals">
      <div>
        <span class="big num">{kwh(totals.kwh)}</span>
        {#if totals.cost != null}<span class="muted num">{money(totals.cost, report.currency)}</span>{/if}
      </div>
      <ul>
        {#each totals.per.filter((p) => p.kwh > 0.0005) as p (p.s.id)}
          <li>
            <i style="background:{p.s.color}"></i>{p.s.name}
            <b class="num">{kwh(p.kwh)}</b>
            {#if p.cost > 0}<span class="muted num">{money(p.cost, report.currency)}</span>{/if}
          </li>
        {/each}
      </ul>
    </div>
    <p class="muted">
      {#if mode === 'bill' && hasGrid}
        {t('systeme.energie.note_facture')}
      {:else}
        {t('systeme.energie.note_circuits')}
      {/if}
      {t('systeme.energie.note_historique')}
    </p>
  {/if}
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: color-mix(in srgb, var(--ink) 35%, transparent);
    z-index: 20;
  }
  .panel {
    position: fixed;
    z-index: 21;
    left: 50%;
    top: 50%;
    transform: translate(-50%, -50%);
    width: min(860px, calc(100vw - 32px));
    max-height: calc(100vh - 32px);
    overflow: auto;
    background: var(--card);
    border: var(--stroke) solid var(--ink);
    border-radius: var(--radius);
    padding: 16px 18px;
  }
  header {
    display: flex;
    justify-content: space-between;
    align-items: center;
  }
  h3 {
    margin: 0;
    font-size: 18px;
  }
  .close {
    border: 0;
    background: none;
    font-size: 22px;
    cursor: pointer;
  }
  .controls {
    display: flex;
    flex-wrap: wrap;
    gap: 12px;
    margin: 12px 0;
  }
  .seg {
    display: inline-flex;
    border: var(--stroke) solid var(--ink);
    border-radius: var(--radius);
    overflow: hidden;
  }
  .seg button {
    border: 0;
    background: none;
    padding: 4px 12px;
    font: inherit;
    font-size: 13px;
    cursor: pointer;
  }
  .seg button.on {
    background: var(--ink);
    color: var(--paper);
  }
  svg {
    width: 100%;
    height: auto;
    display: block;
  }
  .grid {
    stroke: var(--line);
    stroke-width: 1;
  }
  .axis {
    font-size: 11px;
    fill: var(--ink-3);
    font-family: var(--mono);
  }
  .hit {
    fill: transparent;
  }
  g:hover .hit {
    fill: color-mix(in srgb, var(--sun) 14%, transparent);
  }
  .totals {
    display: flex;
    flex-wrap: wrap;
    gap: 10px 28px;
    align-items: baseline;
    margin-top: 10px;
  }
  .totals > div {
    display: grid;
  }
  .big {
    font-size: 26px;
    font-weight: 800;
    letter-spacing: -0.02em;
  }
  ul {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 18px;
    list-style: none;
    margin: 0;
    padding: 0;
    font-size: 13px;
  }
  li i {
    display: inline-block;
    width: 9px;
    height: 9px;
    border-radius: 2px;
    margin-right: 6px;
  }
  li b {
    font-weight: 600;
    margin-left: 4px;
  }
  .muted {
    color: var(--ink-3);
    font-size: 13px;
  }
</style>
