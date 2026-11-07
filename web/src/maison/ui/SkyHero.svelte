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
  /** What the page shows under the greeting (the house's modes, what to know). */
  let { children } = $props();

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
    {#if children}<div class="extra">{@render children()}</div>{/if}
  </div>

  {#if w}
    <button class="sky" class:live={sky.map} class:night={!day} onclick={() => (open = true)} aria-label={t('maison.meteo.ouvrir', { label: now.label })}>
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
    gap: 24px;
    padding: 24px;
    min-height: 160px;
    border-radius: var(--r-xl);
    background: var(--glass);
    -webkit-backdrop-filter: var(--glass-blur);
    backdrop-filter: var(--glass-blur);
    box-shadow: var(--shadow), inset 0 0 0 1px var(--glass-line);
  }

  .hello {
    align-self: center;
    flex: 1 1 auto;
    min-width: 0;
  }

  .extra {
    display: grid;
    gap: 12px;
    margin-top: 16px;
    min-width: 0;
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
    overflow: hidden;
    flex: 0 1 460px;
    min-width: 300px;
    min-height: 150px;
    display: flex;
    justify-content: start;
    align-items: center;
    border: 0;
    padding: 20px 24px;
    background: linear-gradient(135deg, #3f7ef0 0%, #6fb4ff 100%);
    color: #fff;
    text-align: left;
    border-radius: var(--r-lg);
    box-shadow: 0 18px 40px -18px rgb(40 90 200 / 55%), inset 0 0 0 1px rgb(255 255 255 / 22%);
    cursor: pointer;
    transition: transform 0.25s var(--ease), box-shadow 0.25s var(--ease);
  }

  .sky.night {
    background: linear-gradient(135deg, #1d2452 0%, #3d3f8f 100%);
    box-shadow: 0 18px 40px -18px rgb(30 30 90 / 60%), inset 0 0 0 1px rgb(255 255 255 / 14%);
  }

  .sky:hover {
    transform: translateY(-2px);
  }

  /* A soft sun (or moon) glow in the corner of the card. */
  .sky::before {
    content: '';
    position: absolute;
    z-index: -2;
    right: -60px;
    top: -70px;
    width: 220px;
    height: 220px;
    border-radius: 50%;
    background: radial-gradient(circle, rgb(255 220 120 / 55%), transparent 70%);
  }

  .sky.night::before {
    background: radial-gradient(circle, rgb(200 210 255 / 30%), transparent 70%);
  }

  /* The map: right half, melting into the page on the left, top and
     bottom (no frame). */
  .space {
    position: absolute;
    inset: 0;
    z-index: -1;
    overflow: hidden;
    border-radius: inherit;
    opacity: 0.85;
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

  /* The weather icon turns gently, like the sun in the sky. */
  .now > :global(svg) {
    filter: drop-shadow(0 6px 14px color-mix(in srgb, var(--sun) 55%, transparent));
    animation: sky-sway 7s ease-in-out infinite;
  }

  @keyframes sky-sway {
    0%,
    100% { transform: translateY(0) rotate(0deg); }
    50% { transform: translateY(-4px) rotate(10deg); }
  }

  h1 {
    animation: hello-in 0.7s var(--ease) backwards;
  }

  @keyframes hello-in {
    from {
      opacity: 0;
      transform: translateY(12px);
      filter: blur(6px);
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .now > :global(svg),
    h1 {
      animation: none;
    }
  }

  .words {
    display: grid;
    color: #fff;
    text-shadow: 0 1px 6px rgb(0 0 0 / 18%);
  }

  .now {
    color: #ffd75e;
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
      gap: 14px;
      padding: 18px;
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
      min-height: 130px;
    }
  }
</style>
