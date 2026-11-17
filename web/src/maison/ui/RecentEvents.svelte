<script>
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

  async function load() {
    const before = Math.ceil(Date.now() / 1000);
    const after = before - HOURS * 3600;
    const lists = await Promise.all(
      cameras.map((c) =>
        fetch(`/api/devices/${encodeURIComponent(c.id)}/review?after=${after}&before=${before}`)
          .then((r) => (r.ok ? r.json() : { items: [] }))
          .then((b) => ({ camera: c, items: (b.items ?? []).filter((i) => i.events.length) }))
          .catch(() => ({ camera: c, items: [] })),
      ),
    );
    events = latest(lists, 20);
    loading = false;
  }

  $effect(() => {
    cameras;
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
    <p class="muted">…</p>
  {:else if !events.length}
    <p class="muted">{t('revue.rien_24h')}</p>
  {:else}
    <ul class="strip">
      {#each events as { camera, item } (camera.id + item.id)}
        <li>
          <button onclick={() => onopen(camera.id, item)} aria-label={t('revue.revoir_camera', { camera: camera.name, heure: hm(item.start) })}>
            <img loading="lazy" src="/api/devices/{encodeURIComponent(camera.id)}/review/events/{encodeURIComponent(item.events[0])}/thumbnail" alt="" />
            <span class="cam"><i class={item.severity}></i>{camera.name}</span>
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

  button {
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
    transition: transform 0.3s var(--ease);
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
  }
</style>
