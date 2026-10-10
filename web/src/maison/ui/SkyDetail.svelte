<script>
  // The weather in detail, opened from the home page: the map of the next
  // day around the house (play, pause, slide through the hours: clouds and
  // rain moving, the wind flowing), then the hours and the days to come.
  import { onMount } from 'svelte';
  import { sky, hours, days, outlook, dayName, localTs } from '../lib/sky.svelte.js';
  import { weatherOf } from '../lib/icons.js';
  import { home, num } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';
  import WeatherMap from './WeatherMap.svelte';

  let { onclose } = $props();

  const map = $derived(sky.map);
  const times = $derived(map?.weather?.hours ?? []);
  const offset = $derived(Number(map?.weather?.utc_offset_seconds ?? 0));
  // Now, in the map's hours (it starts at the current hour).
  const nowHour = $derived(times.length ? Math.max(0, (home.now - localTs(times[0], offset)) / 3_600_000) : 0);
  let hour = $state(null);
  let playing = $state(false);
  let tab = $state('heures');
  let dialog;
  const shown = $derived(hour ?? nowHour);
  const last = $derived(Math.max(0, times.length - 1));
  const label = $derived.by(() => {
    if (!times.length) return '';
    if (Math.abs(shown - nowHour) < 0.5) return t('maison.meteo.maintenant');
    const i = Math.min(last, Math.round(shown));
    return t('maison.meteo.heure', { h: Number(times[i].slice(11, 13)) });
  });

  const rows = $derived(hours(sky.forecast));
  const week = $derived(days(sky.forecast));
  const line = $derived(outlook(rows));
  const lo = $derived(Math.min(...week.map((d) => d.min).filter(Number.isFinite)));
  const hi = $derived(Math.max(...week.map((d) => d.max).filter(Number.isFinite)));

  onMount(() => {
    dialog.showModal();
    // Playing: an hour every second and a half, then back to now.
    const timer = setInterval(() => {
      if (!playing || !times.length) return;
      const next = (hour ?? nowHour) + 0.05;
      hour = next > last ? nowHour : next;
    }, 75);
    return () => clearInterval(timer);
  });
</script>

