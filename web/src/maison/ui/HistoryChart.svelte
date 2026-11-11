<script>
  import { onMount } from 'svelte';
  import { hub, device, nameOf } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';

  /** Several points over the last minutes or hours, one chart per unit
   *  (kW with kW, A with A), a legend that hides or shows each curve and
   *  says its value now. Read again on its own, more often for a short
   *  window. Configured in `home.json` (`charts`). */
  let { title, minutes = 60, points = [] } = $props();

  const COLORS = ['#3b6fd8', '#e0a43a', '#e05a4f', '#4fb6a0', '#9b7bf0', '#7a8597', '#d46fb0'];
  const split = (p) => [p.slice(0, p.indexOf('/')), p.slice(p.indexOf('/') + 1)];
  const specOf = (p) => {
    const [id, key] = split(p);
    return device(id)?.points.find((s) => s.key === key);
  };
  const unitOf = (p) => specOf(p)?.unit ?? '';
  const labelOf = (p) => {
    const [id, key] = split(p);
    return `${nameOf(id)} ▸ ${specOf(p)?.label ?? key}`;
  };
  const now = (p) => {
    const [id, key] = split(p);
    const v = hub.devices[id]?.state?.[key]?.value;
    return typeof v === 'number' ? v : null;
  };
  const show = (v, unit) =>
    v == null ? '—' : `${v.toLocaleString(undefined, { maximumFractionDigits: Math.abs(v) < 10 ? 2 : 1 })}${unit ? ` ${unit}` : ''}`;

  let series = $state({});
  let hidden = $state(new Set());
  let failed = $state(false);

  async function load() {
    const hours = minutes / 60;
    const got = await Promise.all(
      points.map((p) =>
        fetch(`/api/history?point=${encodeURIComponent(p)}&hours=${hours}&points=240`)
          .then((r) => (r.ok ? r.json() : null))
          .catch(() => null),
      ),
    );
    const next = {};
    points.forEach((p, i) => (next[p] = got[i]));
    failed = got.every((s) => s == null);
    series = next;
  }

  onMount(() => {
    load();
    const every = Math.min(60, Math.max(10, minutes)) * 1000;
    const timer = setInterval(load, every);
    return () => clearInterval(timer);
  });

  // The values to draw: bucket averages, or each change held until the next.
  const line = (s) => {
    if (!s) return [];
    if (s.buckets?.length) return s.buckets.filter((b) => typeof b.avg === 'number').map((b) => [b.ts, b.avg]);
    const out = [];
    if (s.before && typeof s.before[1] === 'number') out.push([s.from, s.before[1]]);
    for (const [ts, v] of s.raw ?? []) {
      if (typeof v !== 'number') continue;
      if (out.length) out.push([ts, out.at(-1)[1]]);
      out.push([ts, v]);
    }
    if (out.length) out.push([s.to, out.at(-1)[1]]);
    return out;
  };

  // One chart per unit, in the order the points are given.
  const groups = $derived.by(() => {
    const byUnit = new Map();
    points.forEach((p, i) => {
      const u = unitOf(p);
      if (!byUnit.has(u)) byUnit.set(u, []);
      byUnit.get(u).push({ point: p, color: COLORS[i % COLORS.length], pts: line(series[p]) });
    });
    return [...byUnit.entries()].map(([unit, curves]) => ({ unit, curves }));
  });

  const W = 340;
  const H = 150;
  const L = 34;
  const B = 18;
  const span = $derived.by(() => {
    const all = Object.values(series).filter(Boolean);
    if (!all.length) return null;
    return { from: Math.min(...all.map((s) => s.from)), to: Math.max(...all.map((s) => s.to)) };
  });
  const scale = (g) => {
    const vs = g.curves.filter((c) => !hidden.has(c.point)).flatMap((c) => c.pts.map((p) => p[1]));
    if (!vs.length || !span) return null;
    let lo = Math.min(0, ...vs);
    let hi = Math.max(...vs);
    if (hi === lo) hi = lo + 1;
    return { lo, hi };
  };
  const pathOf = (pts, sc) => {
    if (!sc || !span || pts.length < 2) return '';
    const x = (ts) => L + ((ts - span.from) / (span.to - span.from || 1)) * (W - L - 4);
    const y = (v) => H - B - ((v - sc.lo) / (sc.hi - sc.lo)) * (H - B - 8);
    return pts.map(([ts, v], i) => `${i ? 'L' : 'M'}${x(ts).toFixed(1)},${y(v).toFixed(1)}`).join(' ');
  };
  const clock = (ms) => new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
  const ticks = $derived(span ? [0, 0.25, 0.5, 0.75, 1].map((f) => span.from + f * (span.to - span.from)) : []);
  function toggle(p) {
    const next = new Set(hidden);
    next.has(p) ? next.delete(p) : next.add(p);
    hidden = next;
  }
