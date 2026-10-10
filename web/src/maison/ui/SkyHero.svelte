<script>
  // The top of the home page: the greeting, and the weather over the real
  // sky. Behind « 24°, clear sky », the last two hours of Meteosat images
  // close around the house, faded into the page: a picture first, true to
  // what is happening (a storm looks like one). A tap opens the detail: the
  // whole region's clouds and the hours and days to come.
  import { onMount } from 'svelte';
  import { home, value, clock, greeting, num, longDate } from '../lib/home.svelte.js';
  import { sky, watchSky, frameUrl, hours, outlook } from '../lib/sky.svelte.js';
  import { weatherOf } from '../lib/icons.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';
  import SkyDetail from './SkyDetail.svelte';

  const out = $derived(home.config?.outdoor ?? {});
  const w = $derived(out.weather);
  const day = $derived(value(w, 'daylight') !== false);
  const now = $derived(weatherOf(value(w, 'weather_code'), day));
  const hhmm = (iso) => (iso ? String(iso).slice(11, 16) : '—');
  const line = $derived(outlook(hours(sky.forecast)));

  // The loop, slow: an illustration, not a film.
  const frames = $derived(sky.satellite?.frames ?? []);
  const house = $derived(sky.satellite?.house ?? [0.5, 0.5]);
  const size = $derived(sky.satellite?.size ?? [480, 360]);
  let shown = $state(-1);
  let loaded = $state({});
  let open = $state(false);
  const current = $derived(frames.length ? frames[Math.min(Math.max(shown, 0), frames.length - 1)] : null);
  const latest = $derived(frames.at(-1));
  const ready = $derived(latest && loaded[latest]);

  // The crop: the house where the fade ends, the image covering its box.
  let W = $state(0);
  let H = $state(0);
  const place = $derived.by(() => {
    const [iw, ih] = size;
    if (!W || !H) return null;
    const [tx, ty] = [0.36, 0.5];
    const [hx, hy] = house;
    const s = Math.max(W / iw, H / ih, (tx * W) / (hx * iw), ((1 - tx) * W) / ((1 - hx) * iw), (ty * H) / (hy * ih), ((1 - ty) * H) / ((1 - hy) * ih));
    return { w: iw * s, h: ih * s, left: tx * W - hx * iw * s, top: ty * H - hy * ih * s, x: tx * W, y: ty * H };
  });

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
      timer = setTimeout(tick, shown === n - 1 ? 4000 : 900);
    };
    tick();
    return () => {
      clearTimeout(timer);
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
    <button class="sky" class:live={ready} class:night={!day} onclick={() => (open = true)} aria-label={t('maison.meteo.ouvrir', { label: now.label })}>
      <span class="space" aria-hidden="true" bind:clientWidth={W} bind:clientHeight={H}>
        {#each frames as f (f)}
          <img
            src={frameUrl(f)}
            alt=""
            class:on={f === current && loaded[f]}
            style={place ? `width:${place.w}px; height:${place.h}px; left:${place.left}px; top:${place.top}px` : ''}
            onload={() => (loaded[f] = true)}
            decoding="async"
          />
        {/each}
        {#if ready && place}
          <span class="dot" style="left:{place.x}px; top:{place.y}px"></span>
        {/if}
      </span>
      <span class="now">
        <Icon path={now.icon} size={44} />
        <span class="words">
          <strong class="num">{num(value(w, 'temperature'), 0)}°</strong>
          <span class="label">{now.label}</span>
          <span class="small num">↑ {num(value(w, 'today_max'), 0)}° ↓ {num(value(w, 'today_min'), 0)}°
            · <Icon name={day ? 'sunset' : 'sunrise'} size={13} />
            {day ? hhmm(value(w, 'sunset')) : hhmm(value(w, 'sunrise'))}</span>
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
    min-height: 150px;
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
    flex: 0 1 50%;
    min-width: 300px;
    display: flex;
    justify-content: end;
    align-items: end;
    border: 0;
    padding: 12px 18px 10px;
    background: none;
    color: inherit;
    text-align: left;
    border-radius: var(--r-lg);
  }

  /* The picture: right half, melting into the page on the left, top and
     bottom (no frame, no map). */
  .space {
    position: absolute;
    inset: -6px 0 -6px 0;
    z-index: -1;
    overflow: hidden;
    border-radius: 0 var(--r-lg) var(--r-lg) 0;
    -webkit-mask-image: linear-gradient(90deg, transparent 0%, #000 42%), linear-gradient(180deg, transparent 0%, #000 22%, #000 80%, transparent 100%);
    -webkit-mask-composite: source-in;
    mask-image: linear-gradient(90deg, transparent 0%, #000 42%), linear-gradient(180deg, transparent 0%, #000 22%, #000 80%, transparent 100%);
    mask-composite: intersect;
    opacity: 0;
    transition: opacity 1.2s var(--ease);
  }

  .live .space {
    opacity: 1;
  }

  .space img {
    position: absolute;
    max-width: none;
    opacity: 0;
    transition: opacity 0.9s linear;
    filter: saturate(1.1) brightness(0.92);
  }

  .space img.on {
    opacity: 1;
  }

  .dot {
    position: absolute;
    width: 9px;
    height: 9px;
    margin: -4.5px 0 0 -4.5px;
    border-radius: 50%;
    background: var(--warm);
    box-shadow: 0 0 0 2px rgb(255 255 255 / 85%);
  }

  .dot::after {
    content: '';
    position: absolute;
    inset: -2px;
    border-radius: 50%;
    border: 2px solid var(--warm);
    animation: ping 3s var(--ease) infinite;
  }

  @keyframes ping {
    from {
      transform: scale(1);
      opacity: 0.8;
    }
    to {
      transform: scale(3);
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

  /* Over the picture: white words, a soft shadow. */
  .live .now {
    color: #ffd75e;
    filter: drop-shadow(0 1px 8px rgb(0 0 0 / 45%));
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

  .sky:hover .space {
    filter: brightness(1.08);
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
      min-height: 130px;
      margin: 0 -4px;
    }

    .space {
      border-radius: var(--r-lg);
      -webkit-mask-image: linear-gradient(90deg, transparent 0%, #000 30%), linear-gradient(180deg, transparent 0%, #000 18%, #000 82%, transparent 100%);
      mask-image: linear-gradient(90deg, transparent 0%, #000 30%), linear-gradient(180deg, transparent 0%, #000 18%, #000 82%, transparent 100%);
    }
  }
</style>
