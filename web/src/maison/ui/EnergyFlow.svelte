<script>
  import { onMount } from 'svelte';
  import { hub } from '../lib/home.svelte.js';
  import { energy, useEnergy, kwh, watts } from '../lib/energy-live.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';
  import { icon } from '../lib/icons.js';
  import { batteryNow, splitFlows } from '../lib/battery.js';

  /** Where the energy goes right now: the grid (bought, sent back), the
   *  panels, the house, and gas today. From the meters' live powers
   *  (`power` of the grid, export and solar meters): the house is what was
   *  bought plus what the panels made, less what went out. */
  /** On the home page (`home`), only when there is more than the grid:
   *  without panels nor export, the energy card above says it all. */
  let { home = false } = $props();

  onMount(useEnergy);

  const s = $derived(energy.summary);
  const current = (point) => {
    if (!point) return null;
    const i = point.indexOf('/');
    const v = hub.devices[point.slice(0, i)]?.state?.[point.slice(i + 1)]?.value;
    return typeof v === 'number' && Number.isFinite(v) ? v : null;
  };
  // Several meters can share one power point (a P1's day and night
  // indexes): each point once. Signed: a single clamp on the supply reads
  // below zero when the house sends power out.
  const powerOf = (role) => {
    const points = [...new Set((s?.meters ?? []).filter((m) => m.role === role && m.power).map((m) => m.power))];
    if (!points.length) return null;
    const values = points.map(current).filter((v) => v != null);
    return values.length ? values.reduce((a, v) => a + v, 0) : null;
  };
  const positive = (v) => (v == null ? null : Math.max(0, v));
  const kwhOf = (role, period) => {
    const ids = (s?.meters ?? []).filter((m) => m.role === role).map((m) => m.id);
    if (!ids.length || !period) return null;
    return ids.reduce((a, id) => a + (period.meters[id]?.kwh ?? 0), 0);
  };

  const hasExport = $derived((s?.meters ?? []).some((m) => m.role === 'export'));
  const gridNow = $derived(powerOf('grid') ?? powerOf('total'));
  const bought = $derived(positive(gridNow));
  // Without an export meter, what the grid meter reads below zero went out.
  const sent = $derived(hasExport ? (positive(powerOf('export')) ?? 0) : gridNow == null ? 0 : Math.max(0, -gridNow));
  const made = $derived(positive(powerOf('solar')));
  const hasSolar = $derived((s?.meters ?? []).some((m) => m.role === 'solar'));
  const shown = $derived(!home || hasSolar || hasExport);
  const hasGas = $derived((s?.meters ?? []).some((m) => m.role === 'gas'));
  // The home battery, positive while it charges: it takes from the house's
  // share, and gives back to it while it discharges.
  const battery = $derived(batteryNow(s, current));
  const batteryW = $derived(battery?.power ?? null);
  const hasBattery = $derived(batteryW != null);
  const house = $derived(bought == null ? null : Math.max(0, bought + (made ?? 0) - sent - (batteryW ?? 0)));
  // Who feeds whom: the battery charges from the sun first, then the grid;
  // the sun then feeds the house, the rest goes out (`splitFlows`).
  const split = $derived(splitFlows({ bought, made, sent, batteryW }));
  const toGrid = $derived(split.solarToGrid);
  const toHouse = $derived(split.solarToHouse);

  const today = $derived(s?.today);
  const boughtToday = $derived(kwhOf('grid', today) ?? kwhOf('total', today));
  const sentToday = $derived(kwhOf('export', today));
  const madeToday = $derived(kwhOf('solar', today));
  const gasToday = $derived(today?.gas?.kwh ?? null);

  // A dot's trip: faster with more power, none below a trickle.
  const duration = (w) => `${Math.min(6, Math.max(0.7, 2500 / Math.max(w, 1))).toFixed(2)}s`;
  const flowing = (w) => w != null && w >= 5;
  // A line that carries power stands out, thicker with more; the others stay faint.
  const weight = (w) => (flowing(w) ? Math.min(6, 2.5 + w / 900) : 1.5);
  // The grid says one thing: what is bought, or what goes out.
  const gridSends = $derived(sent >= 5 && (bought ?? 0) < 5);
  // The battery's charge as a ring just outside its disc (r = 34.5).
  const RING = 2 * Math.PI * 34.5;

  // A diamond: the sun on top, the battery below, the grid and the house on
  // each side; the curves are quarter arcs, the same on every side.
  const PATHS = {
    solarHouse: 'M 240 92 C 240 131 278 162 324 162',
    solarGrid: 'M 240 92 C 240 131 202 162 156 162',
    gridHouse: 'M 156 170 L 324 170',
    gasHouse: 'M 360 92 L 360 134',
    solarBattery: 'M 240 92 L 240 248',
    gridBattery: 'M 156 178 C 202 178 240 209 240 248',
    batteryGrid: 'M 240 248 C 240 209 202 178 156 178',
    batteryHouse: 'M 240 248 C 240 209 278 178 324 178',
  };
</script>

