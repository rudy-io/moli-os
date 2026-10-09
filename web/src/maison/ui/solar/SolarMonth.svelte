<script>
  import { kwhs, pct, dayName } from './format.js';
  import { t } from '../../../lib/i18n.svelte.js';

  /** Day by day: above the line, what the house consumed (green: from the
   *  sun, blue: bought); below, the surplus sent to the grid. Touch a day
   *  to see it hour by hour. `days`: from `byDay`. */
  let { days = [], selected = null, timezone = null, onselect } = $props();

  const H = 220;
  const TOP = 10;
  const BOTTOM = 22;
  const GAP = 2;
  let W = $state(640);

  const bars = $derived(
    days.map((d) => ({
      day: d.day,
      start: d.start,
      sun: d.direct + d.battOut,
      buy: d.import,
      out: d.export,
      pv: d.pv,
      load: d.load,
    })),
  );
  const up = $derived(Math.max(0.5, ...bars.map((b) => b.sun + b.buy)));
  const down = $derived(Math.max(0, ...bars.map((b) => b.out)));
  // One scale for both sides: the baseline sits where the two maxima meet.
  const scale = $derived((H - TOP - BOTTOM) / (up + down || 1));
  const base = $derived(TOP + up * scale);
  const slot = $derived(bars.length ? W / bars.length : 0);
  const bw = $derived(Math.max(3, Math.min(18, slot * 0.62)));

  /** A bar segment rounded on one end only (the data end). */
  function seg(x, y0, y1, w, roundTop, roundBottom) {
    const h = Math.abs(y1 - y0);
    if (h < 0.5) return '';
    const top = Math.min(y0, y1);
    const r = Math.min(4, w / 2, h);
    const rt = roundTop ? r : 0;
    const rb = roundBottom ? r : 0;
    return `M${x},${top + rt} q0,${-rt} ${rt},${-rt} h${w - 2 * rt} q${rt},0 ${rt},${rt} v${h - rt - rb} q0,${rb} ${-rb},${rb} h${-(w - 2 * rb)} q${-rb},0 ${-rb},${-rb} Z`;
  }

  let hover = $state(null);
  const shown = $derived(bars.find((b) => b.day === (hover ?? selected)) ?? null);
  const label = (b, i) => (i === bars.length - 1 ? t('energie.solaire.mois.auj') : (bars.length - 1 - i) % 7 === 0 ? dayName(b.start, timezone, true).replace(/^\S+\s/, '') : '');
</script>

<div class="readout" aria-live="polite">
  {#if shown}
    <span class="when">{dayName(shown.start, timezone)}</span>
    <span class="fig"><i class="sun"></i>{t('energie.solaire.courbe.soleil')} <b class="num">{kwhs(shown.sun)}</b></span>
    <span class="fig"><i class="buy"></i>{t('energie.solaire.courbe.achete')} <b class="num">{kwhs(shown.buy)}</b></span>
    <span class="fig"><i class="out"></i>{t('energie.solaire.courbe.surplus')} <b class="num">{kwhs(shown.out)}</b></span>
    {#if shown.load > 0.05}<span class="chip good">{t('energie.solaire.mois.besoins', { pct: pct(shown.sun / shown.load) })}</span>{/if}
  {/if}
</div>

<div class="plot" bind:clientWidth={W}>
  <svg viewBox="0 0 {W} {H}" height={H} role="img" aria-label={t('energie.solaire.mois.aria')} onpointerleave={() => (hover = null)}>
    <line x1="0" x2={W} y1={base} y2={base} class="base" />
    {#each bars as b, i (b.day)}
      {@const x = i * slot + (slot - bw) / 2}
      {@const ySun = base - b.sun * scale}
      {@const yBuy = ySun - b.buy * scale}
      <g class:dim={hover != null && hover !== b.day}>
        {#if selected === b.day && slot > 2}<rect x={i * slot + 1} y={TOP - 4} width={slot - 2} height={H - TOP - BOTTOM + 8} rx="6" class="pick" />{/if}
        <path d={seg(x, base, ySun, bw, b.buy * scale < 0.5, false)} class="sun" />
        <path d={seg(x, ySun - (b.sun * scale >= 0.5 ? GAP : 0), yBuy, bw, true, false)} class="buy" />
        <path d={seg(x, base + GAP, base + GAP + b.out * scale, bw, false, true)} class="out" />
        <rect
          x={i * slot}
          y="0"
          width={slot}
          height={H}
          class="hit"
          role="button"
          tabindex="-1"
          aria-label={dayName(b.start, timezone)}
          onpointerenter={() => (hover = b.day)}
          onclick={() => onselect?.(b.day)}
          onkeydown={(e) => e.key === 'Enter' && onselect?.(b.day)}
        />
      </g>
      {@const cx = i * slot + slot / 2}
      <text x={cx < 26 ? 0 : cx > W - 26 ? W : cx} y={H - 5} class="axis" text-anchor={cx < 26 ? 'start' : cx > W - 26 ? 'end' : 'middle'}>{label(b, i)}</text>
    {/each}
    <text x="2" y={TOP + 10} class="axis halo">{kwhs(up)}</text>
    {#if down > 0.5}<text x="2" y={H - BOTTOM - 4} class="axis halo">{t('energie.solaire.mois.de_surplus', { kwh: kwhs(down) })}</text>{/if}
  </svg>
</div>

<ul class="legend">
  <li><i class="sw sun"></i>{t('energie.solaire.mois.legende_soleil')}</li>
  <li><i class="sw buy"></i>{t('energie.solaire.mois.legende_achete')}</li>
  <li><i class="sw out"></i>{t('energie.solaire.mois.legende_surplus')}</li>
</ul>

<style>
  .readout {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px 16px;
    min-height: 34px;
    margin-bottom: 8px;
    font-size: 13.5px;
  }

  .when {
    font-weight: 700;
    color: var(--ink-2);
  }

  .fig {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    color: var(--ink-2);
  }

  .fig b {
    color: var(--ink);
    font-weight: 650;
  }

  .fig i,
  .sw {
    width: 10px;
    height: 10px;
    border-radius: 3px;
  }

  .sun {
    fill: var(--self);
    background: var(--self);
  }

  .buy {
    fill: var(--grid);
    background: var(--grid);
  }

  .out {
    fill: var(--pv);
    background: var(--pv);
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

  .base {
    stroke: var(--ink-3);
    stroke-width: 1;
  }

  g {
    transition: opacity 0.2s;
  }

  g.dim {
    opacity: 0.45;
  }

  .pick {
    fill: var(--surface-2);
  }

  .hit {
    fill: transparent;
    cursor: pointer;
  }

  .hit:focus {
    outline: none;
  }

  .axis {
    font-size: 11px;
    font-weight: 600;
    fill: var(--ink-3);
  }

  .halo {
    paint-order: stroke;
    stroke: var(--surface);
    stroke-width: 4px;
    stroke-linejoin: round;
  }

  .legend {
    list-style: none;
    margin: 12px 0 0;
    padding: 0;
    display: flex;
    flex-wrap: wrap;
    gap: 6px 16px;
    font-size: 12.5px;
    color: var(--ink-2);
  }

  .legend li {
    display: inline-flex;
    align-items: center;
    gap: 7px;
  }
</style>
