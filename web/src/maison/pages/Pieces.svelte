<script module>
  /** The plan, kept between visits: the floors show at once the next time. */
  let cachedPlan = null;
</script>

<script>
  import { onMount } from 'svelte';
  import { roomList, roomNamed, savePref } from '../lib/home.svelte.js';
  import Icon from '../ui/Icon.svelte';
  import RoomCard from '../ui/RoomCard.svelte';
  import FilterChips from '../ui/FilterChips.svelte';
  import HeatingAway from '../ui/HeatingAway.svelte';
  import HeatingNight from '../ui/HeatingNight.svelte';
  import VeluxPlans from '../ui/VeluxPlans.svelte';
  import { categoryOf, loadFilter } from '../lib/filters.js';
  import { floorsOf } from '../lib/floors.js';
  import { t } from '../../lib/i18n.svelte.js';

  /** `only`: one kind of device, the page then shows it alone, room by room
   *  (the Heating and Windows pages), with no chips to choose. `focus`: a
   *  device whose card opens, scrolled to (a stove's alarm, from home). */
  let { only = null, focus = null } = $props();

  // [title, subtitle, nothing to show] for each page.
  const TEXTS = {
    pieces: ['maison.pieces.titre', 'maison.pieces.sous_titre', null],
    chauffage: ['maison.chauffage.titre', 'maison.chauffage.sous_titre', 'maison.chauffage.vide'],
    fenetres: ['maison.fenetres.titre', 'maison.fenetres.sous_titre', 'maison.fenetres.vide'],
  };
  const [title, subtitle, empty] = $derived(TEXTS[only ?? 'pieces']);

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
  const kept = $derived(only ? [only] : filter);
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
  const devices = $derived(
    all.flatMap((r) => [r.group, ...r.lights, ...r.plugs, ...r.covers, ...r.climates, ...r.media, ...r.sensors].filter(Boolean)),
  );

  /** A room with only what the filter keeps; null when nothing is left.
   *  Its sensors still give its temperature (`probes`). */
  function trim(r) {
    if (!kept.length) return r;
    const on = (id) => kept.includes(id);
    const f = {
      ...r,
      probes: r.sensors,
      group: on('lumieres') ? r.group : null,
      lights: on('lumieres') ? r.lights : [],
      plugs: on('prises') ? r.plugs : [],
      covers: on('fenetres') ? r.covers : [],
      climates: on('chauffage') ? r.climates : [],
      media: on('medias') ? r.media : [],
      sensors: r.sensors.filter((d) => on(categoryOf(d))),
    };
    const any = f.group || f.lights.length || f.plugs.length || f.covers.length || f.climates.length || f.media.length || f.sensors.length;
    return any ? f : null;
  }

  const rooms = $derived(all.map(trim).filter(Boolean));

  // Floor by floor (from all the rooms: a filter does not move one).
  const placed = $derived(floorsOf(all, plan, roomNamed));
  const floors = $derived(
    placed.sections
      .map((s) => ({ ...s, rooms: rooms.filter((r) => placed.of.get(r.name) === s.id) }))
      .filter((s) => s.rooms.length),
  );
  const litIn = (list) => list.reduce((n, r) => n + r.lit, 0);
  // Names or icons on the lights' pads: no light, no choice to make.
  const hasLights = $derived(all.some((r) => r.group || r.lights.length));
</script>

<div class="pieces">
  <header>
    <h1 class="page-title">{t(title)}</h1>
    <p class="page-sub">{t(subtitle)}</p>
  </header>

  {#if !only}<div class="bar">
    <FilterChips {devices} bind:filter key="pieces-filter" />
    {#if hasLights}<div class="look" role="radiogroup" aria-label={t('maison.pieces.affichage')}>
      <button role="radio" aria-checked={look === 'text'} class:active={look === 'text'} onclick={() => choose('text')} title={t('maison.pieces.avec_noms')}>Aa</button>
      <button role="radio" aria-checked={look === 'icons'} class:active={look === 'icons'} onclick={() => choose('icons')} title={t('maison.pieces.icones')}><Icon name="light" size={15} /></button>
    </div>{/if}
  </div>{/if}

  {#if only === 'chauffage'}<HeatingAway /><HeatingNight />{/if}

  {#if empty && !rooms.length}<p class="empty">{t(empty)}</p>{/if}

  {#each floors as floor (floor.id)}
    <section class="floor" aria-label={floor.name ?? t('maison.pieces.titre')}>
      {#if floors.length > 1 && floor.name}
        <!-- Lights on, counted only where lights show. -->
        {@const lit = only ? 0 : litIn(floor.rooms)}
        <h2 class="floor-title">
          <span>{floor.name}</span>
          {#if lit}<small>{t('maison.pieces.allumees', { count: lit })}</small>{/if}
        </h2>
      {/if}
      <div class="rooms">
        {#each floor.rooms as room (room.name)}
          <div class="slot"><RoomCard {room} every={kept.includes('capteurs')} icons={look === 'icons'} {focus} /></div>
        {/each}
      </div>
    </section>
  {/each}

  <!-- The gateway's automations, after the rooms' windows. -->
  {#if only === 'fenetres'}<VeluxPlans />{/if}
</div>

<style>
  .pieces {
    display: grid;
    /* minmax(0, …): the filter chips, which never wrap, would widen the
       page past a phone's edge. */
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

  .empty {
    margin: 0;
    color: var(--ink-3);
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

  .slot {
    break-inside: avoid;
    margin-bottom: 20px;
  }
</style>
