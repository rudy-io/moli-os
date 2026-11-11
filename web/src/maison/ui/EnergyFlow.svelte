<script>
  import { onMount } from 'svelte';
  import { hub } from '../lib/home.svelte.js';
  import { energy, useEnergy, kwh, watts } from '../lib/energy-live.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';

  /** Where the energy goes right now: the grid (bought, sent back), the
   *  panels, the house, and gas today. From the meters' live powers
   *  (`power` of the grid, export and solar meters): the house is what was
   *  bought plus what the panels made, less what went out. */
  onMount(useEnergy);

  const s = $derived(energy.summary);
  const current = (point) => {
    if (!point) return null;
    const i = point.indexOf('/');
    const v = hub.devices[point.slice(0, i)]?.state?.[point.slice(i + 1)]?.value;
    return typeof v === 'number' && Number.isFinite(v) ? v : null;
  };
  // Several meters can share one power point (a P1's day and night
  // indexes): each point once.
  const powerOf = (role) => {
    const points = [...new Set((s?.meters ?? []).filter((m) => m.role === role && m.power).map((m) => m.power))];
    if (!points.length) return null;
    const values = points.map(current).filter((v) => v != null);
    return values.length ? values.reduce((a, v) => a + Math.max(0, v), 0) : null;
  };
  const kwhOf = (role, period) => {
    const ids = (s?.meters ?? []).filter((m) => m.role === role).map((m) => m.id);
    if (!ids.length || !period) return null;
    return ids.reduce((a, id) => a + (period.meters[id]?.kwh ?? 0), 0);
  };

  const bought = $derived(powerOf('grid') ?? powerOf('total'));
  const sent = $derived(powerOf('export') ?? 0);
  const made = $derived(powerOf('solar'));
  const hasSolar = $derived((s?.meters ?? []).some((m) => m.role === 'solar'));
  const hasGas = $derived((s?.meters ?? []).some((m) => m.role === 'gas'));
  const house = $derived(bought == null ? null : Math.max(0, bought + (made ?? 0) - sent));
  // The panels feed the house first; the rest goes out.
  const toGrid = $derived(Math.min(sent, made ?? sent));
  const toHouse = $derived(made == null ? 0 : Math.max(0, made - toGrid));

  const today = $derived(s?.today);
  const boughtToday = $derived(kwhOf('grid', today) ?? kwhOf('total', today));
  const sentToday = $derived(kwhOf('export', today));
  const madeToday = $derived(kwhOf('solar', today));
  const gasToday = $derived(today?.gas?.kwh ?? null);

  // A dot's trip: faster with more power, none below a trickle.
  const duration = (w) => `${Math.min(6, Math.max(0.7, 2500 / Math.max(w, 1))).toFixed(2)}s`;
  const flowing = (w) => w != null && w >= 5;

  const PATHS = {
    solarHouse: 'M 200 92 C 200 170 290 172 302 172',
    solarGrid: 'M 200 92 C 200 170 110 172 98 172',
    gridHouse: 'M 98 180 L 302 180',
    gasHouse: 'M 340 92 L 340 136',
  };
</script>

{#if s && bought != null}
  <section class="card flow" aria-label={t('energie.flux.titre')}>
    <div class="card-head"><h2><Icon name="flash" size={18} />{t('energie.flux.titre')}</h2></div>
    <svg viewBox="0 0 400 236" role="img" aria-label={t('energie.flux.resume', { maison: watts(house), reseau: watts(bought), solaire: watts(made ?? 0) })}>
      {#if hasSolar}
        <path class="line solar" d={PATHS.solarHouse} />
        <path class="line export" d={PATHS.solarGrid} />
      {/if}
      <path class="line grid" d={PATHS.gridHouse} />
      {#if hasGas}<path class="line gas" d={PATHS.gasHouse} />{/if}

      {#if hasSolar && flowing(toHouse)}
        {#key duration(toHouse)}<circle class="dot solar" r="4"><animateMotion dur={duration(toHouse)} repeatCount="indefinite" path={PATHS.solarHouse} /></circle>{/key}
      {/if}
      {#if hasSolar && flowing(toGrid)}
        {#key duration(toGrid)}<circle class="dot export" r="4"><animateMotion dur={duration(toGrid)} repeatCount="indefinite" path={PATHS.solarGrid} /></circle>{/key}
      {/if}
      {#if flowing(bought)}
        {#key duration(bought)}<circle class="dot grid" r="4"><animateMotion dur={duration(bought)} repeatCount="indefinite" path={PATHS.gridHouse} /></circle>{/key}
      {/if}

      {#if hasSolar}
        <g class="node solar" transform="translate(200 56)">
          <circle r="36" />
          <text class="label" y="-46">{t('energie.flux.solaire')}</text>
          <text class="big" y="2">{watts(made ?? 0)}</text>
          <text class="small" y="18">{kwh(madeToday)} kWh</text>
        </g>
      {/if}
      {#if hasGas}
        <g class="node gas" transform="translate(340 56)">
          <circle r="36" />
          <text class="label" y="-46">{t('energie.flux.gaz')}</text>
          <text class="big" y="6">{kwh(gasToday)} kWh</text>
        </g>
      {/if}
      <g class="node grid" transform="translate(60 176)">
        <circle r="38" />
        <text class="in" y="-4">→ {watts(bought)}</text>
        <text class="out" y="13">← {watts(sent)}</text>
        <text class="label" y="56">{t('energie.flux.reseau')}</text>
      </g>
      <g class="node house" transform="translate(340 176)">
        <circle r="38" />
        <text class="big" y="2">{watts(house)}</text>
        <text class="label" y="56">{t('energie.flux.maison')}</text>
      </g>
    </svg>
    <p class="muted small">{t('energie.flux.aujourdhui', { achete: kwh(boughtToday), injecte: kwh(sentToday) })}</p>
  </section>
{/if}

<style>
  .flow svg {
    width: 100%;
    height: auto;
    max-width: 460px;
    display: block;
    margin: 0 auto;
  }

  .line {
    fill: none;
    stroke-width: 2;
    opacity: 0.55;
  }

  .line.solar,
  .dot.solar {
    stroke: var(--warm);
    fill: var(--warm);
  }

  .line.export,
  .dot.export {
    stroke: #9b7bf0;
    fill: #9b7bf0;
  }

  .line.grid,
  .dot.grid {
    stroke: var(--cool);
    fill: var(--cool);
  }

  .line.gas {
    stroke: #a33b3b;
  }

  .line.solar,
  .line.export,
  .line.grid,
  .line.gas {
    fill: none;
  }

  .node circle {
    fill: var(--surface);
    stroke-width: 3;
  }

  .node.solar circle {
    stroke: var(--warm);
  }

  .node.grid circle {
    stroke: var(--cool);
  }

  .node.house circle {
    stroke: var(--cool);
  }

  .node.gas circle {
    stroke: #a33b3b;
  }

  text {
    text-anchor: middle;
    fill: var(--ink);
    font-size: 12px;
  }

  text.big {
    font-size: 14px;
    font-weight: 600;
  }

  text.small,
  text.label {
    fill: var(--ink-3);
    font-size: 11px;
  }

  text.in {
    fill: var(--cool);
    font-size: 12px;
    font-weight: 600;
  }

  text.out {
    fill: #9b7bf0;
    font-size: 12px;
    font-weight: 600;
  }

  .small {
    margin: 4px 0 0;
    text-align: center;
  }
</style>
