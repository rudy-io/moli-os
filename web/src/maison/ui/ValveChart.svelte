<script>
  import { device, value, num } from '../lib/home.svelte.js';
  import { t, locale } from '../../lib/i18n.svelte.js';
  import { scheduleSpec } from '../lib/schedule.js';
  import { seriesPoints, seriesSteps, programmeSteps, extent, scaleOf, linePath, stepPath, valueTicks, timeTicks } from '../lib/valve-history.js';

  // What a valve measured and was asked, over the last hours: the temperature, the
  // setpoint and what the schedule asks, on one scale. Read-only, from `/api/history`.
  let { id, name } = $props();

  const RANGES = [
    { hours: 6, label: 'systeme.historique.plage_6h' },
    { hours: 24, label: 'systeme.historique.plage_24h' },
    { hours: 168, label: 'systeme.historique.plage_7j' },
  ];
  const REFRESH_MS = 5 * 60_000;
  const W = 320;
  const H = 150;
  const BOX = { left: 34, top: 8, width: W - 34 - 8, height: H - 8 - 22 };

  const d = $derived(device(id));
  const spec = $derived(scheduleSpec(d));
  const unit = $derived(d?.points.find((p) => p.key === 'occupied_heating_setpoint')?.unit ?? '');
  let hours = $state(24);
  let loaded = $state(null);
  let failed = $state(false);

  const read = (key, hrs, points) => fetch(`/api/history?point=${encodeURIComponent(`${id}/${key}`)}&hours=${hrs}&points=${points}`).then((r) => (r.ok ? r.json() : Promise.reject(new Error(String(r.status)))));

  // A refresh keeps the curve on screen; changing the window or the valve clears it.
  $effect(() => {
    const range = hours;
    let live = true;
    const load = () =>
      Promise.all([read('local_temperature', range, 300), read('occupied_heating_setpoint', range, 1000)])
        .then(([temp, setpoint]) => {
          if (!live) return;
          loaded = { temp, setpoint };
          failed = false;
        })
        .catch(() => live && (failed = true));
    loaded = null;
    failed = false;
    load();
    const timer = setInterval(load, REFRESH_MS);
    return () => {
      live = false;
      clearInterval(timer);
    };
  });

  const from = $derived(loaded?.temp.from ?? 0);
  const to = $derived(loaded?.temp.to ?? 0);
  const temp = $derived(seriesPoints(loaded?.temp));
  const setpoint = $derived(seriesSteps(loaded?.setpoint));
  const days = $derived(spec ? Object.fromEntries(spec.days.map((day) => [day, value(id, `weekly_schedule_${day}`)])) : {});
  const programme = $derived(programmeSteps(spec, days, from, to));
  const range = $derived(extent([temp, setpoint, programme]));
  const scale = $derived(scaleOf(from, to, range, BOX));
  const lastTemp = $derived(temp.at(-1)?.[1]);
  const lastSetpoint = $derived(setpoint.at(-1)?.[1]);

  const clock = (ts) => new Date(ts).toLocaleTimeString(locale(), { hour: '2-digit', minute: '2-digit' });
  const day = (ts) => new Date(ts).toLocaleDateString(locale(), { weekday: 'short' }).replace('.', '');
  const tickLabel = (ts) => (hours > 24 ? day(ts) : clock(ts));
  const degrees = (v) => `${num(v, v % 1 ? 1 : 0)}${unit}`;
</script>

<div class="chart">
  <div class="ranges" role="group" aria-label={t('commun.vanne.courbe.periode')}>
    {#each RANGES as r (r.hours)}
      <button class:active={hours === r.hours} aria-pressed={hours === r.hours} onclick={() => (hours = r.hours)}>{t(r.label)}</button>
    {/each}
  </div>
  {#if failed && !loaded}
    <p class="note">{t('commun.vanne.courbe.erreur')}</p>
  {:else if loaded && !scale}
    <p class="note">{t('commun.vanne.courbe.vide')}</p>
  {:else if loaded}
    <svg viewBox="0 0 {W} {H}" role="img" aria-label={t('commun.vanne.courbe.resume', { name, range: t(RANGES.find((r) => r.hours === hours).label) })}>
      {#each valueTicks(range) as v (v)}
        <line class="grid" x1={BOX.left} x2={BOX.left + BOX.width} y1={scale.y(v)} y2={scale.y(v)} />
        <text class="axis" x={BOX.left - 5} y={scale.y(v)} text-anchor="end" dominant-baseline="middle">{num(v, v % 1 ? 1 : 0)}</text>
      {/each}
      {#each timeTicks(from, to, hours) as ts (ts)}
        <text class="axis" x={scale.x(ts)} y={H - 6} text-anchor="middle">{tickLabel(ts)}</text>
      {/each}
      {#if programme.length}<path class="programme" d={stepPath(programme, to, scale)} />{/if}
      {#if setpoint.length}<path class="setpoint" d={stepPath(setpoint, to, scale)} />{/if}
      {#if temp.length}<path class="temp" d={linePath(temp, scale)} />{/if}
    </svg>
    <ul class="legend">
      <li><i class="temp"></i>{t('commun.clim.temp_piece')}{#if Number.isFinite(lastTemp)} <b class="num">{degrees(lastTemp)}</b>{/if}</li>
      <li><i class="setpoint"></i>{t('commun.clim.consigne')}{#if Number.isFinite(lastSetpoint)} <b class="num">{degrees(lastSetpoint)}</b>{/if}</li>
      {#if programme.length}<li><i class="programme"></i>{t('commun.vanne.courbe.programme')}</li>{/if}
    </ul>
  {/if}
</div>

<style>
  .chart {
    display: grid;
    gap: 8px;
  }

  .ranges {
    display: flex;
    gap: 6px;
  }

  .ranges button {
    border: none;
    background: var(--surface-2);
    color: var(--ink);
    font: inherit;
    font-size: 13px;
    padding: 4px 12px;
    border-radius: 999px;
    cursor: pointer;
  }

  .ranges button.active {
    background: var(--ink);
    color: var(--surface);
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
    fill: var(--ink-3);
    font-size: 9px;
  }

  path {
    fill: none;
    stroke-linejoin: round;
    vector-effect: non-scaling-stroke;
  }

  path.temp {
    stroke: var(--warm);
    stroke-width: 2;
  }

  path.setpoint {
    stroke: var(--cool);
    stroke-width: 2;
  }

  path.programme {
    stroke: var(--ink-3);
    stroke-width: 1.5;
    stroke-dasharray: 4 3;
  }

  .legend {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 14px;
    margin: 0;
    padding: 0;
    list-style: none;
    font-size: 12.5px;
    color: var(--ink-2);
  }

  .legend b {
    margin-left: 4px;
    color: var(--ink);
  }

  .legend i {
    display: inline-block;
    width: 14px;
    height: 0;
    margin-right: 5px;
    vertical-align: middle;
    border-top: 2px solid;
  }

  i.temp {
    border-color: var(--warm);
  }

  i.setpoint {
    border-color: var(--cool);
  }

  i.programme {
    border-color: var(--ink-3);
    border-top-style: dashed;
  }

  .note {
    margin: 0;
    font-size: 12.5px;
    color: var(--ink-2);
  }
</style>
