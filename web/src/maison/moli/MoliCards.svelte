<script>
  import { home, roomList } from '../lib/home.svelte.js';
  import SmartDevice from './SmartDevice.svelte';
  import WeatherCard from './WeatherCard.svelte';
  import HistoryCard from './HistoryCard.svelte';
  import RoomCard from '../ui/RoomCard.svelte';
  import EnergyCard from '../ui/EnergyCard.svelte';
  import Remote from '../ui/Remote.svelte';
  import CameraTile from '../ui/CameraTile.svelte';
  import AutomationCard from './AutomationCard.svelte';

  /** What Moli chose to show, laid out: one column in the bubble, a
   *  board on Moli's page. */
  let { cards = [], compact = false } = $props();

  const rooms = $derived(roomList());
  const cameras = $derived(home.config?.favorites?.cameras ?? []);
  const wide = (c) => !compact && (c.kind === 'remote' || c.kind === 'cameras' || c.kind === 'room');
</script>

<div class="board" class:compact>
  {#each cards as card, i (i)}
    <div class="slot" class:wide={wide(card)} style="--i:{i}">
      {#if card.kind === 'device'}
        <SmartDevice id={card.id} title={card.title} />
      {:else if card.kind === 'room'}
        {@const room = rooms.find((r) => r.name === card.room)}
        {#if room}<RoomCard {room} />{/if}
      {:else if card.kind === 'energy'}
        <EnergyCard />
      {:else if card.kind === 'weather'}
        <WeatherCard />
      {:else if card.kind === 'remote'}
        <Remote tv={card.id} />
      {:else if card.kind === 'cameras'}
        <div class="cams">
          {#each cameras as cam (cam.id)}
            <CameraTile id={cam.id} name={cam.name} refresh={cam.refresh} battery={cam.battery} onopen={() => (location.hash = '#/cameras')} />
          {/each}
        </div>
      {:else if card.kind === 'automation'}
        <AutomationCard id={card.id} />
      {:else if card.kind === 'history'}
        <HistoryCard point={card.point} hours={card.hours ?? 24} title={card.title} />
      {/if}
    </div>
  {/each}
</div>

<style>
  .board {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(min(100%, 320px), 1fr));
    gap: 16px;
    align-items: start;
  }

  .board.compact {
    grid-template-columns: 1fr;
    gap: 12px;
  }

  .slot {
    min-width: 0;
    animation: rise 0.45s var(--ease) both;
    animation-delay: calc(var(--i) * 70ms);
  }

  .slot.wide {
    grid-column: span 2;
  }

  .cams {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(min(100%, 260px), 1fr));
    gap: 12px;
  }

  @keyframes rise {
    from {
      opacity: 0;
      transform: translateY(12px) scale(0.98);
    }
  }

  @media (max-width: 760px) {
    .slot.wide {
      grid-column: auto;
    }
  }
</style>
