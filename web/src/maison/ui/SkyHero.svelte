<script>
  // The top of the home page: the greeting over the real sky. The last two
  // hours of Meteosat images play in a loop behind it, the house pinned in
  // the middle, so the clouds are seen coming; below, the hours and days to
  // come. Without images (no position, no network), a painted sky.
  import { onMount } from 'svelte';
  import { home, value, clock, greeting, num, longDate } from '../lib/home.svelte.js';
  import { sky, watchSky, frameUrl, hours, days, outlook, dayName, frameClock } from '../lib/sky.svelte.js';
  import { weatherOf } from '../lib/icons.js';
  import { t, locale } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';
  import SkyViewer from './SkyViewer.svelte';

  const out = $derived(home.config?.outdoor ?? {});
  const w = $derived(out.weather);
  const day = $derived(value(w, 'daylight') !== false);
  const now = $derived(weatherOf(value(w, 'weather_code'), day));
  const hhmm = (iso) => (iso ? String(iso).slice(11, 16) : '—');

  const rows = $derived(hours(sky.forecast));
  const week = $derived(days(sky.forecast));
  const line = $derived(outlook(rows));
  // The week's span, for the bars of the days.
  const lo = $derived(Math.min(...week.map((d) => d.min).filter(Number.isFinite)));
  const hi = $derived(Math.max(...week.map((d) => d.max).filter(Number.isFinite)));

  let tab = $state('heures');

  // The loop: one image every 0.6 s, the latest held a while.
  const frames = $derived(sky.satellite?.frames ?? []);
  const house = $derived(sky.satellite?.house ?? [0.5, 0.5]);
  let shown = $state(-1);
  let loaded = $state({});
  let viewer = $state(false);
  const current = $derived(frames.length ? frames[Math.min(Math.max(shown, 0), frames.length - 1)] : null);
  const latest = $derived(frames.at(-1));
  const ready = $derived(latest && loaded[latest]);
  const age = $derived(latest ? Math.max(0, Math.round((home.now - Date.parse(latest)) / 60_000)) : null);

  onMount(() => {
    const stop = watchSky();
    const still = window.matchMedia('(prefers-reduced-motion: reduce)');
    let timer;
    const tick = () => {
      const n = frames.length;
      if (!n) {
        timer = setTimeout(tick, 1000);
        return;
      }
      // Only images already arrived take part (never a blank in the loop).
      const next = still.matches || document.hidden ? n - 1 : (shown + 1) % n;
      shown = loaded[frames[next]] || next === n - 1 ? next : n - 1;
      timer = setTimeout(tick, shown === n - 1 ? 2600 : 600);
    };
    tick();
    return () => {
      clearTimeout(timer);
      stop();
    };
  });
</script>

