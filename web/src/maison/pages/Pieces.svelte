<script module>
  /** The plan, kept between visits: the floors show at once the next time. */
  let cachedPlan = null;
</script>

<script>
  import { onMount } from 'svelte';
  import { roomList, roomNamed, savePref, protectedRoom, nameOf, value } from '../lib/home.svelte.js';
  import { isValve, climateKeys, canWrite } from '../lib/capabilities.js';
  import Thermometer from '../ui/Thermometer.svelte';
  import ClimateCard from '../ui/ClimateCard.svelte';
  import ValveCard from '../ui/ValveCard.svelte';
  import Icon from '../ui/Icon.svelte';
  import RoomCard from '../ui/RoomCard.svelte';
  import RoomScene from '../ui/RoomScene.svelte';
  import { photoOf, setPhoto, loadPhotos } from '../lib/photos.svelte.js';
  import CoverTile from '../ui/CoverTile.svelte';
  import FilterChips from '../ui/FilterChips.svelte';
  import { categoryOf, loadFilter } from '../lib/filters.js';
  import { floorsOf } from '../lib/floors.js';
  import { t } from '../../lib/i18n.svelte.js';

  let { route = 'pieces' } = $props();
  const coversOnly = $derived(route === 'pieces/volets');
  let plan = $state(cachedPlan);
  onMount(loadPhotos);
  /** A clim or a shutter tapped on the photo: its full control, below. */
  const openControl = () => document.querySelector('.stage-comfort, .stage-room')?.scrollIntoView({ behavior: 'smooth', block: 'start' });

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

  // One room at a time (a button per room), or the overview of them all.
  // The choice is this device's, kept for the next visit.
  const ALL = '__all';
  let pick = $state((() => {
    try {
      return localStorage.getItem('pieces-room');
    } catch {
      return null;
    }
  })());
  function choose_room(name) {
    pick = name;
    savePref('pieces-room', name);
  }
  const current = $derived(pick === ALL ? null : (rooms.find((r) => r.name === pick) ?? floors[0]?.rooms[0] ?? null));
  const overview = $derived(pick === ALL || !current);
  /** The room's climate devices that take a setpoint: one thermometer each. */
  const comfort = $derived(
    (current?.climates ?? []).filter((c) => value(c.id, climateKeys(c).target) != null || canWrite(c, climateKeys(c).target)),
  );
  /** What a room button says under its name: its lights on, else its temperature. */
  function roomTemp(r) {
    for (const c of [...r.climates, ...r.sensors]) {
      const v = value(c.id, climateKeys(c).temperature);
      if (typeof v === 'number') return v;
    }
    return null;
  }
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

  {#if !coversOnly && overview}
  <div class="bar">
    <FilterChips {devices} bind:filter key="pieces-filter" />
    <div class="look" role="radiogroup" aria-label={t('maison.pieces.affichage')}>
      <button role="radio" aria-checked={look === 'text'} class:active={look === 'text'} onclick={() => choose('text')} title={t('maison.pieces.avec_noms')}>Aa</button>
      <button role="radio" aria-checked={look === 'icons'} class:active={look === 'icons'} onclick={() => choose('icons')} title={t('maison.pieces.icones')}><Icon name="light" size={15} /></button>
    </div>
  </div>
  {/if}

  {#if coversOnly && !coverCount}<p class="page-sub">{t('maison.pieces.volets_vides')}</p>{/if}

  {#if !coversOnly}
    <nav class="picker" aria-label={t('maison.pieces.choisir')}>
      <button class="pick all" class:on={overview} aria-pressed={overview} onclick={() => choose_room(ALL)}>
        <span class="pick-icon"><Icon name="light-group" size={20} /></span>
        <span class="pick-name">{t('maison.pieces.toutes_pieces')}</span>
      </button>
      {#each floors as floor (floor.id)}
        {#if floors.length > 1 && floor.name}<span class="pick-floor">{floor.name}</span>{/if}
        {#each floor.rooms as r (r.name)}
          {@const temp = roomTemp(r)}
          <button class="pick" class:on={!overview && current?.name === r.name} class:lit={r.lit} aria-pressed={!overview && current?.name === r.name} onclick={() => choose_room(r.name)}>
            <span class="pick-icon"><Icon name={r.icon} size={20} /></span>
            <span class="pick-name">{r.name}</span>
            {#if r.lit}<span class="pick-badge">{r.lit}</span>{:else if temp != null}<span class="pick-temp num">{Math.round(temp)}°</span>{/if}
          </button>
        {/each}
      {/each}
    </nav>
  {/if}

  {#if !coversOnly && !overview && current}
    {#key current.name}
      <RoomScene room={current} photo={photoOf(current.name)} onphoto={(file) => setPhoto(current.name, file)} onopen={openControl} />
      <div class="stage" class:has-comfort={comfort.length}>
        <div class="stage-room"><RoomCard room={current} every={filter.includes('capteurs')} icons={look === 'icons'} /></div>
        {#if comfort.length}
          <div class="stage-comfort">
            {#each comfort as c (c.id)}
              <section class="card comfort">
                <div class="card-head"><h2><Icon name="thermometer" size={18} />{nameOf(c.id)}</h2></div>
                <div class="comfort-body">
                  <Thermometer id={c.id} name={nameOf(c.id)} />
                  <div class="comfort-card">
                    {#if isValve(c)}<ValveCard id={c.id} name={nameOf(c.id)} />{:else}<ClimateCard id={c.id} name={nameOf(c.id)} />{/if}
                  </div>
                </div>
              </section>
            {/each}
          </div>
        {/if}
      </div>
    {/key}
  {/if}

  {#each overview || coversOnly ? floors : [] as floor (floor.id)}
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

  /* The room buttons: one tap, one room. */
  .picker {
    display: flex;
    align-items: center;
    gap: 8px;
    overflow-x: auto;
    scrollbar-width: none;
    margin: -6px -36px -6px -14px;
    padding: 6px 36px 12px 14px;
    mask-image: linear-gradient(90deg, transparent, #000 14px, #000 calc(100% - 40px), transparent);
  }

  .pick {
    flex: none;
  }

  .pick-floor {
    flex: none;
    writing-mode: vertical-rl;
    transform: rotate(180deg);
    padding: 0 2px;
    font-size: 10.5px;
    font-weight: 750;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--ink-3);
  }

  .pick {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    min-height: 48px;
    padding: 6px 14px 6px 6px;
    border: 0;
    border-radius: 999px;
    background: var(--glass);
    box-shadow: var(--shadow), inset 0 0 0 1px var(--glass-line);
    color: var(--ink-2);
    font-weight: 650;
    font-size: 14.5px;
  }

  .pick:hover {
    transform: translateY(-2px);
    box-shadow: var(--shadow-lift), inset 0 0 0 1px var(--glass-line);
    color: var(--ink);
  }

  .pick-icon {
    width: 36px;
    height: 36px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    background: var(--surface-2);
    color: var(--ink-3);
    transition: background 0.25s, color 0.25s;
  }

  .pick.lit .pick-icon {
    background: var(--warm-soft);
    color: var(--warm-ink);
  }

  .pick.on {
    background: linear-gradient(135deg, var(--accent), var(--accent-2));
    color: #fff;
    box-shadow: 0 12px 24px -12px var(--accent-2);
  }

  .pick.on .pick-icon {
    background: rgb(255 255 255 / 25%);
    color: #fff;
  }

  .pick-badge {
    min-width: 22px;
    height: 22px;
    padding: 0 6px;
    border-radius: 999px;
    display: grid;
    place-items: center;
    font-size: 12px;
    font-weight: 750;
    background: var(--warm);
    color: #fff;
  }

  .pick-temp {
    font-size: 12.5px;
    color: var(--ink-3);
  }

  .pick.on .pick-temp {
    color: rgb(255 255 255 / 85%);
  }

  /* One room, big: its controls, and beside them its thermometers. */
  .stage {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 20px;
    align-items: start;
  }

  .stage.has-comfort {
    grid-template-columns: minmax(0, 1.1fr) minmax(0, 1fr);
  }

  .stage-room :global(.room) {
    font-size: 17px;
  }

  .stage-comfort {
    display: grid;
    gap: 20px;
  }

  .comfort-body {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr);
    gap: 18px;
    align-items: center;
  }

  .comfort-card :global(.card) {
    box-shadow: none;
    background: transparent;
    padding: 0;
    animation: none;
  }

  @media (max-width: 1100px) {
    .stage.has-comfort {
      grid-template-columns: minmax(0, 1fr);
    }
  }

  @media (max-width: 760px) {
    .picker {
      margin: -6px -16px;
      padding: 6px 16px 12px;
    }

    .comfort-body {
      grid-template-columns: minmax(0, 1fr);
      justify-items: center;
    }
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
