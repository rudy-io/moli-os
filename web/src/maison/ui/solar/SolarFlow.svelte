<script>
  import { ico } from './solar-icons.js';
  import { kw, pct } from './format.js';
  import { t } from '../../../lib/i18n.svelte.js';

  /** The energy right now, as it would flow with panels: the sun on top,
   *  the grid on the left, the house on the right, the battery below
   *  (when there is one). Dots run along the links that carry something,
   *  faster the more they carry. `flow`: from `flowNow`; `battery`:
   *  `{ cap, soc }` or null; `shifted`: kW Moli moved into this hour. */
  let { flow, battery = null, shifted = 0 } = $props();

  const withBattery = $derived(battery?.cap > 0);
  const H = $derived(withBattery ? 300 : 222);

  // Node centres (viewBox 320 wide) and the points where links meet them.
  const SUN = { x: 160, y: 46 };
  const GRID = { x: 50, y: 150 };
  const HOUSE = { x: 270, y: 150 };
  const BATT = { x: 160, y: 256 };

  const links = $derived(
    [
      { id: 'sun-house', d: 'M174 72 C184 118 206 136 242 136', kw: flow.toHouse, tone: 'pv', label: t('energie.solaire.flux.soleil_maison') },
      { id: 'sun-grid', d: 'M146 72 C136 118 114 136 78 136', kw: flow.toGrid, tone: 'pv', label: t('energie.solaire.flux.soleil_reseau') },
      { id: 'grid-house', d: 'M80 150 L240 150', kw: flow.fromGrid, tone: 'grid', label: t('energie.solaire.flux.reseau_maison') },
      ...(withBattery
        ? [
            { id: 'sun-batt', d: 'M160 76 L160 226', kw: flow.toBattery, tone: 'pv', label: t('energie.solaire.flux.soleil_batterie') },
            { id: 'batt-house', d: 'M174 230 C188 196 210 168 242 164', kw: flow.fromBattery, tone: 'batt', label: t('energie.solaire.flux.batterie_maison') },
          ]
        : []),
    ].map((l) => ({ ...l, active: l.kw > 0.02, speed: Math.max(0.55, Math.min(2.6, 2.8 - l.kw * 0.45)) })),
  );

  const night = $derived(flow.pv < 0.02);
  const describe = $derived(
    [
      flow.pv >= 0.02 ? t('energie.solaire.flux.decrit_pv', { kw: kw(flow.pv) }) : t('energie.solaire.flux.decrit_aucun'),
      t('energie.solaire.flux.decrit_maison', { kw: kw(flow.load) }),
      flow.toGrid > 0.02 ? t('energie.solaire.flux.decrit_vers_reseau', { kw: kw(flow.toGrid) }) : null,
      flow.fromGrid > 0.02 ? t('energie.solaire.flux.decrit_du_reseau', { kw: kw(flow.fromGrid) }) : null,
      flow.toBattery > 0.02 ? t('energie.solaire.flux.decrit_vers_batterie', { kw: kw(flow.toBattery) }) : null,
      flow.fromBattery > 0.02 ? t('energie.solaire.flux.decrit_de_batterie', { kw: kw(flow.fromBattery) }) : null,
    ]
      .filter(Boolean)
      .join(', ') + '.',
  );
</script>