<section class="hero" class:night={!day} class:live={ready} style="--hx:{house[0] * 100}%; --hy:{house[1] * 100}%">
  <div class="space" aria-hidden="true">
    {#each frames as f (f)}
      <img src={frameUrl(f)} alt="" class:on={f === current && loaded[f]} onload={() => (loaded[f] = true)} decoding="async" />
    {/each}
  </div>
  <div class="veil" aria-hidden="true"></div>

  {#if ready}
    <button class="pin" onclick={() => (viewer = true)} aria-label={t('maison.meteo.voir_nuages')}>
      <span class="dot"></span>
      <span class="pin-label">{t('maison.meteo.maison')}</span>
    </button>
  {/if}

  <div class="top">
    <div class="hello">
      <p class="date">{longDate(home.now)}</p>
      <h1>{greeting(home.now)}</h1>
      <p class="time num">{clock(home.now)}</p>
    </div>
    {#if w}
      <a class="now" href="#/dehors" aria-label={t('maison.accueil.meteo', { label: now.label })}>
        <Icon path={now.icon} size={52} />
        <div>
          <strong class="num">{num(value(w, 'temperature'), 0)}°</strong>
          <span class="label">{now.label}</span>
          <span class="small num">↑ {num(value(w, 'today_max'), 0)}° ↓ {num(value(w, 'today_min'), 0)}°</span>
          <span class="small">
            <Icon name={day ? 'sunset' : 'sunrise'} size={14} />
            {day ? t('maison.accueil.coucher', { time: hhmm(value(w, 'sunset')) }) : t('maison.accueil.lever', { time: hhmm(value(w, 'sunrise')) })}
          </span>
        </div>
      </a>
    {/if}
  </div>

  {#if rows.length}
    <div class="glass">
      <div class="glass-head">
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

  {#if ready}
    <button class="credit" onclick={() => (viewer = true)}>
      <Icon name="play-circle" size={14} />
      {current ? frameClock(current) : ''} · {t('maison.meteo.vu_espace', { min: age })}
    </button>
  {/if}
</section>

{#if viewer}
  <SkyViewer onclose={() => (viewer = false)} />
{/if}

<style>
  .hero {
    position: relative;
    isolation: isolate;
    overflow: hidden;
    border-radius: var(--r-xl);
    min-height: 340px;
    padding: 26px 28px 18px;
    display: grid;
    grid-template-rows: auto 1fr auto;
    gap: 14px;
    color: #fff;
    /* The painted sky, until (or without) the real one. */
    background: linear-gradient(165deg, #3f78d8 0%, #6fa3ec 55%, #a9c9f2 100%);
    box-shadow: var(--shadow);
  }

  .hero.night {
    background: linear-gradient(165deg, #0b1324 0%, #18264a 60%, #2c3c66 100%);
  }

  .space {
    position: absolute;
    inset: 0;
    z-index: -2;
  }

  .space img {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    object-fit: cover;
    /* The house stays where the pin is, whatever the crop. */
    object-position: var(--hx) var(--hy);
    opacity: 0;
    transition: opacity 0.5s linear;
    filter: saturate(1.08) contrast(1.04);
  }

  .space img.on {
    opacity: 1;
  }

  /* Enough shade for white words over white clouds. */
  .veil {
    position: absolute;
    inset: 0;
    z-index: -1;
    background:
      linear-gradient(90deg, rgb(6 10 20 / 58%) 0%, rgb(6 10 20 / 12%) 38%, rgb(6 10 20 / 12%) 62%, rgb(6 10 20 / 52%) 100%),
      linear-gradient(180deg, rgb(6 10 20 / 25%) 0%, transparent 35%, rgb(6 10 20 / 45%) 100%);
    opacity: 0;
    transition: opacity 0.8s;
  }

  .live .veil {
    opacity: 1;
  }

  .pin {
    position: absolute;
    left: var(--hx);
    top: var(--hy);
    z-index: 0;
    transform: translate(-7px, -7px);
    display: flex;
    align-items: center;
    gap: 8px;
    border: 0;
    padding: 0;
    background: none;
    color: #fff;
  }

  .dot {
    width: 14px;
    height: 14px;
    border-radius: 50%;
    background: var(--warm);
    box-shadow:
      0 0 0 3px rgb(255 255 255 / 85%),
      0 2px 10px rgb(0 0 0 / 40%);
    position: relative;
  }

  .dot::after {
    content: '';
    position: absolute;
    inset: -3px;
    border-radius: 50%;
    border: 2px solid var(--warm);
    animation: ping 2.4s var(--ease) infinite;
  }

  @keyframes ping {
    from {
      transform: scale(1);
      opacity: 0.9;
    }
    to {
      transform: scale(3.2);
      opacity: 0;
    }
  }

  .pin-label {
    font-size: 12px;
    font-weight: 700;
    letter-spacing: 0.02em;
    padding: 3px 8px;
    border-radius: 999px;
    background: rgb(6 10 20 / 45%);
    backdrop-filter: blur(6px);
  }

  .top {
    display: flex;
    justify-content: space-between;
    align-items: start;
    gap: 20px;
    flex-wrap: wrap;
    text-shadow: 0 1px 12px rgb(0 0 0 / 35%);
  }

  .date {
    font-weight: 600;
    opacity: 0.85;
  }

  h1 {
    font-size: 40px;
    font-weight: 750;
    letter-spacing: -0.03em;
    line-height: 1.1;
  }

  .time {
    font-size: 22px;
    opacity: 0.9;
  }

  .now {
    display: flex;
    align-items: center;
    gap: 14px;
    color: inherit;
    text-decoration: none;
    padding: 6px 10px;
    border-radius: var(--r-md);
    transition: background 0.2s var(--ease);
  }

  .now:hover {
    background: rgb(255 255 255 / 10%);
  }

  .now div {
    display: grid;
  }

  .now strong {
    font-size: 52px;
    font-weight: 300;
    letter-spacing: -0.03em;
    line-height: 1;
  }

  .label {
    font-weight: 650;
  }

  .small {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: 13px;
    font-weight: 550;
    opacity: 0.9;
  }

  .glass {
    grid-row: 3;
    min-width: 0;
    border-radius: var(--r-md);
    padding: 12px 14px 10px;
    background: rgb(10 16 30 / 38%);
    backdrop-filter: blur(14px) saturate(1.2);
    -webkit-backdrop-filter: blur(14px) saturate(1.2);
    box-shadow: inset 0 0 0 1px rgb(255 255 255 / 12%);
  }

  .glass-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    margin-bottom: 8px;
  }

  .outlook {
    font-weight: 650;
    font-size: 15px;
    min-width: 0;
  }

  .tabs {
    flex: none;
    display: flex;
    gap: 2px;
    padding: 3px;
    border-radius: 999px;
    background: rgb(255 255 255 / 10%);
  }

  .tabs button {
    border: 0;
    border-radius: 999px;
    padding: 5px 12px;
    font-size: 13px;
    font-weight: 650;
    background: none;
    color: rgb(255 255 255 / 75%);
  }

  .tabs button.on {
    background: rgb(255 255 255 / 92%);
    color: #111827;
  }

  .strip {
    list-style: none;
    margin: 0;
    padding: 0 0 4px;
    display: flex;
    gap: 4px;
    overflow-x: auto;
    scrollbar-width: none;
    scroll-snap-type: x proximity;
  }

  .strip li {
    flex: 1 0 56px;
    scroll-snap-align: start;
    display: grid;
    justify-items: center;
    gap: 4px;
    padding: 6px 2px;
    border-radius: 14px;
  }

  .strip li.first {
    background: rgb(255 255 255 / 12%);
  }

  .strip small {
    font-size: 12px;
    font-weight: 650;
    opacity: 0.8;
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
    color: #9cc8ff;
  }

  .strip em.dry {
    visibility: hidden;
  }

  .week li {
    flex: 1 0 84px;
  }

  .range {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 13px;
  }

  .range span {
    opacity: 0.7;
  }

  .bar {
    position: relative;
    width: 28px;
    height: 4px;
    border-radius: 2px;
    background: rgb(255 255 255 / 18%);
  }

  .bar i {
    position: absolute;
    top: 0;
    bottom: 0;
    border-radius: 2px;
    background: linear-gradient(90deg, #8fd0ff, #ffc46b);
  }

  .credit {
    position: absolute;
    right: 18px;
    bottom: 6px;
    display: none;
  }

  @media (min-width: 761px) {
    .credit {
      position: absolute;
      top: auto;
      left: 50%;
      right: auto;
      bottom: auto;
      transform: translateX(-50%);
      top: 18px;
      display: inline-flex;
      align-items: center;
      gap: 6px;
      border: 0;
      padding: 4px 10px;
      border-radius: 999px;
      font-size: 12px;
      font-weight: 650;
      color: rgb(255 255 255 / 88%);
      background: rgb(6 10 20 / 35%);
      backdrop-filter: blur(6px);
    }
  }

  @media (max-width: 760px) {
    .hero {
      min-height: 460px;
      padding: 20px 16px 14px;
      border-radius: var(--r-lg);
    }

    h1 {
      font-size: 30px;
    }

    .now strong {
      font-size: 42px;
    }

    .now {
      padding: 0;
    }

    .glass-head {
      flex-wrap: wrap;
    }
  }
</style>
