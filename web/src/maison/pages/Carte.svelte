<script>
  import { onMount } from 'svelte';
  import L from 'leaflet';
  import 'leaflet/dist/leaflet.css';
  import { hub } from '../lib/home.svelte.js';
  import { t, locale } from '../../lib/i18n.svelte.js';
  import { people, loadPeople, saveZones, whereabouts, colorOf } from '../lib/people.svelte.js';
  import Icon from '../ui/Icon.svelte';

  /** Where everyone is (their phones), and the zones automations can use
   *  (« quand Élodie arrive au travail »). Map tiles: OpenStreetMap, loaded
   *  by this browser; positions never leave the house. An owner draws the
   *  zones: « Ajouter une zone », then a touch on the map. */
  let box = $state();
  let map;
  let layer;
  let placing = $state(false);
  let draft = $state(null); // { id?, name, latitude, longitude, radius }
  let error = $state('');
  let touched = false;
  let points = [];

  /** Everyone and every zone in view (until someone moves the map). */
  function frame() {
    if (!map || !points.length || box.clientWidth < 50) return;
    map.fitBounds(L.latLngBounds(points).pad(0.3), { maxZoom: 15 });
  }

  onMount(() => {
    map = L.map(box, { zoomControl: true, attributionControl: true }).setView([46.6, 2.4], 5);
    L.tileLayer('https://tile.openstreetmap.org/{z}/{x}/{y}.png', {
      maxZoom: 19,
      attribution: '© OpenStreetMap',
    }).addTo(map);
    layer = L.layerGroup().addTo(map);
    map.on('click', (e) => {
      if (!placing) return;
      placing = false;
      draft = { name: '', latitude: e.latlng.lat, longitude: e.latlng.lng, radius: 200 };
    });
    // The page settles after the map is made: it must know its real size,
    // or it frames too wide and leaves grey tiles.
    const resized = new ResizeObserver(() => {
      map.invalidateSize();
      if (!touched) frame();
    });
    resized.observe(box);
    // Once someone moves the map, it stays where they put it.
    for (const e of ['mousedown', 'wheel', 'touchstart']) box.addEventListener(e, () => (touched = true), { passive: true });
    loadPeople();
    return () => {
      resized.disconnect();
      map.remove();
    };
  });

  // Who is where, live.
  const located = $derived(
    people.list
      .map((p) => ({ person: p, at: whereabouts(p.id) }))
      .filter(({ at }) => at.latitude != null && at.longitude != null),
  );

  $effect(() => {
    if (!layer) return;
    // Depend on the live state and the zones.
    const zones = people.zones;
    const marks = located;
    const editing = draft;
    void hub.devices;
    layer.clearLayers();
    for (const z of zones) {
      L.circle([z.latitude, z.longitude], { radius: z.radius, color: '#4F8DF7', weight: 2, fillOpacity: 0.08 }).addTo(layer);
      // The name on the circle's top edge: never over the people inside.
      L.marker([z.latitude + z.radius / 111_320, z.longitude], {
        icon: L.divIcon({ className: 'zone-label', html: `<span>${escape(z.name)}</span>`, iconSize: null }),
        interactive: false,
      }).addTo(layer);
    }
    if (editing) {
      L.circle([editing.latitude, editing.longitude], { radius: editing.radius, color: '#E8B931', weight: 2, dashArray: '6 6', fillOpacity: 0.12 }).addTo(layer);
    }
    for (const { person, at } of marks) {
      const icon = L.divIcon({
        className: 'person-pin',
        html: `<span style="background:${escape(colorOf(person))}">${escape(person.name.slice(0, 1).toUpperCase())}</span>`,
        iconSize: [36, 36],
        iconAnchor: [18, 18],
      });
      // A name is text, never HTML.
      const name = document.createElement('span');
      name.textContent = person.name;
      L.marker([at.latitude, at.longitude], { icon }).bindTooltip(name).addTo(layer);
    }
    points = [...marks.map(({ at }) => [at.latitude, at.longitude]), ...zones.map((z) => [z.latitude, z.longitude])];
    if (!touched) frame();
  });

  const escape = (text) => text.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);

  function since(ms) {
    if (!ms) return '';
    return new Date(ms).toLocaleTimeString(locale(), { hour: '2-digit', minute: '2-digit' });
  }

  async function keep(e) {
    e.preventDefault();
    error = '';
    const id = draft.id ?? slug(draft.name);
    const others = people.zones.filter((z) => z.id !== id);
    try {
      await saveZones([...others, { id, name: draft.name.trim(), latitude: draft.latitude, longitude: draft.longitude, radius: Number(draft.radius) }]);
      draft = null;
    } catch (err) {
      error = err.message;
    }
  }

  async function drop(zone) {
    if (!confirm(t('personnes.carte_page.retirer_confirmer', { name: zone.name }))) return;
    try {
      await saveZones(people.zones.filter((z) => z.id !== zone.id));
    } catch (err) {
      error = err.message;
    }
  }

  function slug(name) {
    return (
      name
        .normalize('NFD')
        .replace(/[̀-ͯ]/g, '')
        .toLowerCase()
        .replace(/[^a-z0-9]+/g, '-')
        .replace(/^-|-$/g, '')
        .slice(0, 32) || `zone-${Date.now()}`
    );
  }
