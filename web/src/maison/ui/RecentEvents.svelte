<script>
  import { untrack } from 'svelte';
  import { t, locale } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';
  import { home, relative } from '../lib/home.svelte.js';
  import { latest, readable } from '../lib/review.js';

  /** The newest events of every camera with a past, as pictures: touch
   *  one to see it again in its camera's viewer. `cameras`: [{ id, name }];
   *  `onopen(id, item)`. */
  let { cameras, onopen } = $props();

  /** How far back the strip looks. */
  const HOURS = 24;

  let events = $state([]);
  let loading = $state(true);
  // Pictures arrived, or that will not come: keyed by camera and item.
  let shown = $state({});
  let broken = $state({});

  // Asked again only when the cameras themselves change: not when the page
  // rebuilds their list (a device arrives, another is renamed…).
  const ids = $derived(cameras.map((c) => c.id).join('\n'));
  const names = $derived(new Map(cameras.map((c) => [c.id, c.name])));

  async function load() {
    const before = Math.ceil(Date.now() / 1000);
    const after = before - HOURS * 3600;
    const lists = await Promise.all(
      untrack(() => cameras).map((c) =>
        fetch(`/api/devices/${encodeURIComponent(c.id)}/review?after=${after}&before=${before}`)
          .then((r) => (r.ok ? r.json() : { items: [] }))
          .then((b) => ({ camera: c, items: (b.items ?? []).filter((i) => i.events.length) }))
          .catch(() => ({ camera: c, items: [] })),
      ),
    );
    events = latest(lists, 20);
    // What left the strip is forgotten.
    const kept = new Set(events.map(({ camera, item }) => camera.id + item.id));
    shown = keep(shown, kept);
    broken = keep(broken, kept);
    loading = false;
  }

  const keep = (marks, kept) => Object.fromEntries(Object.entries(marks).filter(([k]) => kept.has(k)));

  $effect(() => {
    ids;
    load();
    const every = setInterval(load, 60_000);
    return () => clearInterval(every);
  });

  const hm = (s) => new Date(s * 1000).toLocaleTimeString(locale(), { hour: '2-digit', minute: '2-digit' });
  const labelName = (l) => {
    const key = `revue.label.${l}`;
    const s = t(key);
    return s === key ? readable(l) : s;
  };
</script>

<section class="card recent" aria-label={t('revue.derniers')}>
  <div class="card-head">
    <h2><Icon name="history" size={18} />{t('revue.derniers')}</h2>
    <span class="muted">{t('revue.depuis', { n: HOURS })}</span>
  </div>
  {#if loading}
    <!-- As many shapes as a phone shows, shining until the list comes. -->
    <ul class="strip" aria-busy="true" aria-label={t('revue.chargement')}>
      {#each [0, 1, 2, 3] as n (n)}
        <li><span class="tile ghost"><span class="bar cam-bar"></span><span class="bar what-bar"></span></span></li>
      {/each}
    </ul>
  {:else if !events.length}
    <p class="muted">{t('revue.rien_24h')}</p>
  {:else}
    <ul class="strip">
      {#each events as { camera, item } (camera.id + item.id)}
        {@const key = camera.id + item.id}
        <li>
          <button class="tile" class:ghost={!shown[key] && !broken[key]} onclick={() => onopen(camera.id, item)} aria-label={t('revue.revoir_camera', { camera: names.get(camera.id) ?? camera.name, heure: hm(item.start) })}>
            {#if broken[key]}
              <span class="no-picture"><Icon name="cctv" size={30} /></span>
            {:else}
              <img
                loading="lazy"
                class:in={shown[key]}
                src="/api/devices/{encodeURIComponent(camera.id)}/review/events/{encodeURIComponent(item.events[0])}/thumbnail"
                alt=""
                onload={() => (shown[key] = true)}
                onerror={() => (broken[key] = true)}
              />
            {/if}
            <span class="cam"><i class={item.severity}></i>{names.get(camera.id) ?? camera.name}</span>
            <span class="what">
              <b>{item.labels.length ? item.labels.map(labelName).join(', ') : t('revue.mouvement')}</b>
              <small>{hm(item.start)} · {relative(item.start * 1000, home.now)}</small>
            </span>
          </button>
        </li>
      {/each}
    </ul>
  {/if}
</section>

<style>
  .recent {
    display: grid;
    gap: 12px;
    min-width: 0;
  }

  .card-head {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    justify-content: space-between;
    gap: 6px 12px;
  }

  .card-head h2 {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    font-size: 15px;
    font-weight: 700;
  }

  .muted {
    color: var(--ink-3);
    font-size: 13px;
  }

  /* One row that scrolls sideways; a little of the next picture shows. */
  .strip {
    list-style: none;
    margin: 0;
    padding: 0 0 6px;
    display: grid;
    grid-auto-flow: column;
    grid-auto-columns: minmax(190px, 220px);
    gap: 12px;
    overflow-x: auto;
    scroll-snap-type: x proximity;
    overscroll-behavior-x: contain;
    scrollbar-width: thin;
  }

  li {
    scroll-snap-align: start;
  }

  .tile {
    position: relative;
    display: block;
    width: 100%;
    aspect-ratio: 4 / 3;
    padding: 0;
    border: 0;
    border-radius: var(--r-sm);
    overflow: hidden;
    background: #0b0f16;
    cursor: pointer;
    color: #fff;
    text-align: left;
    font: inherit;
  }

  span.tile {
    display: block;
  }

  /* Waiting: a light that passes over the dark picture. */
  .ghost::before {
    content: '';
    position: absolute;
    inset: 0;
    background: linear-gradient(100deg, transparent 30%, rgb(255 255 255 / 9%) 50%, transparent 70%) #0b0f16;
    background-size: 220% 100%;
    animation: shine 1.4s linear infinite;
  }

  @keyframes shine {
    from {
      background-position: 120% 0;
    }
    to {
      background-position: -120% 0;
    }
  }

  /* Where the camera's name and the event's words will be. */
  .bar {
    position: absolute;
    left: 10px;
    height: 12px;
    border-radius: 999px;
    background: rgb(255 255 255 / 10%);
  }

  .cam-bar {
    top: 10px;
    width: 38%;
  }

  .what-bar {
    bottom: 12px;
    width: 58%;
  }

  .no-picture {
    position: absolute;
    inset: 0;
    display: grid;
    place-content: center;
    color: #8a93a8;
  }

  button:focus-visible {
    outline: 2px solid var(--cool);
    outline-offset: 2px;
  }

  img {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    object-fit: cover;
    opacity: 0;
    transition:
      opacity 0.25s var(--ease),
      transform 0.3s var(--ease);
  }

  img.in {
    opacity: 1;
  }

  button:hover img {
    transform: scale(1.04);
  }

  .cam {
    position: absolute;
    top: 8px;
    left: 8px;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 3px 9px;
    border-radius: 999px;
    background: rgb(0 0 0 / 60%);
    font-size: 12px;
    font-weight: 650;
  }

  .cam i {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: #f5b14c;
  }

  .cam i.alert {
    background: #ff6b6b;
  }

  .what {
    position: absolute;
    inset: auto 0 0;
    display: grid;
    gap: 1px;
    padding: 22px 10px 8px;
    background: linear-gradient(transparent, rgb(0 0 0 / 78%));
  }

  .what b {
    font-size: 14px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .what small {
    font-size: 12px;
    opacity: 0.85;
    font-variant-numeric: tabular-nums;
  }

  @media (prefers-reduced-motion: reduce) {
    img {
      transition: none;
    }

    .ghost::before {
      animation: none;
    }
  }
</style>
