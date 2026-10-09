<script>
  import { onDestroy, onMount, untrack } from 'svelte';
  import { device, value, home, relative } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';

  /** A camera image, refreshed every `refresh` seconds while visible.
   *  Battery cameras refresh only when asked: every image wakes them. */
  let { id, name, refresh = 10, battery = false, big = false, onopen = null } = $props();

  /** Never two automatic images closer than this, whatever re-runs. */
  const MIN_GAP = 1000;

  let src = $state(null);
  let loading = $state(false);
  let failed = $state(false);
  let shotAt = $state(null);
  // A tab in the background asks for nothing (it catches up when shown).
  let visible = document.visibilityState === 'visible';
  let lastAt = 0;
  let destroyed = false;

  // Plain values: a parent rebuilding its camera objects does not restart
  // the tile (each restart fetched an image: one per live event).
  const camera = $derived(id);
  const every = $derived(!battery && refresh > 0 ? refresh * 1000 : 0);

  async function load(asked = false) {
    if (loading || !visible) return;
    if (!asked && Date.now() - lastAt < MIN_GAP) return;
    lastAt = Date.now();
    loading = true;
    try {
      const res = await fetch(`/api/devices/${encodeURIComponent(id)}/snapshot`);
      if (!res.ok) throw new Error(res.status);
      const blob = await res.blob();
      const url = URL.createObjectURL(blob);
      // Swap only once decoded: no flash between frames.
      const img = new Image();
      img.src = url;
      // A hidden tab never settles decode(): never wait for it for long.
      await Promise.race([img.decode().catch(() => {}), new Promise((r) => setTimeout(r, 2000))]);
      // Gone while it loaded: nobody will show (nor free) it.
      if (destroyed) {
        URL.revokeObjectURL(url);
        return;
      }
      if (src) URL.revokeObjectURL(src);
      src = url;
      shotAt = Date.now();
      failed = false;
    } catch {
      failed = true;
    } finally {
      loading = false;
    }
  }

  function onVisibility() {
    visible = document.visibilityState === 'visible';
    if (visible) load();
  }

  onMount(() => {
    document.addEventListener('visibilitychange', onVisibility);
    return () => document.removeEventListener('visibilitychange', onVisibility);
  });

  $effect(() => {
    void camera;
    const ms = every;
    // Untracked: `load` reads and writes `loading`, which must not restart this.
    untrack(() => load(true));
    if (!ms) return;
    const timer = setInterval(() => load(), ms);
    return () => clearInterval(timer);
  });

  onDestroy(() => {
    destroyed = true;
    if (src) URL.revokeObjectURL(src);
  });

  const EVENTS = [
    ['doorbell', 'salon.evenement.sonnette', 'bell', 'alert'],
    ['person', 'salon.evenement.personne', 'account', 'warm'],
    ['vehicle', 'salon.evenement.vehicule', 'car', 'cool'],
    ['animal', 'salon.evenement.animal', 'dog', 'good'],
    ['package', 'salon.evenement.colis', 'package', 'good'],
    ['motion', 'salon.evenement.mouvement', 'motion', ''],
  ];
  const active = $derived(EVENTS.filter(([key]) => value(id, key) === true || (key === 'person' && Number(value(id, 'person')) > 0)));
  const online = $derived(device(id)?.online !== false);
</script>

<figure class="cam" class:big>
  <button class="frame" onclick={() => (onopen ? onopen(id) : load(true))} aria-label={t(onopen ? 'salon.camera.agrandir' : 'salon.camera.actualiser', { nom: name })}>
    {#if src}
      <img {src} alt={t('salon.camera.image', { nom: name })} />
    {:else}
      <span class="placeholder"><Icon name="cctv" size={34} /><small>{failed ? t('salon.camera.indisponible') : t('salon.camera.chargement')}</small></span>
    {/if}
    <span class="top">
      <span class="live" class:off={!online || failed}><i></i>{name}</span>
      {#if battery}<span class="chip">{t('salon.camera.batterie')} · {shotAt ? relative(shotAt, home.now) : t('salon.camera.toucher_voir')}</span>{/if}
    </span>
    {#if active.length}
      <span class="events">
        {#each active as [key, label, icon, tone] (key)}
          <span class="chip {tone}"><Icon name={icon} size={15} />{t(label)}</span>
        {/each}
      </span>
    {/if}
    {#if loading && !src}<span class="spinner" aria-hidden="true"></span>{/if}
  </button>
</figure>

<style>
  .cam {
    margin: 0;
  }

  .frame {
    all: unset;
    box-sizing: border-box;
    position: relative;
    display: block;
    width: 100%;
    aspect-ratio: 16 / 9;
    border-radius: var(--r-md);
    overflow: hidden;
    background: #111621;
    cursor: pointer;
  }

  .frame:focus-visible {
    outline: 3px solid var(--cool);
    outline-offset: 3px;
  }

  img {
    width: 100%;
    height: 100%;
    object-fit: cover;
    display: block;
  }

  .placeholder {
    position: absolute;
    inset: 0;
    display: grid;
    place-content: center;
    justify-items: center;
    gap: 8px;
    color: #8a93a8;
    font-size: 13px;
  }

  .top {
    position: absolute;
    top: 12px;
    left: 12px;
    right: 12px;
    display: flex;
    justify-content: space-between;
    gap: 8px;
  }

  .live {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    padding: 6px 12px 6px 10px;
    border-radius: 999px;
    background: rgb(10 14 22 / 55%);
    backdrop-filter: blur(6px);
    color: #fff;
    font-weight: 650;
    font-size: 13px;
  }

  .live i {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: #3ddc97;
    box-shadow: 0 0 0 4px rgb(61 220 151 / 25%);
  }

  .live.off i {
    background: #8a93a8;
    box-shadow: none;
  }

  .top .chip {
    background: rgb(10 14 22 / 55%);
    color: #e8ecf4;
    backdrop-filter: blur(6px);
  }

  .events {
    position: absolute;
    left: 12px;
    bottom: 12px;
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }

  .spinner {
    position: absolute;
    right: 14px;
    bottom: 14px;
    width: 18px;
    height: 18px;
    border-radius: 50%;
    border: 2px solid #fff6;
    border-top-color: #fff;
    animation: spin 0.8s linear infinite;
  }

  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
</style>
