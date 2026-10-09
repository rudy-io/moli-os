<script>
  import { kwh } from '../lib/energy-live.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';

  /** A typical day: average consumption hour by hour (`profile`, 24 kWh),
   *  off-peak hours shaded (`windows` in minutes), the floor (`base`, W),
   *  and today so far (`today`, 24 kWh or null) against it. */
  let { profile, windows = [], base = null, today = null, now = Date.now() } = $props();

  const max = $derived(Math.max(0.05, ...profile, ...(today ?? []).filter((v) => v != null)));
  const hourNow = $derived(new Date(now).getHours());
  // So far today, and what a usual day has drawn by this time.
  const soFar = $derived(today ? today.reduce((sum, v) => sum + (v ?? 0), 0) : null);
  const usual = $derived.by(() => {
    if (!today) return null;
    const d = new Date(now);
    const frac = (d.getMinutes() + d.getSeconds() / 60) / 60;
    return profile.slice(0, hourNow).reduce((sum, v) => sum + v, 0) + profile[hourNow] * frac;
  });
  const gap = $derived(soFar != null && usual > 0.05 ? Math.round(((soFar - usual) / usual) * 100) : null);
  const offAt = (h) => {
    const m = h * 60 + 30;
    return windows.some((w) => (w.from < w.to ? m >= w.from && m < w.to : m >= w.from || m < w.to));
  };
  const peakHour = $derived(profile.indexOf(Math.max(...profile)));
  let hover = $state(null);
  const shown = $derived(hover ?? peakHour);
</script>

{#if soFar != null}
  <div class="running">
    <span class="muted">{t('commun.jour.jusqua', { h: hourNow })}</span>
    <strong class="num">{kwh(soFar)} kWh</strong>
    <span class="muted">{t('commun.jour.habitude', { kwh: kwh(usual) })}</span>
    {#if gap != null}
      <span class="chip {gap > 10 ? 'warm' : gap < -10 ? 'good' : ''}">{gap > 0 ? '+' : ''}{t('commun.pourcent', { n: gap })}</span>
    {/if}
  </div>
{/if}
<div class="readout">
  <span class="muted">{hover == null ? t('commun.jour.plus_chargee') : t('commun.jour.moyenne')}</span>
  <strong class="num">{t('commun.jour.plage', { from: shown, to: shown + 1, kwh: kwh(profile[shown]) })}</strong>
  {#if today?.[shown] != null}<span class="muted">{t('commun.jour.aujourdhui_kwh', { kwh: kwh(today[shown]) })}</span>{/if}
  {#if offAt(shown)}<span class="chip cool">{t('commun.jour.creuse')}</span>{:else if windows.length}<span class="chip warm">{t('commun.jour.pleine')}</span>{/if}
</div>
<div class="plot" role="img" aria-label={t('commun.jour.conso')} onpointerleave={() => (hover = null)}>
  {#each profile as v, h (h)}
    <button class="col" class:off={offAt(h)} class:on={shown === h} onpointerenter={() => (hover = h)} onfocus={() => (hover = h)} aria-label={t('commun.jour.heure_kwh', { h, kwh: kwh(v) })}>
      <i style:height="{Math.max(2, (v / max) * 100)}%"></i>
      {#if today?.[h] != null}
        <b class="mark" class:running={h === hourNow} style:bottom="{(today[h] / max) * 100}%"></b>
      {/if}
    </button>
  {/each}
  {#if base}
    <span class="floor" style:bottom="{Math.min(100, (base / 1000 / max) * 100)}%" title={t('commun.jour.talon')}></span>
  {/if}
</div>
<div class="hours" aria-hidden="true">
  {#each [0, 6, 12, 18, 24] as h (h)}<span>{t('commun.jour.heure', { h })}</span>{/each}
</div>
<p class="legend muted">
  {#if windows.length}<span><i class="sw off"></i>{t('commun.jour.heures_creuses')}</span>{/if}
  {#if base}<span><i class="sw floor-sw"></i>{t('commun.jour.talon_legende')}</span>{/if}
  {#if today}<span><i class="sw today-sw"></i>{t('commun.jour.aujourdhui')}</span>{/if}
  <span>{t('commun.jour.moyenne_7j')}</span>
</p>

<style>
  .readout {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 6px 10px;
    margin-bottom: 12px;
    font-size: 13px;
  }

  .readout strong {
    font-size: 16px;
    font-weight: 650;
  }

  .plot {
    position: relative;
    display: grid;
    grid-template-columns: repeat(24, minmax(0, 1fr));
    align-items: end;
    gap: 3px;
    height: 140px;
  }

  .running {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 6px 10px;
    margin-bottom: 6px;
    font-size: 13px;
  }

  .running strong {
    font-size: 18px;
    font-weight: 700;
  }

  /* Today's hour, over the usual one. */
  .mark {
    position: absolute;
    left: 1px;
    right: 1px;
    height: 3px;
    border-radius: 2px;
    background: var(--ink);
    transform: translateY(50%);
    pointer-events: none;
  }

  .mark.running {
    background: repeating-linear-gradient(90deg, var(--ink) 0 4px, transparent 4px 6px);
  }

  .sw.today-sw {
    height: 3px;
    background: var(--ink);
  }

  .col {
    all: unset;
    position: relative;
    box-sizing: border-box;
    height: 100%;
    display: flex;
    align-items: flex-end;
    border-radius: 6px;
    cursor: pointer;
  }

  .col.off {
    background: color-mix(in srgb, var(--cool) 12%, transparent);
  }

  .col i {
    display: block;
    width: 100%;
    border-radius: 5px 5px 2px 2px;
    background: var(--warm);
    opacity: 0.85;
    transition: height 0.5s var(--ease);
  }

  .col.off i {
    background: var(--cool);
  }

  .col.on i {
    opacity: 1;
    box-shadow: 0 0 0 2px var(--surface), 0 0 0 4px currentColor;
  }

  .col:focus-visible {
    outline: 2px solid var(--cool);
  }

  .floor {
    position: absolute;
    left: 0;
    right: 0;
    border-top: 2px dashed var(--ink-3);
    pointer-events: none;
  }

  .hours {
    display: flex;
    justify-content: space-between;
    font-size: 11px;
    color: var(--ink-3);
    margin-top: 6px;
  }

  .legend {
    display: flex;
    flex-wrap: wrap;
    gap: 6px 14px;
    font-size: 12px;
    margin-top: 10px;
  }

  .legend span {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }

  .sw {
    width: 12px;
    height: 12px;
    border-radius: 4px;
  }

  .sw.off {
    background: var(--cool);
  }

  .sw.floor-sw {
    height: 0;
    border-top: 2px dashed var(--ink-3);
  }
</style>
