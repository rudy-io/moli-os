<script module>
  /** The plan, kept between visits: the floors show at once the next time. */
  let cachedPlan = null;
</script>

<script>
  import { onMount } from 'svelte';
  import { roomList, roomNamed, savePref, protectedRoom } from '../lib/home.svelte.js';
  import Icon from '../ui/Icon.svelte';
  import RoomCard from '../ui/RoomCard.svelte';
  import CoverTile from '../ui/CoverTile.svelte';
  import FilterChips from '../ui/FilterChips.svelte';
  import { categoryOf, loadFilter } from '../lib/filters.js';
  import { floorsOf } from '../lib/floors.js';
  import { t } from '../../lib/i18n.svelte.js';

  let { route = 'pieces' } = $props();
  const coversOnly = $derived(route === 'pieces/volets');
  let plan = $state(cachedPlan);
  onMount(async () => {
    try {
      const res = await fetch('/api/plan');
      if (res.ok) plan = cachedPlan = await res.json();
    } catch {
      /* without the plan: one list, no floors */
    }
  });

  /** Which kinds of devices show (none chosen: all). */
  let filter = $state(loadFilter('pieces-filter'));
  /** Pads with their names or icons only: this device's choice. */
  let look = $state((() => {
    try {
      return localStorage.getItem('pieces-look') === 'icons' ? 'icons' : 'text';
    } catch {
      return 'text';
    }
  })());
  function choose(v) {
    look = v;
    savePref('pieces-look', v);
  }

  const all = $derived(roomList());
  const coverCount = $derived(all.reduce((count, room) => count + room.covers.length, 0));
  const devices = $derived(
    all.flatMap((r) => [r.group, ...r.lights, ...r.plugs, ...r.covers, ...r.climates, ...r.media, ...r.sensors].filter(Boolean)),
  );

  /** A room with only what the filter keeps; null when nothing is left. */
  function only(r) {
    // The direct link is a temporary view, independent of saved category choices.
    if (coversOnly) return r.covers.length ? { ...r, group: null, lights: [], plugs: [], climates: [], media: [], sensors: [], lit: 0 } : null;
    if (!filter.length) return r;
    const on = (id) => filter.includes(id);
    const f = {
      ...r,
      group: on('lumieres') ? r.group : null,
      lights: on('lumieres') ? r.lights : [],
      plugs: on('prises') ? r.plugs : [],
      covers: on('climat') ? r.covers : [],
      climates: on('climat') ? r.climates : [],
      media: on('medias') ? r.media : [],
      sensors: r.sensors.filter((d) => on(categoryOf(d))),
    };
    const any = f.group || f.lights.length || f.plugs.length || f.covers.length || f.climates.length || f.media.length || f.sensors.length;
    return any ? f : null;
  }

  const rooms = $derived(all.map(only).filter(Boolean));

  // Floor by floor (from all the rooms: a filter does not move one).
  const placed = $derived(floorsOf(all, plan, roomNamed));
  const floors = $derived(
    placed.sections
      .map((s) => ({ ...s, rooms: rooms.filter((r) => placed.of.get(r.name) === s.id) }))
      .filter((s) => s.rooms.length),
  );
  const litIn = (list) => list.reduce((n, r) => n + r.lit, 0);
</script>

