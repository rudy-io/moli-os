<script>
  import { num } from '../lib/home.svelte.js';
  import { statsOf, valueAt } from '../lib/temperatures.js';
  import { t, locale } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';

  /** One room's temperature over time: a line per thing that measures it
   *  (named in a legend when there are several), outside dashed when asked,
   *  on the scale every room shares (`scale`). Its figures: now, lowest and
   *  highest. Hovering or touching a moment (`hover`, shared by every room)
   *  says the values then. */
  let { name, icon = 'home', lines = [], outdoor = null, scale, from, to, range, hover = $bindable(null) } = $props();

  let W = $state(360);
  const H = 150;
  const L = 30;
  const T = 8;
  const B = 20;
  const x = (ts) => L + ((ts - from) / (to - from)) * (W - L - 4);
  const y = (v) => T + (H - T - B) * (1 - (v - scale.min) / (scale.max - scale.min));
  const path = (line) => line.map(([ts, v], i) => `${i ? 'L' : 'M'}${x(ts).toFixed(1)},${y(v).toFixed(1)}`).join('');

  const main = $derived(lines.length === 1 ? lines[0] : null);
  const all = $derived(lines.flatMap((l) => l.line));
  const stats = $derived(statsOf([...all].sort((a, b) => a[0] - b[0])));
  // Now: the measures' average, as the room's card says it.
  const nowAvg = $derived.by(() => {
    const last = lines.map((l) => l.line.at(-1)?.[1]).filter((v) => typeof v === 'number');
    return last.length ? last.reduce((a, b) => a + b, 0) / last.length : null;
  });
  // A point is close enough to a moment within a twentieth of the span.
  const gap = $derived((to - from) / 20);
  const at = $derived(
    hover == null
      ? null
      : {
          values: lines.map((l) => ({ ...l, v: valueAt(l.line, hover, gap) })),
          out: outdoor ? valueAt(outdoor, hover, gap) : null,
        },
  );

  const atAvg = $derived.by(() => {
    const vs = (at?.values ?? []).map((l) => l.v).filter((v) => v != null);
    return vs.length ? vs.reduce((a, b) => a + b, 0) / vs.length : null;
  });

  const DAY = 24 * 3_600_000;
  const xticks = $derived.by(() => {
    const out = [];
    const step = range === 'day' ? 6 * 3_600_000 : range === 'week' ? DAY : 7 * DAY;
    const d = new Date(from);
    d.setMinutes(0, 0, 0);
    if (range !== 'day') d.setHours(0);
    else d.setHours(Math.ceil(d.getHours() / 6) * 6);
    for (let ts = d.getTime(); ts <= to; ts += step) {
      if (ts < from) continue;
      const day = new Date(ts);
      const text =
        range === 'day'
          ? t('maison.temperatures.heure', { h: day.getHours() })
          : range === 'week'
            ? day.toLocaleDateString(locale(), { weekday: 'short' }).replace('.', '')
            : day.toLocaleDateString(locale(), { day: 'numeric', month: 'short' }).replace('.', '');
      out.push({ ts, text });
    }
    return out;
  });
  const when = (ts) => {
    const d = new Date(ts);
    return range === 'day'
      ? d.toLocaleTimeString(locale(), { hour: '2-digit', minute: '2-digit' })
      : d.toLocaleString(locale(), { weekday: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' });
  };

  let svg = $state();
  function point(e) {
    const r = svg.getBoundingClientRect();
    const px = ((e.clientX - r.left) / r.width) * W;
    if (px < L) return;
    hover = Math.min(to, Math.max(from, from + ((px - L) / (W - L - 4)) * (to - from)));
  }
  const deg = (v) => (v == null ? '—' : `${num(v, 1)}°`);
</script>

<section class="card room">
  <header>
    <h2><Icon name={icon} size={18} />{name}</h2>
    <div class="figure" aria-live="polite">
      {#if at}
        <span class="when">{when(hover)}</span>
        <strong class="num">{deg(atAvg)}</strong>
      {:else}
        <strong class="num">{deg(nowAvg)}</strong>
      {/if}
    </div>
  </header>
  {#if stats}
    <p class="range muted num">
      {t('maison.temperatures.min_max', { min: deg(stats.min), max: deg(stats.max) })}
      {#if at?.out != null}<span>· {t('maison.temperatures.dehors_alors', { v: deg(at.out) })}</span>{/if}
    </p>
  {/if}

  <div class="plot" bind:clientWidth={W}>
    {#if all.length}
      <svg
        bind:this={svg}
        viewBox="0 0 {W} {H}"
        height={H}
        role="img"
        aria-label={t('maison.temperatures.courbe', { name })}
        onpointermove={point}
        onpointerdown={point}
        onpointerleave={() => (hover = null)}>
        {#each scale.ticks as v (v)}
          <line x1={L} x2={W} y1={y(v)} y2={y(v)} class="rule" />
          <text x={L - 6} y={y(v) + 4} class="axis" text-anchor="end">{v}°</text>
        {/each}
        {#each xticks as tick (tick.ts)}
          <text x={x(tick.ts)} y={H - 4} class="axis" text-anchor="middle">{tick.text}</text>
        {/each}
        {#if outdoor?.length}<path d={path(outdoor)} class="out" />{/if}
        {#if main}<path d="{path(main.line)}L{x(main.line.at(-1)[0])},{y(scale.min)}L{x(main.line[0][0])},{y(scale.min)}Z" class="wash" fill={main.color} />{/if}
        {#each lines as l (l.id)}
          {#if l.line.length}<path d={path(l.line)} class="line" stroke={l.color} />{/if}
        {/each}
        {#if at}
          <line x1={x(hover)} x2={x(hover)} y1={T} y2={H - B} class="cross" />
          {#each at.values as l (l.id)}
            {#if l.v != null}<circle cx={x(hover)} cy={y(l.v)} r="4" fill={l.color} class="dot" />{/if}
          {/each}
        {/if}
      </svg>
    {:else}
      <p class="muted empty">{t('maison.temperatures.pas_de_mesure')}</p>
    {/if}
  </div>

  {#if lines.length > 1 || outdoor}
    <ul class="legend">
      {#each lines as l, i (l.id)}
        <li><i style:background={l.color}></i>{l.name}{#if at}<b class="num">{deg(at.values[i].v)}</b>{/if}</li>
      {/each}
      {#if outdoor}<li class="out-key"><i></i>{t('maison.temperatures.dehors')}{#if at}<b class="num">{deg(at.out)}</b>{/if}</li>{/if}
    </ul>
  {:else if main}
    <p class="by muted">{t('maison.temperatures.mesure_par', { name: main.name })}</p>
  {/if}
</section>

<style>
  .room {
    display: grid;
    align-content: start;
    gap: 6px;
  }

  header {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 10px;
  }

  h2 {
    margin: 0;
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 16px;
    font-weight: 700;
    min-width: 0;
  }

  .figure {
    display: flex;
    align-items: baseline;
    gap: 8px;
    flex: none;
  }

  .figure strong {
    font-size: 24px;
    font-weight: 650;
    letter-spacing: -0.02em;
  }

  .when {
    font-size: 12.5px;
    font-weight: 650;
    color: var(--ink-3);
  }

  .range {
    margin: 0;
    font-size: 12.5px;
    font-weight: 600;
  }

  .plot {
    width: 100%;
    min-width: 0;
    overflow: hidden;
  }

  svg {
    display: block;
    width: 100%;
    touch-action: pan-y;
  }

  .rule {
    stroke: var(--line);
    stroke-width: 1;
  }

  .axis {
    font-size: 10.5px;
    font-weight: 600;
    fill: var(--ink-3);
  }

  .line {
    fill: none;
    stroke-width: 2;
    stroke-linejoin: round;
    stroke-linecap: round;
  }

  .wash {
    opacity: 0.1;
  }

  .out {
    fill: none;
    stroke: var(--ink-3);
    stroke-width: 1.5;
    stroke-dasharray: 4 4;
  }

  .cross {
    stroke: var(--ink-3);
    stroke-width: 1;
  }

  .dot {
    stroke: var(--surface);
    stroke-width: 2;
  }

  .empty {
    margin: 30px 0;
    text-align: center;
    font-size: 13px;
  }

  .legend {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-wrap: wrap;
    gap: 4px 14px;
    font-size: 12.5px;
    font-weight: 600;
    color: var(--ink-2);
  }

  .legend li {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }

  .legend i {
    width: 12px;
    height: 3px;
    border-radius: 2px;
  }

  .out-key i {
    background: repeating-linear-gradient(90deg, var(--ink-3) 0 4px, transparent 4px 7px);
  }

  .legend b {
    color: var(--ink);
  }

  .by {
    margin: 0;
    font-size: 12px;
  }
</style>
