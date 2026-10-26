<script>
  /** Bars stacked by series, the electricity page's: one bar per bucket
   *  (`bars`, each with its `start`), its parts in `order` (the bottom of
   *  each stack first, each with its `color`), `amount(bar, id)` high,
   *  each part but the top one leaving a thin gap of the card's colour
   *  above it. Hovering or touching a bar dims the others and sets `hover`
   *  (its index) for the figures beside the chart. Under each bar its
   *  `tick`; optional: `rules` (the half and full rules' labels), a figure
   *  `above` a bar, and a dashed mark on the baseline where a bar is
   *  `unknown` (not zero: not counted). */
  let {
    bars,
    order,
    amount,
    tick,
    label,
    height = 240,
    floor = 0,
    rules = null,
    above = null,
    unknown = null,
    title = null,
    hover = $bindable(null),
  } = $props();

  let W = $state(720);
  const B = 24;
  const T = 18;
  const GAP = 2;
  const totals = $derived(bars.map((bar) => order.reduce((sum, s) => sum + Math.max(0, amount(bar, s.id) ?? 0), 0)));
  const max = $derived(niceMax(Math.max(floor, ...totals)));
  const slot = $derived(bars.length ? W / bars.length : 0);
  const barW = $derived(Math.max(3, Math.min(slot * 0.72, 28)));
  const y = (v) => T + (height - B - T) * (1 - v / max);

  function stack(bar) {
    let base = 0;
    const segs = [];
    for (const s of order) {
      const v = amount(bar, s.id) ?? 0;
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
</script>

<div class="plot" bind:clientWidth={W}>
  <svg viewBox="0 0 {W} {height}" {height} role="img" aria-label={label} onpointerleave={() => (hover = null)}>
    {#if rules}
      {#each [0.5, 1] as f (f)}
        <line x1="0" x2={W} y1={y(max * f)} y2={y(max * f)} class="rule" />
      {/each}
    {/if}
    <line x1="0" x2={W} y1={y(0) + 0.5} y2={y(0) + 0.5} class="base" />
    {#each bars as bar, i (bar.start)}
      {@const x = i * slot + (slot - barW) / 2}
      <g class:dim={hover != null && hover !== i}>
        <rect x={i * slot} y="0" width={slot} height={height} class="hit" role="presentation" onpointerenter={() => (hover = i)} onpointerdown={() => (hover = i)}>
          {#if title}<title>{title(bar, i)}</title>{/if}
        </rect>
        {#each stack(bar) as seg (seg.s.id)}
          {#if seg.h > 0}<rect {x} width={barW} y={seg.y1} height={seg.h} fill={seg.s.color} />{/if}
        {/each}
        {#if totals[i] > 0 && above?.(bar, i)}
          <text x={x + barW / 2} y={y(totals[i]) - 4} class="value" text-anchor="middle">{above(bar, i)}</text>
        {:else if totals[i] === 0 && unknown?.(bar, i)}
          <line x1={x} x2={x + barW} y1={y(0) - 1.5} y2={y(0) - 1.5} class="unknown" />
        {/if}
      </g>
      <!-- Kept inside the chart: a label at either end is not cut. -->
      <text x={Math.min(W - 16, Math.max(16, x + barW / 2))} y={height - 4} class="axis" text-anchor="middle">{tick(bar, i)}</text>
    {/each}
    {#if rules}
      {#each [0.5, 1] as f (f)}
        <text x="2" y={y(max * f) - 5} class="axis halo">{rules(max * f)}</text>
      {/each}
    {/if}
  </svg>
</div>

<style>
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

  .value {
    font-size: 11px;
    fill: var(--ink-2);
    font-weight: 650;
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

  .unknown {
    stroke: var(--ink-3);
    stroke-width: 1.5;
    stroke-dasharray: 2 2;
    opacity: 0.5;
  }

  g {
    transition: opacity 0.2s;
  }

  g.dim {
    opacity: 0.35;
  }
</style>