</script>

<section class="card hchart">
  <div class="card-head"><h2><Icon name="history" size={18} />{title}</h2></div>
  {#if failed}
    <p class="muted">{t('moli.historique.indisponible')}</p>
  {:else}
    {#each groups as g (g.unit)}
      {@const sc = scale(g)}
      <svg viewBox="0 0 {W} {H}" role="img" aria-label={g.curves.map((c) => labelOf(c.point)).join(', ')}>
        {#if sc}
          {#each [0, 0.5, 1] as f}
            {@const yy = H - B - f * (H - B - 8)}
            <line class="grid" x1={L} x2={W - 4} y1={yy} y2={yy} />
            <text class="axis" x={L - 4} y={yy + 3} text-anchor="end">{show(sc.lo + f * (sc.hi - sc.lo), '')}</text>
          {/each}
          <text class="axis unit" x={L - 4} y="6" text-anchor="end">{g.unit}</text>
          {#each ticks as tk, i}
            {@const xx = L + (i / 4) * (W - L - 4)}
            <text class="axis" x={xx} y={H - 4} text-anchor={i === 0 ? 'start' : i === 4 ? 'end' : 'middle'}>{clock(tk)}</text>
          {/each}
          {#each g.curves as c (c.point)}
            {#if !hidden.has(c.point)}
              <path d={pathOf(c.pts, sc)} stroke={c.color} />
            {/if}
          {/each}
        {/if}
      </svg>
      <ul class="legend">
        {#each g.curves as c (c.point)}
          <li>
            <button class:off={hidden.has(c.point)} onclick={() => toggle(c.point)} aria-pressed={!hidden.has(c.point)}>
              <span class="dot" style:background={hidden.has(c.point) ? 'transparent' : c.color} style:border-color={c.color}></span>
              <span class="name">{labelOf(c.point)}</span>
              <span class="value num">{show(now(c.point), g.unit)}</span>
            </button>
          </li>
        {/each}
      </ul>
    {/each}
  {/if}
</section>

<style>
  .hchart svg {
    width: 100%;
    height: auto;
    display: block;
  }

  .grid {
    stroke: var(--surface-3);
    stroke-width: 1;
  }

  path {
    fill: none;
    stroke-width: 1.6;
    stroke-linejoin: round;
  }

  .axis {
    fill: var(--ink-3);
    font-size: 9px;
  }

  .legend {
    list-style: none;
    margin: 6px 0 14px;
    padding: 0;
    display: grid;
    gap: 2px;
  }

  .legend button {
    all: unset;
    cursor: pointer;
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    font-size: 0.85rem;
    padding: 3px 0;
  }

  .legend button.off .name {
    opacity: 0.5;
  }

  .dot {
    width: 12px;
    height: 12px;
    border-radius: 50%;
    border: 2px solid;
    flex: none;
  }

  .name {
    flex: 1;
    min-width: 0;
  }

  .value {
    color: var(--ink-3);
  }
</style>