<div class="pieces">
  <header>
    <h1 class="page-title">{t(coversOnly ? 'maison.pieces.volets' : 'maison.pieces.titre')}</h1>
    <p class="page-sub">{coversOnly ? t('maison.pieces.volets_compte', { count: coverCount }) : t('maison.pieces.sous_titre')}</p>
    {#if coversOnly}
      <a class="section-link" href="#/pieces">{t('maison.pieces.toutes')}</a>
    {:else if coverCount}
      <a class="section-link" href="#/pieces/volets"><Icon name="shutter" size={18} />{t('maison.pieces.voir_volets')}</a>
    {/if}
  </header>

  {#if !coversOnly}
  <div class="bar">
    <FilterChips {devices} bind:filter key="pieces-filter" />
    <div class="look" role="radiogroup" aria-label={t('maison.pieces.affichage')}>
      <button role="radio" aria-checked={look === 'text'} class:active={look === 'text'} onclick={() => choose('text')} title={t('maison.pieces.avec_noms')}>Aa</button>
      <button role="radio" aria-checked={look === 'icons'} class:active={look === 'icons'} onclick={() => choose('icons')} title={t('maison.pieces.icones')}><Icon name="light" size={15} /></button>
    </div>
  </div>
  {/if}

  {#if coversOnly && !coverCount}<p class="page-sub">{t('maison.pieces.volets_vides')}</p>{/if}

  {#each floors as floor (floor.id)}
    <section class="floor" aria-label={floor.name ?? t('maison.pieces.titre')}>
      {#if floors.length > 1 && floor.name}
        {@const lit = litIn(floor.rooms)}
        <h2 class="floor-title">
          <span>{floor.name}</span>
          {#if lit}<small>{t('maison.pieces.allumees', { count: lit })}</small>{/if}
        </h2>
      {/if}
      {#if coversOnly}
        <div class="cover-rooms">
          {#each floor.rooms as room (room.name)}
            <div class="cover-room">
              <header class="cover-heading">
                <svelte:element this={floors.length > 1 && floor.name ? 'h3' : 'h2'} class="cover-title">
                  <Icon name={room.icon} size={22} />
                  <span>{room.name}</span>
                  {#if protectedRoom(room.name)}<span title={t('commun.piece.protegee_titre')}><Icon name="lock" size={13} /></span>{/if}
                </svelte:element>
                <span class="muted">{t('maison.pieces.volets_piece', { count: room.covers.length })}</span>
              </header>
              <ul class="cover-grid" aria-label={room.name}>
                {#each room.covers as cover (cover.id)}
                  <li><CoverTile id={cover.id} standalone /></li>
                {/each}
              </ul>
            </div>
          {/each}
        </div>
      {:else}
        <div class="rooms">
          {#each floor.rooms as room (room.name)}
            <div class="slot"><RoomCard {room} every={filter.includes('capteurs')} icons={look === 'icons'} /></div>
          {/each}
        </div>
      {/if}
    </section>
  {/each}
</div>

<style>
  .section-link { display: inline-flex; align-items: center; gap: 8px; margin-top: 12px; color: var(--ink); font-size: 14px; text-underline-offset: 3px; }
  .pieces {
    display: grid;
    /* minmax(0, …): a plain column grows to the widest card and pushes the page
       sideways on a phone. */
    grid-template-columns: minmax(0, 1fr);
    gap: 22px;
  }

  .bar {
    display: flex;
    align-items: center;
    gap: 12px;
    flex-wrap: wrap;
  }

  .bar > :global(:first-child) {
    flex: 1;
    min-width: 0;
  }

  .look {
    display: inline-flex;
    padding: 3px;
    border-radius: 999px;
    background: var(--surface-2);
  }

  .look button {
    display: grid;
    place-items: center;
    min-width: 34px;
    height: 28px;
    border: none;
    border-radius: 999px;
    background: transparent;
    color: var(--ink-3);
    font: inherit;
    font-size: 13px;
    font-weight: 700;
    cursor: pointer;
  }

  .look button.active {
    background: var(--surface);
    color: var(--ink);
    box-shadow: 0 1px 3px rgb(0 0 0 / 15%);
  }

  .floor {
    display: grid;
    gap: 12px;
  }

  .floor-title {
    display: flex;
    align-items: baseline;
    gap: 10px;
    margin: 0;
    font-size: 13px;
    font-weight: 750;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--ink-2);
  }

  /* A thin rule after the name, to the edge. */
  .floor-title::after {
    content: '';
    flex: 1;
    align-self: center;
    height: 1px;
    background: color-mix(in srgb, var(--ink-3) 25%, transparent);
  }

  .floor-title small {
    font-size: 12px;
    font-weight: 600;
    letter-spacing: 0;
    text-transform: none;
    color: var(--warm-ink);
  }

  .rooms {
    columns: 3 340px;
    column-gap: 20px;
  }

  .cover-rooms { display: grid; gap: 24px; }
  .cover-room { min-width: 0; }
  .cover-heading { display: flex; align-items: baseline; flex-wrap: wrap; gap: 8px 14px; margin-bottom: 12px; }
  .cover-heading > .muted { font-size: 14px; }
  .cover-title { display: flex; align-items: center; gap: 8px; min-width: 0; margin: 0; font-size: 18px; font-weight: 700; line-height: 1.4; overflow-wrap: anywhere; }
  .cover-title > span { min-width: 0; }
  .cover-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(min(100%, 19rem), 1fr));
    align-items: start;
    gap: 12px;
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .cover-grid > li { min-width: 0; overflow-wrap: anywhere; }

  .slot {
    break-inside: avoid;
    margin-bottom: 20px;
  }
</style>
