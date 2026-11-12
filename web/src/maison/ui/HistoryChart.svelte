<script>
  import { onMount } from 'svelte';
  import { hub, device, nameOf, clock } from '../lib/home.svelte.js';
  import { t, locale } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';

  /** Several points over the last minutes or hours, one chart per unit
   *  (kW with kW, A with A), a legend that hides or shows each curve and
   *  says its value now. Read again on its own, more often for a short
   *  window, never while the page is hidden. Configured in `home.json`
   *  (`charts`): 8 points at most, from 1 minute to a week. */
  let { title, minutes = 60, points = [] } = $props();

  const MAX_POINTS = 8;
  const COLORS = ['var(--cool)', 'var(--warm)', 'var(--alert)', 'var(--good)', 'var(--sun)', 'var(--ink-3)', 'var(--warm-ink)', 'var(--ink)'];
  const window_ = $derived(Math.min(10_080, Math.max(1, Number(minutes) || 60)));
  const split = (p) => [p.slice(0, p.indexOf('/')), p.slice(p.indexOf('/') + 1)];
  // A point is `device/key`; once the house is known, one it does not have
  // is left aside (and said so below the chart).
  const wellFormed = (p) => typeof p === 'string' && p.indexOf('/') > 0 && p.indexOf('/') < p.length - 1;
  const known = (p) => {
    if (!Object.keys(hub.devices ?? {}).length) return true;
    const [id, key] = split(p);
    return !!device(id)?.points?.some((s) => s.key === key);
  };
  const unique = $derived([...new Set(points)]);
  const drawn = $derived(unique.filter((p) => wellFormed(p) && known(p)).slice(0, MAX_POINTS));
  const leftOut = $derived(unique.filter((p) => !drawn.includes(p)));
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
    v == null ? '—' : `${v.toLocaleString(locale(), { maximumFractionDigits: Math.abs(v) < 10 ? 2 : 1 })}${unit ? ` ${unit}` : ''}`;

  let series = $state({});
  let hidden = $state(new Set());
  let failed = $state(false);

  async function load() {
    const hours = window_ / 60;
    const wanted = drawn;
    const got = await Promise.all(
      wanted.map((p) =>
        fetch(`/api/history?point=${encodeURIComponent(p)}&hours=${hours}&points=240`)
          .then((r) => (r.ok ? r.json() : null))
          .catch(() => null),
      ),
    );
    const next = {};
    wanted.forEach((p, i) => (next[p] = got[i]));
    failed = got.length > 0 && got.every((s) => s == null);
    series = next;
  }

  // Read again on its own while the page is seen; a hidden tab asks
  // nothing (a small machine serves every open screen), and catches up
  // when it shows again.
  onMount(() => {
    const every = Math.min(60, Math.max(10, window_)) * 1000;
    let timer = null;
    const start = () => {
      if (timer || document.hidden) return;
      load();
      timer = setInterval(load, every);
    };
    const stop = () => {
      clearInterval(timer);
      timer = null;
    };
    const seen = () => (document.hidden ? stop() : start());
    document.addEventListener('visibilitychange', seen);
    start();
    return () => {
      stop();
      document.removeEventListener('visibilitychange', seen);
    };
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
    drawn.forEach((p, i) => {
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
  /** Room above the plot for the unit. */
  const T = 16;
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
    const y = (v) => H - B - ((v - sc.lo) / (sc.hi - sc.lo)) * (H - B - T);
    return pts.map(([ts, v], i) => `${i ? 'L' : 'M'}${x(ts).toFixed(1)},${y(v).toFixed(1)}`).join(' ');
  };
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
            {@const yy = H - B - f * (H - B - T)}
            <line class="grid" x1={L} x2={W - 4} y1={yy} y2={yy} />
            <text class="axis" x={L - 4} y={yy + 3} text-anchor="end">{show(sc.lo + f * (sc.hi - sc.lo), '')}</text>
          {/each}
          <text class="axis unit" x={L} y="8" text-anchor="start">{g.unit}</text>
          {#each ticks as tk, i}
            {@const xx = L + (i / 4) * (W - L - 4)}
            <text class="axis" x={xx} y={H - 4} text-anchor={i === 0 ? 'start' : i === 4 ? 'end' : 'middle'}>{clock(tk)}</text>
          {/each}
          {#each g.curves as c (c.point)}
            {#if !hidden.has(c.point)}
              <path d={pathOf(c.pts, sc)} style:stroke={c.color} />
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
  {#if leftOut.length}
    <p class="muted small left-out">{t('moli.historique.ecartes', { points: leftOut.join(', ') })}</p>
  {/if}
</section>

<style>
  /* Spaced like the other cards of its column. */
  .hchart {
    margin-top: 20px;
  }

  .left-out {
    margin: 0;
    overflow-wrap: anywhere;
  }

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