</script>

<div class="carte">
  <header>
    <h1 class="page-title">{t('personnes.carte_page.titre')}</h1>
    <p class="page-sub">{t('personnes.carte_page.sous_titre')}</p>
  </header>

  <div class="layout">
    <div class="map" class:placing bind:this={box}></div>

    <aside>
      <section class="card">
        <h2>{t('personnes.carte_page.qui')}</h2>
        <ul>
          {#each people.list as p (p.id)}
            {@const at = whereabouts(p.id)}
            <li>
              <span class="dot" style:background={colorOf(p)}>{p.name.slice(0, 1).toUpperCase()}</span>
              <div>
                <b>{p.name}</b>
                <span class="muted">
                  {at.home === true ? t('personnes.carte.a_la_maison') : (at.zone ?? t('personnes.carte.inconnu'))}
                  {#if at.since} · {t('personnes.carte_page.depuis', { heure: since(at.since) })}{/if}
                  {#if at.battery != null} · {Math.round(at.battery)} %{/if}
                </span>
              </div>
            </li>
          {:else}
            <li class="muted">{t('personnes.carte.personne')}</li>
          {/each}
        </ul>
      </section>

      <section class="card">
        <h2>{t('personnes.carte_page.zones')}</h2>
        <ul>
          {#each people.zones as z (z.id)}
            <li>
              <span class="zone"><Icon name="map" size={18} /></span>
              <div><b>{z.name}</b><span class="muted">{Math.round(z.radius)} m</span></div>
              {#if people.owner}
                <button class="ghost small" onclick={() => (draft = { ...z })}>{t('personnes.carte.modifier')}</button>
                <button class="ghost small danger" onclick={() => drop(z)} aria-label={t('personnes.carte.retirer')}><Icon name="trash" size={16} /></button>
              {/if}
            </li>
          {:else}
            <li class="muted">{t('personnes.carte_page.aucune_zone')}</li>
          {/each}
        </ul>
        {#if people.owner && !draft}
          <button class="primary" onclick={() => (placing = true)} disabled={placing}>
            <Icon name="plus" size={18} />{placing ? t('personnes.carte_page.touche_la_carte') : t('personnes.carte_page.ajouter')}
          </button>
        {/if}
        {#if draft}
          <form class="edit" onsubmit={keep}>
            <label><span>{t('personnes.carte_page.nom')}</span><input bind:value={draft.name} maxlength="40" required placeholder={t('personnes.carte_page.nom_exemple')} /></label>
            <label>
              <span>{t('personnes.carte_page.rayon', { m: Math.round(draft.radius) })}</span>
              <input type="range" min="50" max="5000" step="50" bind:value={draft.radius} />
            </label>
            <div class="row">
              <button class="ghost" type="button" onclick={() => (draft = null)}>{t('personnes.carte.annuler')}</button>
              <button class="primary" type="submit" disabled={!draft.name.trim()}>{t('personnes.carte.enregistrer')}</button>
            </div>
          </form>
        {/if}
        {#if error}<p class="error" role="alert">{error}</p>{/if}
        <p class="muted hint">{t('personnes.carte_page.automatismes')}</p>
      </section>
    </aside>
  </div>
</div>

<style>
  .carte {
    display: grid;
    gap: 18px;
  }

  .layout {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 320px;
    gap: 16px;
    align-items: start;
  }

  @media (max-width: 900px) {
    .layout {
      grid-template-columns: minmax(0, 1fr);
    }
  }

  .map {
    height: min(70vh, 640px);
    min-height: 320px;
    border-radius: var(--r-xl);
    overflow: hidden;
    box-shadow: var(--shadow);
    z-index: 0;
  }

  .map.placing {
    cursor: crosshair;
  }

  aside {
    display: grid;
    gap: 16px;
  }

  .card {
    display: grid;
    gap: 12px;
  }

  h2 {
    font-size: 16px;
    font-weight: 700;
  }

  ul {
    display: grid;
    gap: 8px;
    margin: 0;
    padding: 0;
    list-style: none;
  }

  li {
    display: flex;
    align-items: center;
    gap: 10px;
    font-size: 14px;
  }

  li > div {
    display: grid;
    flex: 1;
  }

  .dot,
  .zone {
    display: grid;
    place-items: center;
    width: 32px;
    height: 32px;
    border-radius: 50%;
    flex: none;
    color: #fff;
    font-weight: 750;
  }

  .zone {
    background: var(--cool-soft);
    color: var(--cool);
  }

  .edit {
    display: grid;
    gap: 10px;
  }

  label {
    display: grid;
    gap: 6px;
    font-size: 14px;
    font-weight: 650;
  }

  input:not([type='range']) {
    min-height: 40px;
    padding: 0 12px;
    border: 1px solid var(--surface-3);
    border-radius: 12px;
    background: var(--surface-2);
    color: var(--ink);
    font: inherit;
  }

  .row {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }

  .primary,
  .ghost {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 6px;
    min-height: 40px;
    padding: 0 16px;
    border-radius: 999px;
    font: inherit;
    font-weight: 650;
    cursor: pointer;
  }

  .primary {
    border: none;
    background: var(--cool);
    color: #fff;
  }

  .ghost {
    border: 1px solid var(--surface-3);
    background: var(--surface);
    color: var(--ink);
  }

  .small {
    min-height: 32px;
    padding: 0 10px;
    font-size: 13px;
  }

  .danger {
    color: var(--alert);
  }

  button:disabled {
    opacity: 0.6;
  }

  .error {
    color: var(--alert);
    font-weight: 600;
    font-size: 14px;
  }

  .hint {
    font-size: 13px;
  }

  :global(.person-pin span) {
    display: grid;
    place-items: center;
    width: 36px;
    height: 36px;
    border: 3px solid #fff;
    border-radius: 50%;
    color: #fff;
    font: 750 15px/1 'Figtree Variable', sans-serif;
    box-shadow: 0 2px 8px rgb(0 0 0 / 0.3);
  }

  :global(.zone-label span) {
    display: inline-block;
    transform: translate(-50%, -110%);
    padding: 2px 8px;
    border-radius: 999px;
    background: rgb(255 255 255 / 0.9);
    color: #1d2a44;
    font: 650 12px/1.4 'Figtree Variable', sans-serif;
    white-space: nowrap;
    box-shadow: 0 1px 4px rgb(0 0 0 / 0.2);
  }
</style>
