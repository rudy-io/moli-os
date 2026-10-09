<script>
  import { onMount } from 'svelte';
  import { hub, connect } from './lib/hub.svelte.js';
  import { deviceName } from './lib/format.js';
  import { t, locale } from './lib/i18n.svelte.js';
  import Header from './components/Header.svelte';
  import DeviceCard from './components/DeviceCard.svelte';
  import Journal from './components/Journal.svelte';
  import Approvals from './components/Approvals.svelte';
  import HistoryPanel from './components/HistoryPanel.svelte';
  import BenchPanel from './components/BenchPanel.svelte';
  import EnergyStrip from './components/EnergyStrip.svelte';
  import EnergyPanel from './components/EnergyPanel.svelte';

  let energyOpen = $state(false);

  // Same rule as the server: case, spaces and accents don't count.
  const norm = (s) =>
    s.normalize('NFD').replace(/\p{M}/gu, '').trim().replace(/\s+/g, ' ').toLowerCase();
  const isProtected = (room) =>
    hub.guard?.protected_rooms?.some((p) => norm(p) === norm(room));

  onMount(connect);

  // Not a word: it sorts last and is said with t() where it is shown.
  const NO_ROOM = '§no-room';

  const rooms = $derived.by(() => {
    const groups = new Map();
    const devices = Object.values(hub.devices).sort((a, b) =>
      deviceName(a).localeCompare(deviceName(b), locale()),
    );
    for (const d of devices) {
      // The user's label wins; the system's own room is a fallback.
      const room = d.label?.room || d.native_room || NO_ROOM;
      if (!groups.has(room)) groups.set(room, []);
      groups.get(room).push(d);
    }
    return [...groups.entries()].sort(([a], [b]) =>
      a === NO_ROOM ? 1 : b === NO_ROOM ? -1 : a.localeCompare(b, locale()),
    );
  });
</script>

<Header />
<Approvals />

{#each hub.guard?.unmatched_protected_rooms ?? [] as room (room)}
  <p class="waiting" role="alert">
    <strong>{t('commun.atelier.piece_introuvable', { room })}</strong> {t('commun.atelier.piece_introuvable_detail')}
  </p>
{/each}

{#each hub.drivers.filter((d) => d.status.state === 'waiting') as d (d.instance)}
  <p class="waiting" role="alert"><strong>{d.instance}</strong> {d.status.reason}</p>
{/each}

<main>
  <section class="home" aria-label={t('commun.atelier.maison')}>
    <EnergyStrip onopen={() => (energyOpen = true)} />
    {#if !hub.connected && rooms.length === 0}
      <p class="empty">{t('commun.atelier.connexion')}</p>
    {:else if rooms.length === 0}
      <p class="empty">{t('commun.atelier.aucun_appareil')}</p>
    {/if}
    {#each rooms as [room, devices] (room)}
      <section class="room">
        <h2>
          {room === NO_ROOM ? t('commun.atelier.sans_piece') : room} <span class="count num">{devices.length}</span>
          {#if isProtected(room)}<span class="lock" title={t('commun.atelier.protegee_titre')}>{t('commun.atelier.protegee')}</span>{/if}
        </h2>
        <div class="grid">
          {#each devices as device (device.id)}
            <DeviceCard {device} />
          {/each}
        </div>
      </section>
    {/each}
  </section>
  <Journal />
</main>

{#if hub.chart}
  <HistoryPanel point={hub.chart} onclose={() => (hub.chart = null)} />
{/if}
{#if hub.bench}
  <BenchPanel onclose={() => (hub.bench = false)} />
{/if}
{#if energyOpen}
  <EnergyPanel onclose={() => (energyOpen = false)} />
{/if}

<div class="toasts" role="status" aria-live="polite">
  {#each hub.toasts as toast (toast.id)}
    <p>{toast.text}</p>
  {/each}
</div>

<style>
  main {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 320px;
    gap: 28px;
    max-width: 1440px;
    margin: 0 auto;
    padding: 24px 28px 64px;
  }
  @media (max-width: 1000px) {
    main {
      grid-template-columns: minmax(0, 1fr);
      padding: 16px 16px 48px;
    }
  }
  .room + .room {
    margin-top: 32px;
  }
  h2 {
    font-size: 13px;
    font-weight: 700;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    margin: 0 0 12px;
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .count {
    font-weight: 600;
    color: var(--ink-3);
  }
  .lock {
    font-size: 10.5px;
    letter-spacing: 0.06em;
    padding: 2px 6px;
    border: 1.5px solid var(--copper);
    color: var(--copper);
    border-radius: 3px;
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(260px, 1fr));
    gap: 14px;
  }
  .empty {
    color: var(--ink-2);
  }
  .waiting {
    margin: 0;
    padding: 12px 28px;
    background: var(--sun);
    color: #161512;
    border-bottom: var(--stroke) solid var(--ink);
    font-weight: 600;
  }
  .waiting strong {
    margin-right: 8px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    font-size: 12px;
  }
  .toasts {
    position: fixed;
    bottom: 16px;
    left: 50%;
    transform: translateX(-50%);
    display: grid;
    gap: 8px;
    z-index: 10;
  }
  .toasts p {
    margin: 0;
    padding: 10px 14px;
    background: var(--ink);
    color: var(--bg);
    border-radius: var(--radius);
    font-size: 14px;
  }
</style>