<figure class="flow">
  <svg viewBox="0 0 320 {H}" role="img" aria-label={describe}>
    {#each links as l (l.id)}
      <path d={l.d} class="track" />
      {#if l.active}
        <path d={l.d} class="dots {l.tone}" style:--speed="{l.speed}s" />
      {/if}
    {/each}

    <g class="node pv" class:idle={night}>
      <circle cx={SUN.x} cy={SUN.y} r="28" />
      <path d={ico('solar')} transform="translate({SUN.x - 13.2} {SUN.y - 13.2}) scale(1.1)" />
    </g>
    <text x={SUN.x + 40} y={SUN.y - 6} class="name">{t('energie.solaire.flux.soleil')}</text>
    <text x={SUN.x + 40} y={SUN.y + 14} class="value">{night ? t('energie.solaire.flux.repos') : kw(flow.pv)}</text>

    <g class="node grid" class:idle={flow.toGrid < 0.02 && flow.fromGrid < 0.02}>
      <circle cx={GRID.x} cy={GRID.y} r="28" />
      <path d={ico('grid')} transform="translate({GRID.x - 13.2} {GRID.y - 13.2}) scale(1.1)" />
    </g>
    <text x={GRID.x} y={GRID.y + 48} class="name" text-anchor="middle">
      {flow.toGrid > 0.02 ? t('energie.solaire.flux.vers_reseau') : flow.fromGrid > 0.02 ? t('energie.solaire.flux.du_reseau') : t('energie.solaire.flux.reseau')}
    </text>
    <text x={GRID.x} y={GRID.y + 67} class="value" text-anchor="middle">{kw(flow.toGrid > 0.02 ? flow.toGrid : flow.fromGrid)}</text>

    <g class="node house">
      <circle cx={HOUSE.x} cy={HOUSE.y} r="28" />
      <path d={ico('house')} transform="translate({HOUSE.x - 13.2} {HOUSE.y - 13.2}) scale(1.1)" />
    </g>
    <text x={HOUSE.x} y={HOUSE.y + 48} class="name" text-anchor="middle">{t('energie.solaire.flux.maison')}</text>
    <text x={HOUSE.x} y={HOUSE.y + 67} class="value" text-anchor="middle">{kw(flow.load)}</text>

    {#if withBattery}
      <g class="node batt" class:idle={flow.toBattery < 0.02 && flow.fromBattery < 0.02}>
        <circle cx={BATT.x} cy={BATT.y} r="26" />
        <!-- The charge, as a ring around the battery. -->
        <circle cx={BATT.x} cy={BATT.y} r="26" class="charge" pathLength="100" stroke-dasharray="{(battery.soc / battery.cap) * 100} 100" transform="rotate(-90 {BATT.x} {BATT.y})" />
        <path d={ico('battery')} transform="translate({BATT.x - 12} {BATT.y - 12})" />
      </g>
      <text x={BATT.x + 38} y={BATT.y - 4} class="name">{t('energie.solaire.flux.batterie')}</text>
      <text x={BATT.x + 38} y={BATT.y + 15} class="value">{pct(battery.soc / battery.cap)}</text>
    {/if}
  </svg>
  <figcaption>
    <span class="key"><i class="pv"></i>{t('energie.solaire.flux.legende_soleil')}</span>
    <span class="key"><i class="grid"></i>{t('energie.solaire.flux.legende_reseau')}</span>
    {#if withBattery}<span class="key"><i class="batt"></i>{t('energie.solaire.flux.legende_batterie')}</span>{/if}
    {#if shifted > 0.05}<span class="moved">{t('energie.solaire.flux.lances', { kw: kw(shifted) })}</span>{/if}
  </figcaption>
</figure>

<style>
  .flow {
    margin: 0;
    display: grid;
    gap: 6px;
    justify-items: center;
  }

  svg {
    width: 100%;
    max-width: 420px;
    display: block;
    overflow: visible;
  }

  .track {
    fill: none;
    stroke: var(--line);
    stroke-width: 2;
  }

  /* Energy on the move: round dots sliding along the link. */
  .dots {
    fill: none;
    stroke-width: 5;
    stroke-linecap: round;
    stroke-dasharray: 0.1 13;
    animation: run var(--speed, 1.5s) linear infinite;
  }

  .dots.pv {
    stroke: var(--pv);
  }

  .dots.grid {
    stroke: var(--grid);
  }

  .dots.batt {
    stroke: var(--self);
  }

  @keyframes run {
    from {
      stroke-dashoffset: 13.1;
    }
    to {
      stroke-dashoffset: 0;
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .dots {
      animation: none;
      stroke-dasharray: none;
      stroke-width: 3;
    }
  }

  .node circle {
    fill: var(--surface);
    stroke-width: 3;
    transition: opacity 0.3s var(--ease);
  }

  .node path {
    fill: var(--ink-2);
  }

  .node.pv circle {
    stroke: var(--pv);
    fill: color-mix(in srgb, var(--pv) 12%, var(--surface));
  }

  .node.pv path {
    fill: var(--pv-ink);
  }

  .node.grid circle {
    stroke: var(--grid);
  }

  .node.house circle {
    stroke: var(--ink-2);
  }

  .node.batt circle {
    stroke: var(--line);
  }

  .node.batt circle.charge {
    fill: none;
    stroke: var(--self);
    stroke-linecap: round;
  }

  .node.idle circle:not(.charge) {
    stroke: var(--line);
  }

  .node.pv.idle circle {
    fill: var(--surface);
  }

  .node.pv.idle path {
    fill: var(--ink-3);
  }

  .name {
    font-size: 11px;
    font-weight: 700;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    fill: var(--ink-3);
  }

  .value {
    font-size: 17px;
    font-weight: 650;
    fill: var(--ink);
  }

  figcaption {
    display: flex;
    flex-wrap: wrap;
    justify-content: center;
    gap: 4px 14px;
    font-size: 12px;
    color: var(--ink-3);
  }

  .key {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }

  .key i {
    width: 10px;
    height: 10px;
    border-radius: 50%;
  }

  .key i.pv {
    background: var(--pv);
  }

  .key i.grid {
    background: var(--grid);
  }

  .key i.batt {
    background: var(--self);
  }

  .moved {
    color: var(--ink-2);
    font-weight: 600;
  }
</style>
