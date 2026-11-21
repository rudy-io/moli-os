<script>
  import { onMount } from 'svelte';
  import { hub, connect } from './lib/hub.svelte.js';
  import { deviceName } from './lib/format.js';
  import { t, locale } from './lib/i18n.svelte.js';
  import Header from './components/Header.svelte';
  import DriverWarnings from './components/DriverWarnings.svelte';
  import DeviceCard from './components/DeviceCard.svelte';
  import Journal from './components/Journal.svelte';
  import Approvals from './components/Approvals.svelte';
  import HistoryPanel from './components/HistoryPanel.svelte';
  import BenchPanel from './components/BenchPanel.svelte';
  import EnergyStrip from './components/EnergyStrip.svelte';
  import EnergyPanel from './components/EnergyPanel.svelte';
  import { airzonePresent } from './maison/lib/airzone.js';

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

<div class="atelier">
<Header />
{#if airzonePresent(hub.drivers)}
  <a class="integration-link" href="#/airzone"><strong>{t('airzone_ui.open')}</strong><span>{t('airzone_ui.workshop_link')}</span></a>
{/if}
<Approvals />
<DriverWarnings drivers={hub.drivers} />

{#each hub.guard?.unmatched_protected_rooms ?? [] as room (room)}
  <p class="waiting" role="alert">
    <strong>{t('commun.atelier.piece_introuvable', { room })}</strong> {t('commun.atelier.piece_introuvable_detail')}
  </p>
{/each}

{#each hub.drivers.filter((d) => d.status.state === 'waiting') as d (d.instance)}
  <p class="waiting" role="alert"><strong>{d.instance}</strong> {d.status.reason}</p>
{/each}

<div class="layout">
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
</div>

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
</div>

<style>
  .integration-link {
    display: flex;
    flex-wrap: wrap;
    gap: 8px 20px;
    align-items: center;
    min-height: 48px;
    padding: 14px 28px;
    color: var(--ink);
    background: color-mix(in srgb, var(--sun) 16%, var(--card));
    text-decoration: none;
  }
  .integration-link strong { text-decoration: underline; text-underline-offset: 3px; }
  .integration-link span { font-size: 14px; color: var(--ink-2); }
  /* The Atelier's parts were drawn in ink (« Le Relevé »): on this page they
     take the house's look, day and night alike. */
  .atelier {
    --card: var(--surface);
    --paper: var(--surface);
    --bone: var(--bg);
    --radius: var(--r-sm);
    --stroke: 1px;
    --edge: var(--line);
    --card-shadow: var(--shadow);
    --copper: var(--warm-ink);
    --alarm: var(--alert);
    --ok: var(--good);
    --sun-soft: var(--warm-soft);
    font-size: 15px;
  }
  .layout {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 320px;
    gap: 28px;
  }
  @media (max-width: 1100px) {
    .layout {
      grid-template-columns: minmax(0, 1fr);
    }
  }
  .room + .room {
    margin-top: 32px;
  }
  h2 {
    font-size: 19px;
    font-weight: 700;
    margin: 0 0 14px;
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .count {
    font-size: 14px;
    font-weight: 600;
    color: var(--ink-3);
  }
  .lock {
    font-size: 12px;
    font-weight: 600;
    padding: 3px 10px;
    border-radius: 999px;
    background: var(--warm-soft);
    color: var(--warm-ink);
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
    margin: 0 0 12px;
    padding: 14px 18px;
    border-radius: var(--r-sm);
    background: var(--warm-soft);
    color: var(--warm-ink);
    font-weight: 500;
  }
  .waiting strong {
    margin-right: 8px;
    font-weight: 700;
  }
  .toasts {
    position: fixed;
    bottom: calc(28px + env(safe-area-inset-bottom));
    left: 50%;
    transform: translateX(-50%);
    display: grid;
    gap: 8px;
    z-index: 40;
  }
  .toasts p {
    margin: 0;
    padding: 10px 16px;
    background: var(--ink);
    color: var(--bg);
    border-radius: 999px;
    font-size: 14px;
    box-shadow: var(--shadow-lift);
  }
  @media (max-width: 760px) {
    .toasts {
      bottom: calc(110px + env(safe-area-inset-bottom));
    }
  }
</style>