{#if s && bought != null && shown}
  <section class="card flow" class:home aria-label={t('energie.flux.titre')}>
    <div class="card-head"><h2><Icon name="flash" size={18} />{t('energie.flux.titre')}</h2></div>
    <svg viewBox="0 0 480 {hasBattery ? 344 : 236}" role="img" aria-label={t('energie.flux.resume', { maison: watts(house), reseau: watts(bought), solaire: watts(made ?? 0) })}>
      {#if hasSolar}
        <path class="line solar" class:on={flowing(toHouse)} stroke-width={weight(toHouse)} d={PATHS.solarHouse} />
        <path class="line export" class:on={flowing(toGrid)} stroke-width={weight(toGrid)} d={PATHS.solarGrid} />
      {/if}
      <path class="line grid" class:on={flowing(split.gridToHouse)} stroke-width={weight(split.gridToHouse)} d={PATHS.gridHouse} />
      {#if hasGas}<path class="line gas" d={PATHS.gasHouse} />{/if}
      {#if hasBattery}
        {#if hasSolar}<path class="line solar" class:on={flowing(split.solarToBattery)} stroke-width={weight(split.solarToBattery)} d={PATHS.solarBattery} />{/if}
        <path class="line grid" class:on={flowing(split.gridToBattery) || flowing(split.batteryToGrid)} stroke-width={weight(Math.max(split.gridToBattery ?? 0, split.batteryToGrid ?? 0))} d={PATHS.gridBattery} />
        <path class="line battery" class:on={flowing(split.batteryToHouse)} stroke-width={weight(split.batteryToHouse)} d={PATHS.batteryHouse} />
      {/if}

      {#if hasSolar && flowing(toHouse)}
        {#key duration(toHouse)}<circle class="dot solar" r="4"><animateMotion dur={duration(toHouse)} repeatCount="indefinite" path={PATHS.solarHouse} /></circle>{/key}
      {/if}
      {#if hasSolar && flowing(toGrid)}
        {#key duration(toGrid)}<circle class="dot export" r="4"><animateMotion dur={duration(toGrid)} repeatCount="indefinite" path={PATHS.solarGrid} /></circle>{/key}
      {/if}
      {#if hasBattery}
        {#each [['solar', split.solarToBattery, PATHS.solarBattery], ['grid', split.gridToBattery, PATHS.gridBattery], ['battery', split.batteryToHouse, PATHS.batteryHouse], ['battery', split.batteryToGrid, PATHS.batteryGrid]] as [kind, w, path] (path)}
          {#if flowing(w)}
            {#key duration(w)}<circle class="dot {kind}" r="4"><animateMotion dur={duration(w)} repeatCount="indefinite" {path} /></circle>{/key}
          {/if}
        {/each}
      {/if}
      {#if flowing(split.gridToHouse)}
        {#key duration(split.gridToHouse)}<circle class="dot grid" r="4"><animateMotion dur={duration(split.gridToHouse)} repeatCount="indefinite" path={PATHS.gridHouse} /></circle>{/key}
      {/if}

      <!-- Every node alike: an icon, the value, a word under it; its name outside. -->
      {#if hasSolar}
        <g class="node solar" transform="translate(240 56)">
          <circle class="bg" r="34.75" />
          <path class="ico" d={icon('sun')} transform="translate(-8 -27) scale(0.68)" />
          <text class="big" y="8">{watts(made ?? 0)}</text>
          <text class="small" y="22">{kwh(madeToday)} kWh</text>
          <text class="label" y="-46">{t('energie.flux.solaire')}</text>
        </g>
      {/if}
      {#if hasGas}
        <g class="node gas" transform="translate(360 56)">
          <circle class="bg" r="34.75" />
          <path class="ico" d={icon('fire')} transform="translate(-8 -27) scale(0.68)" />
          <text class="big" y="8">{kwh(gasToday)}</text>
          <text class="small" y="22">kWh</text>
          <text class="label" y="-46">{t('energie.flux.gaz')}</text>
        </g>
      {/if}
      <g class="node grid" class:sends={gridSends} transform="translate(120 170)">
        <circle class="bg" r="34.75" />
        <path class="ico" d={icon('transmission-tower')} transform="translate(-8 -27) scale(0.68)" />
        <text class="big" y="8">{watts(gridSends ? sent : bought)}</text>
        <text class="small way" y="22">{gridSends ? t('energie.flux.injection') : t('energie.flux.achat')}</text>
        <text class="label side left" x="-46" y="0">{t('energie.flux.reseau')}</text>
      </g>
      <g class="node house" transform="translate(360 170)">
        <circle class="bg" r="34.75" />
        <path class="ico" d={icon('home')} transform="translate(-8 -27) scale(0.68)" />
        <text class="big" y="8">{watts(house)}</text>
        <text class="small" y="22">{t('energie.flux.conso')}</text>
        <text class="label side right" x="46" y="0">{t('energie.flux.maison')}</text>
      </g>
      {#if hasBattery}
        <g class="node battery" transform="translate(240 284)">
          <circle class="bg" r="33" />
          <circle class="track" r="34.5" />
          {#if battery.soc != null}
            <circle class="ring" r="34.5" stroke-dasharray="{(RING * Math.max(0, Math.min(100, battery.soc))) / 100} {RING}" transform="rotate(-90)" />
          {/if}
          <path class="ico" d={icon('battery')} transform="translate(-8 -27) scale(0.68)" />
          <text class="big" y="8">{battery.soc == null ? watts(Math.abs(batteryW)) : `${Math.round(battery.soc)} %`}</text>
          <text class="small" y="22">{batteryW > 0 ? '↓ ' : batteryW < 0 ? '↑ ' : ''}{watts(Math.abs(batteryW))}</text>
          <text class="label" y="56">{t('energie.flux.batterie')}</text>
        </g>
      {/if}
    </svg>
    <div class="today" aria-label={t('energie.flux.aujourdhui', { achete: kwh(boughtToday), injecte: kwh(sentToday) })}>
      <span class="muted">{t('energie.flux.aujourdhui_court')}</span>
      {#if hasSolar}<span class="pill solar"><b>{kwh(madeToday)} kWh</b> {t('energie.flux.produits')}</span>{/if}
      <span class="pill grid"><b>{kwh(boughtToday)} kWh</b> {t('energie.flux.achetes')}</span>
      {#if hasExport || sentToday}<span class="pill export"><b>{kwh(sentToday)} kWh</b> {t('energie.flux.injectes')}</span>{/if}
    </div>
  </section>
{/if}

<style>
  /* On the home page, spaced like the other cards of its column. */
  .flow.home {
    margin-top: 20px;
  }

  .flow svg {
    width: 100%;
    height: auto;
    max-width: 480px;
    display: block;
    margin: 0 auto;
    font-variant-numeric: tabular-nums;
  }

  .line {
    fill: none;
    stroke-width: 1.5;
    stroke-linecap: round;
    stroke-dasharray: 2 5;
    opacity: 0.28;
    transition: stroke-width 0.4s, opacity 0.4s;
  }

  .line.on {
    stroke-dasharray: none;
    opacity: 0.75;
  }

  .line.solar {
    stroke: var(--warm);
  }

  .line.export,
  .line.battery {
    stroke: var(--good);
  }

  .line.grid {
    stroke: var(--cool);
  }

  .line.gas {
    stroke: var(--alert);
  }

  .dot.solar {
    fill: var(--warm);
  }

  .dot.export,
  .dot.battery {
    fill: var(--good);
  }

  .dot.grid {
    fill: var(--cool);
  }

  .dot {
    stroke: var(--surface);
    stroke-width: 1.5;
  }

  .node .bg {
    stroke-width: 2.5;
  }

  .node.solar .bg {
    fill: var(--warm-soft);
    stroke: var(--warm);
  }

  .node.grid .bg {
    fill: var(--cool-soft);
    stroke: var(--cool);
  }

  .node.grid.sends .bg {
    fill: var(--good-soft);
    stroke: var(--good);
  }

  .node.house .bg {
    fill: var(--surface-2);
    stroke: var(--ink);
  }

  .node.gas .bg {
    fill: var(--alert-soft);
    stroke: var(--alert);
  }

  .node.battery .bg {
    fill: var(--good-soft);
    stroke: none;
  }

  .node .track,
  .node .ring {
    fill: none;
    stroke-width: 3;
  }

  .node .track {
    stroke: var(--good-soft);
    filter: brightness(0.92);
  }

  .node .ring {
    stroke: var(--good);
    stroke-linecap: round;
  }

  .ico {
    fill: var(--ink);
  }

  .node.solar .ico {
    fill: var(--warm-ink);
  }

  .node.grid .ico,
  .node.grid .way {
    fill: var(--cool);
  }

  .node.grid.sends .ico,
  .node.grid.sends .way,
  .node.battery .ico {
    fill: var(--good);
  }

  .node.gas .ico {
    fill: var(--alert);
  }

  text {
    text-anchor: middle;
    fill: var(--ink);
    font-size: 12px;
  }

  text.big {
    font-size: 15px;
    font-weight: 700;
  }

  text.small {
    fill: var(--ink-2);
    font-size: 11px;
    font-weight: 600;
  }

  text.label {
    fill: var(--ink-2);
    font-size: 12px;
    font-weight: 600;
  }

  text.label.side {
    dominant-baseline: central;
  }

  text.label.left {
    text-anchor: end;
  }

  text.label.right {
    text-anchor: start;
  }

  .today {
    display: flex;
    flex-wrap: wrap;
    justify-content: center;
    align-items: center;
    gap: 6px 8px;
    margin-top: 4px;
    padding-top: 12px;
    border-top: 1px solid var(--line);
    font-size: 13px;
  }

  .pill {
    padding: 6px 12px;
    border-radius: 999px;
    font-weight: 600;
  }

  .pill b {
    font-weight: 800;
  }

  .pill.solar {
    background: var(--warm-soft);
    color: var(--warm-ink);
  }

  .pill.grid {
    background: var(--cool-soft);
    color: var(--cool);
  }

  .pill.export {
    background: var(--good-soft);
    color: var(--good);
  }
</style>
