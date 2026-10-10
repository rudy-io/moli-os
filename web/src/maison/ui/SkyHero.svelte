<script>
  // The top of the home page: the greeting, and the weather over a living
  // map. Behind « 24°, clear sky », the land around the house with the
  // clouds, the rain and the wind flowing as they are now, faded into the
  // page: a picture first, true to what is happening (a storm looks like
  // one). A tap opens the detail: the next day on the map, the hours and the
  // days to come.
  import { onMount } from 'svelte';
  import { home, value, clock, greeting, num, longDate } from '../lib/home.svelte.js';
  import { sky, watchSky, hours, outlook, localTs } from '../lib/sky.svelte.js';
  import { weatherOf } from '../lib/icons.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';
  import WeatherMap from './WeatherMap.svelte';
  import SkyDetail from './SkyDetail.svelte';

  const out = $derived(home.config?.outdoor ?? {});
  const w = $derived(out.weather);
  const day = $derived(value(w, 'daylight') !== false);
  const now = $derived(weatherOf(value(w, 'weather_code'), day));
  const hhmm = (iso) => (iso ? String(iso).slice(11, 16) : '—');
  const line = $derived(outlook(hours(sky.forecast)));
  let open = $state(false);
  let narrow = $state(false);

  // The map's « now »: hours since its first one.
  const hour = $derived.by(() => {
    const first = sky.map?.weather?.hours?.[0];
    if (!first) return 0;
    const t0 = localTs(first, Number(sky.map.weather.utc_offset_seconds ?? 0));
    return Math.max(0, (home.now - t0) / 3_600_000);
  });

  onMount(() => {
    const phone = window.matchMedia('(max-width: 760px)');
    narrow = phone.matches;
    const change = () => (narrow = phone.matches);
    phone.addEventListener('change', change);
    const stop = watchSky();
    return () => {
      phone.removeEventListener('change', change);
      stop();
    };
  });
</script>

<section class="hero">
  <div class="hello">
    <p class="date">{longDate(home.now)}</p>
    <h1>{greeting(home.now)}</h1>
    <p class="time num">{clock(home.now)}</p>
  </div>

  {#if w}
    <button class="sky" class:live={sky.map} onclick={() => (open = true)} aria-label={t('maison.meteo.ouvrir', { label: now.label })}>
      {#if sky.map}
        <span class="space" aria-hidden="true">
          <WeatherMap map={sky.map} {hour} tx={narrow ? 0.17 : 0.34} ty={0.5} />
        </span>
      {/if}
      <span class="now">
        <Icon path={now.icon} size={44} />
        <span class="words">
          <strong class="num">{num(value(w, 'temperature'), 0)}°</strong>
          <span class="label">{now.label}</span>
          <span class="small num"
            >↑ {num(value(w, 'today_max'), 0)}° ↓ {num(value(w, 'today_min'), 0)}° ·
            <Icon name={day ? 'sunset' : 'sunrise'} size={13} />
            {day ? hhmm(value(w, 'sunset')) : hhmm(value(w, 'sunrise'))}</span
          >
          {#if line}<span class="small outlook">{line}</span>{/if}
        </span>
      </span>
    </button>
  {/if}
</section>

{#if open}
  <SkyDetail onclose={() => (open = false)} />
{/if}

<style>
  .hero {
    display: flex;
    justify-content: space-between;
    align-items: stretch;
    gap: 16px;
    padding: 6px 0 0 4px;
    min-height: 160px;
  }

  .hello {
    align-self: end;
  }

  .date {
    color: var(--ink-3);
    font-weight: 600;
  }

  h1 {
    font-size: 40px;
    font-weight: 750;
    letter-spacing: -0.03em;
    line-height: 1.1;
  }

  .time {
    font-size: 22px;
    font-weight: 400;
    color: var(--ink-2);
  }

  .sky {
    position: relative;
    isolation: isolate;
    flex: 0 1 52%;
    min-width: 300px;
    display: flex;
    justify-content: end;
    align-items: end;
    border: 0;
    padding: 12px 18px 12px;
    background: none;
    color: inherit;
    text-align: left;
    border-radius: var(--r-lg);
  }

  /* The map: right half, melting into the page on the left, top and
     bottom (no frame). */
  .space {
    position: absolute;
    inset: -8px 0 -8px 0;
    z-index: -1;
    overflow: hidden;
    border-radius: 0 var(--r-lg) var(--r-lg) 0;
    -webkit-mask-image: linear-gradient(90deg, transparent 0%, #000 40%), linear-gradient(180deg, transparent 0%, #000 20%, #000 82%, transparent 100%);
    -webkit-mask-composite: source-in;
    mask-image: linear-gradient(90deg, transparent 0%, #000 40%), linear-gradient(180deg, transparent 0%, #000 20%, #000 82%, transparent 100%);
    mask-composite: intersect;
    animation: appear 1.2s var(--ease) both;
  }

  @keyframes appear {
    from {
      opacity: 0;
    }
  }

  .now {
    display: flex;
    align-items: center;
    gap: 12px;
    color: var(--sun);
  }

  .words {
    display: grid;
    color: var(--ink);
  }

  /* Over the map: white words, a soft shadow. */
  .live .now {
    color: #ffd75e;
    filter: drop-shadow(0 1px 8px rgb(0 0 0 / 55%));
  }

  .live .words {
    color: #fff;
  }

  .words strong {
    font-size: 46px;
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
    gap: 5px;
    font-size: 13px;
    font-weight: 550;
    opacity: 0.92;
  }

  .outlook {
    max-width: 230px;
  }

  @media (max-width: 760px) {
    .hero {
      flex-direction: column;
      gap: 6px;
    }

    .hello {
      align-self: start;
    }

    h1 {
      font-size: 32px;
    }

    .sky {
      flex: none;
      min-width: 0;
      min-height: 140px;
      margin: 0 -4px;
    }

    .space {
      border-radius: var(--r-lg);
      -webkit-mask-image: linear-gradient(90deg, transparent 0%, #000 28%), linear-gradient(180deg, transparent 0%, #000 16%, #000 84%, transparent 100%);
      mask-image: linear-gradient(90deg, transparent 0%, #000 28%), linear-gradient(180deg, transparent 0%, #000 16%, #000 84%, transparent 100%);
    }
  }
</style>