<dialog bind:this={dialog} onclose={onclose} onclick={(e) => e.target === dialog && dialog.close()} aria-label={t('maison.meteo.titre')}>
  <header>
    <h2>{t('maison.meteo.titre')}</h2>
    <button class="close" onclick={() => dialog.close()} aria-label={t('maison.meteo.fermer')}><Icon name="close" size={22} /></button>
  </header>

  {#if map}
    <div class="frame">
      <WeatherMap {map} hour={shown} />
      <span class="when num">{label}</span>
      <div class="controls">
        <button class="play" onclick={() => (playing = !playing)} aria-label={playing ? t('maison.meteo.pause') : t('maison.meteo.lecture')}>
          <Icon name={playing ? 'pause' : 'play'} size={20} />
        </button>
        <input
          type="range"
          min={Math.floor(nowHour * 10) / 10}
          max={last}
          step="0.1"
          value={shown}
          oninput={(e) => {
            playing = false;
            hour = Number(e.currentTarget.value);
          }}
          aria-label={t('maison.meteo.moment')}
        />
      </div>
    </div>
    <div class="legend">
      <span><i class="sw rain"></i>{t('maison.meteo.legende_pluie')}</span>
      <span><i class="sw cloud"></i>{t('maison.meteo.legende_nuages')}</span>
      <span><i class="sw wind"></i>{t('maison.meteo.legende_vent')}</span>
      <span class="src">{t('maison.meteo.credit')}</span>
    </div>
  {/if}

  {#if rows.length}
    <div class="forecast">
      <div class="head">
        <p class="outlook">{line ?? ''}</p>
        <div class="tabs" role="tablist" aria-label={t('maison.meteo.previsions')}>
          <button role="tab" aria-selected={tab === 'heures'} class:on={tab === 'heures'} onclick={() => (tab = 'heures')}>{t('maison.meteo.heures')}</button>
          <button role="tab" aria-selected={tab === 'jours'} class:on={tab === 'jours'} onclick={() => (tab = 'jours')}>{t('maison.meteo.jours')}</button>
        </div>
      </div>
      {#if tab === 'heures'}
        <ol class="strip">
          {#each rows as h, i (h.at)}
            {@const s = weatherOf(h.code, h.day)}
            <li class:first={i === 0} title={s.label}>
              <small>{i === 0 ? t('maison.meteo.maintenant') : t('maison.meteo.heure', { h: h.hour })}</small>
              <Icon path={s.icon} size={24} label={s.label} />
              <b class="num">{num(h.temp, 0)}°</b>
              <em class="num" class:dry={h.rain < 20}>{h.rain}%</em>
            </li>
          {/each}
        </ol>
      {:else}
        <ol class="strip week">
          {#each week as d, i (d.date)}
            {@const s = weatherOf(d.code, true)}
            <li title={s.label}>
              <small>{dayName(d.date, i)}</small>
              <Icon path={s.icon} size={24} label={s.label} />
              <span class="range num">
                <b>{num(d.max, 0)}°</b>
                <i class="bar" aria-hidden="true"><i style="left:{((d.min - lo) / Math.max(1, hi - lo)) * 100}%; right:{((hi - d.max) / Math.max(1, hi - lo)) * 100}%"></i></i>
                <span>{num(d.min, 0)}°</span>
              </span>
              <em class="num" class:dry={d.rain < 20}>{d.rain}%</em>
            </li>
          {/each}
        </ol>
      {/if}
    </div>
  {/if}
  <a class="more" href="#/dehors" onclick={() => dialog.close()}>{t('maison.meteo.dehors')} <Icon name="arrow-top-right" size={16} /></a>
</dialog>

<style>
  dialog {
    border: 0;
    padding: 18px;
    width: min(980px, 96vw);
    max-height: 94dvh;
    overflow-y: auto;
    border-radius: var(--r-xl);
    background: var(--surface);
    color: var(--ink);
    box-shadow: 0 30px 80px rgb(0 0 0 / 40%);
  }

  dialog::backdrop {
    background: rgb(4 6 12 / 60%);
    backdrop-filter: blur(4px);
  }

  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 12px;
  }

  h2 {
    font-size: 22px;
    font-weight: 750;
  }

  .close {
    width: 40px;
    height: 40px;
    border: 0;
    border-radius: 50%;
    display: grid;
    place-items: center;
    background: var(--surface-2);
  }

  .frame {
    position: relative;
    width: min(100%, calc((94dvh - 330px) * 4 / 3));
    min-width: min(100%, 320px);
    aspect-ratio: 4 / 3;
    margin: 0 auto;
    border-radius: var(--r-lg);
    overflow: hidden;
    background: #000;
  }

  .when {
    position: absolute;
    top: 10px;
    left: 12px;
    padding: 4px 12px;
    border-radius: 999px;
    font-weight: 700;
    color: #fff;
    background: rgb(6 10 20 / 55%);
    backdrop-filter: blur(8px);
  }
  .controls {
    position: absolute;
    left: 10px;
    right: 10px;
    bottom: 10px;
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 6px 12px 6px 6px;
    border-radius: 999px;
    background: rgb(6 10 20 / 55%);
    backdrop-filter: blur(8px);
    color: #fff;
  }

  .play {
    width: 38px;
    height: 38px;
    flex: none;
    border: 0;
    border-radius: 50%;
    display: grid;
    place-items: center;
    background: rgb(255 255 255 / 16%);
    color: #fff !important;
  }

  input {
    flex: 1;
    min-width: 0;
    accent-color: var(--warm);
  }

  .legend {
    display: flex;
    flex-wrap: wrap;
    gap: 6px 14px;
    align-items: center;
    font-size: 12px;
    color: var(--ink-3);
    margin: 8px 4px 0;
  }

  .legend span {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }

  .legend .src {
    margin-left: auto;
  }

  .sw {
    width: 22px;
    height: 8px;
    border-radius: 4px;
  }

  .sw.rain {
    background: linear-gradient(90deg, #5a96ff, #46c88c, #f0d246, #f05a46);
  }

  .sw.cloud {
    background: linear-gradient(90deg, #0d1b2c, #eef1f6);
  }

  .sw.wind {
    background: linear-gradient(90deg, #ffffff, #cde8ff, #ffd678, #ff785c);
  }
  .forecast {
    margin-top: 16px;
  }

  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    flex-wrap: wrap;
    margin-bottom: 8px;
  }

  .outlook {
    font-weight: 650;
  }

  .tabs {
    display: flex;
    gap: 2px;
    padding: 3px;
    border-radius: 999px;
    background: var(--surface-2);
  }

  .tabs button {
    border: 0;
    border-radius: 999px;
    padding: 6px 14px;
    font-size: 13px;
    font-weight: 650;
    background: none;
    color: var(--ink-3);
  }

  .tabs button.on {
    background: var(--surface);
    color: var(--ink);
    box-shadow: var(--shadow);
  }

  .strip {
    list-style: none;
    margin: 0;
    padding: 0 0 4px;
    display: flex;
    gap: 4px;
    overflow-x: auto;
    scrollbar-width: thin;
  }

  .strip li {
    flex: 1 0 58px;
    display: grid;
    justify-items: center;
    gap: 4px;
    padding: 8px 2px;
    border-radius: 14px;
  }

  .strip li.first {
    background: var(--surface-2);
  }

  .strip small {
    font-size: 12px;
    font-weight: 650;
    color: var(--ink-3);
    white-space: nowrap;
  }

  .strip b {
    font-size: 16px;
    font-weight: 700;
  }

  .strip em {
    font-style: normal;
    font-size: 11px;
    font-weight: 700;
    color: var(--cool);
  }

  .strip em.dry {
    visibility: hidden;
  }

  .week li {
    flex: 1 0 86px;
  }

  .range {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 13px;
  }

  .range span {
    color: var(--ink-3);
  }

  .bar {
    position: relative;
    width: 28px;
    height: 4px;
    border-radius: 2px;
    background: var(--surface-3);
  }

  .bar i {
    position: absolute;
    top: 0;
    bottom: 0;
    border-radius: 2px;
    background: linear-gradient(90deg, #6cb6f5, #f5a742);
  }

  .more {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    margin-top: 12px;
    font-size: 14px;
    font-weight: 650;
    color: var(--ink-2);
    text-decoration: none;
  }
</style>
