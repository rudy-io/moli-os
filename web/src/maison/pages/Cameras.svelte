<script>
  import { home, hub, roomOf, hidden, protectedRoom, nameOf, clock, relative } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from '../ui/Icon.svelte';
  import CameraTile from '../ui/CameraTile.svelte';
  import SecurityPanel from '../ui/SecurityPanel.svelte';

  // The layout's cameras first (their names, pace, battery), then any other.
  const cameras = $derived.by(() => {
    const listed = home.config?.favorites?.cameras ?? [];
    const known = new Set(listed.map((c) => c.id));
    const others = Object.values(hub.devices)
      .filter((d) => d.camera && !known.has(d.id) && !hidden(d.id))
      .map((d) => ({ id: d.id, name: nameOf(d.id), refresh: 15 }));
    return [...listed, ...others].map((c) => ({ ...c, locked: protectedRoom(roomOf(hub.devices[c.id]) ?? '') }));
  });

  let open = $state(null);
  const current = $derived(cameras.find((c) => c.id === open));

  // What the camera saw today, from the history (rising edges only).
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
    const keys = KINDS.filter(([k]) => d?.points.some((p) => p.key === k));
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

  <SecurityPanel />

  <div class="wall">
    {#each cameras as cam (cam.id)}
      <div class="slot">
        <CameraTile id={cam.id} name={cam.name} refresh={cam.refresh} battery={cam.battery} onopen={(id) => (open = id)} />
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
        <CameraTile id={current.id} name={current.name} refresh={current.battery ? 0 : 2} battery={current.battery} big />
      {/key}
      <div class="seen">
        <h3>{t('salon.cameras.aujourdhui')}</h3>
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
    </div>
  </div>
{/if}

<style>
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
