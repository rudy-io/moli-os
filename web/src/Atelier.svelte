<script>
  import { onMount } from 'svelte';
  import { hub, connect, command } from './lib/hub.svelte.js';
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

  // Gateways expose their pairing window as a `pairing` point on a
  // `<instance>:bridge` device: they get a button, not a room card.
  const isGateway = (d) =>
    d.id.endsWith(':bridge') &&
    d.points?.some((p) => p.key === 'pairing' && p.access?.write) === true;
  const gateways = $derived(Object.values(hub.devices).filter(isGateway));

  const rooms = $derived.by(() => {
    const groups = new Map();
    const devices = Object.values(hub.devices)
      .filter((d) => !isGateway(d))
      .sort((a, b) =>
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

<main>
  <section class="home" aria-label={t('commun.atelier.maison')}>
    <EnergyStrip onopen={() => (energyOpen = true)} />
    {#if gateways.length}
      <section class="gateways">
        <h2>{t('commun.atelier.passerelles')}</h2>
        <div class="gw-grid">
          {#each gateways as g (g.id)}
            {@const open = g.state.pairing?.value === true}
            {@const busy = `${g.id}/pairing` in hub.pending}
            <div class="gateway">
              <span class="gw-name">{deviceName(g)}</span>
              <button
                class="pair"
                class:open={open}
                disabled={busy}
                title={t('commun.atelier.appairage_info')}
                onclick={() => command(`${g.id}/pairing`, !open)}
              >
                {open ? t('commun.atelier.fermer_appairage') : t('commun.atelier.appairer')}
              </button>
            </div>
          {/each}
        </div>
      </section>
    {/if}
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
  .gateways {
    margin-bottom: 28px;
  }
  .gw-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(260px, 1fr));
    gap: 14px;
  }
  .gateway {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 12px 16px;
    background: var(--card);
    border: var(--stroke) solid var(--ink);
    border-radius: var(--radius);
  }
  .gw-name {
    font-weight: 700;
    font-size: 14.5px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .pair {
    flex: none;
    border: var(--stroke) solid var(--ink);
    border-radius: var(--radius);
    background: var(--card);
    color: var(--ink);
    font: inherit;
    font-size: 13.5px;
    font-weight: 650;
    padding: 6px 12px;
    cursor: pointer;
  }
  .pair.open {
    background: var(--sun);
    color: #161512;
  }
  .pair:disabled {
    opacity: 0.5;
    cursor: wait;
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
