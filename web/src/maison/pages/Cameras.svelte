<script>
  import { home, hub, roomOf, hidden, protectedRoom, nameOf, clock, relative, deviceKind } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from '../ui/Icon.svelte';
  import CameraTile from '../ui/CameraTile.svelte';
  import LiveVideo from '../ui/LiveVideo.svelte';
  import CameraReview from '../ui/CameraReview.svelte';
  import SecurityPanel from '../ui/SecurityPanel.svelte';
  import RecentEvents from '../ui/RecentEvents.svelte';

  // The layout's cameras first (their names, pace, battery), then any other.
  const cameras = $derived.by(() => {
    const listed = home.config?.favorites?.cameras ?? [];
    const known = new Set(listed.map((c) => c.id));
    const others = Object.values(hub.devices)
      .filter((d) => deviceKind(d) === 'camera' && !known.has(d.id) && !hidden(d.id))
      .map((d) => ({ id: d.id, name: nameOf(d.id), refresh: 15 }));
    return [...listed, ...others].map((c) => ({ ...c, locked: protectedRoom(roomOf(hub.devices[c.id]) ?? '') }));
  });

  let open = $state(null);
  let startWith = $state(null); // an event to play as the viewer opens
  const current = $derived(cameras.find((c) => c.id === open));

  // Live video goes from the camera to this browser: away from home (Access,
  // outside) it cannot, so the images show at once. At home, a live view that
  // fails falls back to them too, never a black screen.
  const atHome = $derived(!['access', 'outside'].includes(hub.session?.via));
  // Live video away from home too, where the house opened its cameras' WebRTC
  // port to the Internet (home.json `live_outside`); else only at home, where
  // the browser reaches the cameras. The past stays home-only (its routes).
  const liveHere = $derived(atHome || home.config?.live_outside === true);
  let noLive = $state(null);

  // Cameras whose NVR tells their past: their newest events, above the
  // security card. At home only: the past (clips above all) is declared
  // « not from outside » (`routes.txt`, column `distant`); away, the images
  // and the live view as before.
  const pastOf = (id) => atHome && hub.devices[id]?.review;
  const reviewed = $derived(cameras.filter((c) => pastOf(c.id)));
  function openAt(id, item = null) {
    startWith = item;
    open = id;
  }

  // What the camera saw today, from the history (rising edges only). A
  // camera with a past has its NVR's list instead: only what the NVR does
  // not tell is kept from the history, the doorbell's presses.
  const KINDS = [
    ['doorbell', 'salon.evenement.sonnette', 'bell'],
    ['person', 'salon.evenement.personne', 'account'],
    ['vehicle', 'salon.evenement.vehicule', 'car'],
    ['animal', 'salon.evenement.animal', 'dog'],
    ['package', 'salon.evenement.colis', 'package'],
  ];
  let seen = $state([]);
  let seenLoading = $state(false);

  $effect(() => {
    const id = open;
    seen = [];
    if (!id) return;
    const d = hub.devices[id];
    const kinds = pastOf(id) ? KINDS.filter(([k]) => k === 'doorbell') : KINDS;
    const keys = kinds.filter(([k]) => d?.points.some((p) => p.key === k));
    if (!keys.length && pastOf(id)) return;
    seenLoading = true;
    Promise.all(
      keys.map(([k, label, icon]) =>
        fetch(`/api/history?point=${encodeURIComponent(`${id}/${k}`)}&hours=24&points=2000`)
          .then((r) => (r.ok ? r.json() : null))
          .then((s) => {
            const out = [];
            const on = (v) => v === true || (typeof v === 'number' && v > 0);
            let prev = s?.before ? on(s.before[1]) : false;
            for (const [ts, v] of s?.raw ?? []) {
              if (on(v) && !prev) out.push({ ts, label, icon });
              prev = on(v);
            }
            return out;
          })
          .catch(() => []),
      ),
    ).then((lists) => {
      if (open !== id) return;
      seen = lists.flat().sort((a, b) => b.ts - a.ts).slice(0, 12);
      seenLoading = false;
    });
  });

  function onkey(e) {
    if (e.key === 'Escape') open = null;
  }
</script>

<svelte:window onkeydown={onkey} />

<div class="cameras">
  <header>
    <h1 class="page-title">{t('salon.cameras.titre')}</h1>
    <p class="page-sub">{t('salon.cameras.sous_titre')}</p>
  </header>

  {#if reviewed.length}
    <RecentEvents cameras={reviewed} onopen={openAt} />
  {/if}
  <!-- Its switches and its « alarm under way » line stay, under the events. -->
  <SecurityPanel />

  <div class="wall">
    {#each cameras as cam (cam.id)}
      <div class="slot">
        <CameraTile id={cam.id} name={cam.name} refresh={cam.refresh} battery={cam.battery} onopen={(id) => openAt(id)} />
        {#if cam.locked}<span class="chip cool lock"><Icon name="lock" size={14} />{t('salon.cameras.protegee')}</span>{/if}
      </div>
    {:else}
      <p class="muted">{t('salon.cameras.aucune')}</p>
    {/each}
  </div>
</div>

{#if current}
  <div class="viewer" role="dialog" aria-modal="true" aria-label={t('salon.cameras.camera', { nom: current.name })}>
    <button class="backdrop" onclick={() => (open = null)} aria-label={t('commun.fermer')}></button>
    <div class="stage">
      <div class="bar">
        <h2>{current.name}</h2>
        <button class="close" onclick={() => (open = null)} aria-label={t('commun.fermer')}><Icon name="close" size={22} /></button>
      </div>
      {#key current.id}
        {#if pastOf(current.id)}
          <!-- Its past from the NVR: frieze, events, recordings, zones. -->
          <CameraReview id={current.id} name={current.name} start={startWith} live={hub.devices[current.id]?.live && !current.locked && atHome} battery={current.battery} />
        {:else if hub.devices[current.id]?.live && !current.locked && liveHere && noLive !== current.id}
          <!-- Live video, played by this browser straight from the camera. -->
          <div class="live-slot"><LiveVideo id={current.id} onfail={() => (noLive = current.id)} /></div>
        {:else}
          <CameraTile id={current.id} name={current.name} refresh={current.battery ? 0 : 2} battery={current.battery} big />
          {#if noLive === current.id}
            <p class="no-live muted">
              {t('salon.cameras.direct_impossible')}
              <button class="link" onclick={() => (noLive = null)}>{t('salon.cameras.essayer_direct')}</button>
            </p>
          {/if}
        {/if}
      {/key}
      {#if !pastOf(current.id) || seen.length}
        <div class="seen">
          <h3>{pastOf(current.id) ? t('salon.cameras.sonnette_24h') : t('salon.cameras.aujourdhui')}</h3>
          {#if seenLoading}
            <p class="muted">…</p>
          {:else if seen.length}
            <ul>
              {#each seen as s (s.ts + s.label)}
                <li><Icon name={s.icon} size={16} /><b>{t(s.label)}</b><span class="muted">{clock(s.ts)} · {relative(s.ts, home.now)}</span></li>
              {/each}
            </ul>
          {:else}
            <p class="muted">{t('salon.cameras.rien')}</p>
          {/if}
        </div>
      {/if}
    </div>
  </div>
{/if}

<style>
  .no-live {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 8px;
    margin: 8px 0 0;
    font-size: 13px;
  }

  .no-live .link {
    background: none;
    border: 0;
    padding: 0;
    color: var(--cool);
    font: inherit;
    cursor: pointer;
    text-decoration: underline;
  }

  .cameras {
    display: grid;
    gap: 22px;
  }

  .wall {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(min(100%, 420px), 1fr));
    gap: 20px;
  }

  .slot {
    position: relative;
    display: grid;
    gap: 8px;
  }

  .lock {
    justify-self: start;
  }

  .viewer {
    position: fixed;
    inset: 0;
    z-index: 50;
    display: grid;
    place-items: center;
    padding: 24px;
  }

  .backdrop {
    position: absolute;
    inset: 0;
    border: 0;
    background: rgb(6 9 14 / 82%);
    backdrop-filter: blur(8px);
  }

  .stage {
    position: relative;
    width: min(1200px, 100%);
    max-height: calc(100dvh - 48px);
    overflow: auto;
    display: grid;
    gap: 14px;
    color: #eef1f7;
    /* A thin, dim scrollbar on the dark, kept clear of the picture. */
    scrollbar-width: thin;
    scrollbar-color: rgb(255 255 255 / 22%) transparent;
    scrollbar-gutter: stable;
    padding-right: 6px;
  }

  .bar {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }

  .bar h2 {
    font-size: 22px;
    font-weight: 700;
  }

  .close {
    width: 44px;
    height: 44px;
    border-radius: 50%;
    border: 0;
    background: rgb(255 255 255 / 12%);
    color: #fff;
    display: grid;
    place-items: center;
  }

  .seen h3 {
    font-size: 13px;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    color: #a9b1c2;
    margin-bottom: 8px;
  }

  .seen ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }

  .seen li {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 14px;
    border-radius: 999px;
    background: rgb(255 255 255 / 9%);
    font-size: 14px;
  }

  .seen .muted {
    color: #a9b1c2;
  }
</style>
