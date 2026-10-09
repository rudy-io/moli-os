<script>
  import { onMount } from 'svelte';
  import Icon from '../ui/Icon.svelte';
  import { hub, value, nameOf, deviceKind, isOn, toggle, num, note, roomOf, savePref, doorOpen, leak, motion, isGroup, isFixture, unpowered } from '../lib/home.svelte.js';
  import { asHuman, CANCELLED } from '../lib/auto.svelte.js';
  import Drawing from '../plan/Drawing.svelte';
  import House3D from '../plan/House3D.svelte';
  import { SKINS, SKIN_IDS, ITEMS } from '../plan/skins.js';
  import { spread } from '../plan/geometry.js';
  import FilterChips from '../ui/FilterChips.svelte';
  import { keeps, loadFilter } from '../lib/filters.js';
  import { t, locale } from '../../lib/i18n.svelte.js';

  /** The house on its own plans: each device where it really is, alive. */
  let plan = $state({ floors: [] });
  let loaded = $state(false);
  let floorId = $state(null);
  /** 2D, on arriving: every floor side by side (until one is chosen). */
  let whole = $state(true);
  let ow = $state(0);
  let ih = $state(800);
  let editing = $state(false);
  let draft = $state(null);
  /** Edit: the device waiting for a tap on the plan. */
  let picking = $state(null);
  let search = $state('');
  /** View: the device whose card is open. */
  let opened = $state(null);
  let busy = $state(false);
  let newName = $state('');
  let surface = $state(null);
  let drawing = $state(null);
  let dragging = null;
  /** Edit, drawn floor: the fixture chosen, and the kind to add. */
  let selected = $state(null);
  let addKind = $state('bed');

  /** This viewer's skin (the house's default otherwise). */
  function readSkin() {
    try {
      return localStorage.getItem('plan-skin');
    } catch {
      return null;
    }
  }
  let mySkin = $state(readSkin());
  /** Which kinds of devices show (none chosen: all). Edit mode shows all. */
  let filter = $state(loadFilter('plan-filter'));
  // A Hue room is not a lamp, a fixture's bulbs and relay are the fixture:
  // none of them gets its own pin (D12).
  const inFixture = $derived(new Set(Object.values(hub.devices).filter(isFixture).flatMap((d) => d.members ?? [])));
  const pinnable = (d) => !isGroup(d) && !inFixture.has(d.id);
  const keep = (d) => pinnable(d) && (editing || keeps(filter, d));
  /** 2D or 3D, this viewer's choice. */
  let view = $state((() => {
    try {
      return localStorage.getItem('plan-view') === '3d' ? '3d' : '2d';
    } catch {
      return '2d';
    }
  })());

  function chooseView(v) {
    view = v;
    savePref('plan-view', v);
  }

  /** A tap on a device in 3D: lights switch, the rest says what it knows. */
  function tap3d(id) {
    const d = hub.devices[id];
    if (!d) return;
    if (['light', 'group', 'plug'].includes(deviceKind(d))) {
      if (!switchOf(d)) toggle(id);
      return;
    }
    const row = details(id)[0];
    const v = row ? (typeof row.v === 'number' ? num(row.v, 1) : row.v === true ? t('maison.plan.oui') : row.v === false ? t('maison.plan.non') : row.v) : null;
    note(row ? t('maison.plan.lecture', { name: nameOf(id), label: row.label, value: `${v}${row.unit ? ` ${row.unit}` : ''}` }) : nameOf(id));
  }

  const headers = { 'content-type': 'application/json', 'x-moli-origin': 'ui' };

  onMount(load);

  async function load() {
    try {
      const res = await fetch('/api/plan');
      plan = res.ok ? await res.json() : { floors: [] };
    } catch {
      plan = { floors: [] };
    }
    plan.floors ??= [];
    floorId = plan.floors.find((f) => f.id === floorId)?.id ?? plan.floors[0]?.id ?? null;
    loaded = true;
  }

  const shown = $derived(editing ? draft : plan);
  const floor = $derived(shown?.floors.find((f) => f.id === floorId) ?? shown?.floors[0] ?? null);
  const drawn = $derived((shown?.floors ?? []).some((f) => f.size));
  // The overview: the drawn floors at one scale, side by side, each at its
  // height on the first (the floor above lines up with the house below).
  const stack = $derived((shown?.floors ?? []).filter((f) => f.size));
  const overview = $derived(whole && view === '2d' && !editing && stack.length > 1);
  const GAP = 18;
  const scale = $derived.by(() => {
    const width = stack.reduce((t, f) => t + f.size[0], 0);
    const height = Math.max(...stack.map((f) => (f.offset?.[1] ?? 0) + f.size[1]));
    return Math.max(0.05, Math.min((ow - GAP * (stack.length - 1)) / width, Math.max(260, ih - 300) / height));
  });

  function pickFloor(id) {
    whole = false;
    floorId = id;
  }
  const in3d = $derived(view === '3d' && !editing && drawn && floor?.size);
  const skinId = $derived(editing ? (draft?.skin ?? 'plan') : SKINS[mySkin] ? mySkin : (plan.skin ?? 'plan'));
  const skin = $derived(SKINS[skinId] ?? SKINS.plan);

  function chooseSkin(id) {
    if (editing) {
      draft.skin = id;
      return;
    }
    mySkin = id;
    savePref('plan-skin', id);
  }
  const placed = $derived(new Set((shown?.floors ?? []).flatMap((f) => f.devices.map((d) => d.id))));
  const unplaced = $derived(
    Object.values(hub.devices)
      .filter((d) => !placed.has(d.id) && d.model !== 'Aide' && pinnable(d))
      .filter((d) => {
        const q = search.trim().toLowerCase();
        return !q || `${nameOf(d.id)} ${roomOf(d) ?? ''}`.toLowerCase().includes(q);
      })
      // By room, the devices in no room (weather, tariffs…) last.
      .sort(
        (a, b) =>
          Number(!roomOf(a)) - Number(!roomOf(b)) ||
          (roomOf(a) ?? '').localeCompare(roomOf(b) ?? '', locale()) ||
          nameOf(a.id).localeCompare(nameOf(b.id), locale()),
      ),
  );

  // ---- what a token shows -------------------------------------------------------

  const ICON = { light: 'light', group: 'light-group', plug: 'power', climate: 'air-conditioner', camera: 'cctv', speaker: 'speaker', tv: 'tv', cover: 'shutter' };
  const has = (d, key) => d.points.some((p) => p.key === key);

  function iconOf(d) {
    const kind = deviceKind(d);
    if (ICON[kind]) return ICON[kind];
    if (has(d, 'smoke')) return 'fire';
    if (has(d, 'water_leak') || has(d, 'watersensor_state')) return 'water';
    if (has(d, 'contact') || has(d, 'doorcontact_state')) return 'door';
    if (has(d, 'occupancy') || has(d, 'motion') || has(d, 'pir')) return 'motion';
    if (has(d, 'temperature') || has(d, 'temp_current')) return 'thermometer';
    return 'info';
  }

  /** A short reading next to the token (sensors). */
  function reading(d) {
    const t = value(d.id, 'temperature') ?? value(d.id, 'temp_current');
    if (typeof t === 'number' && deviceKind(d) !== 'climate' && !floor?.size) return `${num(t, 1)}°`;
    const p = value(d.id, 'power') ?? value(d.id, 'cur_power');
    if (typeof p === 'number' && p >= 1) return `${num(p)} W`;
    return null;
  }

  function alarming(d) {
    return value(d.id, 'smoke') === true || leak(d.id);
  }

  function opening(d) {
    return doorOpen(d.id) === true || motion(d.id);
  }

  function lit(d) {
    const kind = deviceKind(d);
    return ['light', 'group', 'plug', 'tv', 'speaker'].includes(kind) && isOn(d.id);
  }

  // ---- view ------------------------------------------------------------------------

  /** A lamp cut at the wall: only the switch brings it back (D12). */
  function switchOf(d) {
    if (!unpowered(d.id) || isFixture(d)) return false;
    note(t('maison.plan.coupee_mur', { name: nameOf(d.id) }));
    return true;
  }

  function tap(d) {
    if (editing) return;
    const kind = deviceKind(d);
    if (['light', 'group', 'plug'].includes(kind)) {
      if (!switchOf(d)) toggle(d.id);
      return;
    }
    if (kind === 'camera') location.hash = '#/cameras';
    else if (kind === 'tv' || kind === 'speaker') location.hash = '#/salon';
    else opened = opened === d.id ? null : d.id;
  }

  function details(id) {
    const d = hub.devices[id];
    if (!d) return [];
    return d.points
      .filter((p) => p.access?.read !== false)
      .map((p) => ({ label: p.label, v: value(id, p.key), unit: p.unit }))
      .filter((x) => x.v != null && x.v !== '')
      .slice(0, 6);
  }

  // ---- edit ------------------------------------------------------------------------

  function startEdit() {
    draft = structuredClone($state.snapshot(plan));
    editing = true;
    opened = null;
  }

  function cancelEdit() {
    editing = false;
    draft = null;
    picking = null;
    floorId = plan.floors.find((f) => f.id === floorId)?.id ?? plan.floors[0]?.id ?? null;
  }

  function slug(name) {
    const base = name
      .normalize('NFD')
      .replace(/[̀-ͯ]/g, '')
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, '-')
      .replace(/^-|-$/g, '')
      .slice(0, 24) || 'etage';
    let id = base;
    for (let n = 2; draft.floors.some((f) => f.id === id); n++) id = `${base}-${n}`;
    return id;
  }

  async function uploadImage(file) {
    const res = await fetch('/api/plan/images', {
      method: 'POST',
      headers: { 'content-type': file.type || 'application/octet-stream', 'x-moli-origin': 'ui' },
      body: file,
    });
    const body = await res.json().catch(() => ({}));
    if (!res.ok) {
      const err = new Error(body.error ?? res.statusText);
      err.status = res.status;
      throw err;
    }
    return body.image;
  }

  async function addFloor(event) {
    const file = event.currentTarget.files?.[0];
    event.currentTarget.value = '';
    const name = newName.trim() || t('maison.plan.etage_defaut', { n: draft.floors.length + 1 });
    try {
      busy = true;
      const image = file ? await asHuman(t('maison.plan.ajouter_plan', { name }), () => uploadImage(file)) : null;
      const id = slug(name);
      draft.floors.push({ id, name, ...(image ? { image } : {}), devices: [] });
      floorId = id;
      newName = '';
    } catch (err) {
      if (err.message !== CANCELLED) note(err.message, 'error');
    } finally {
      busy = false;
    }
  }

  /** A floor drawn on a grid, until its plan's image comes. */
  function addBlankFloor() {
    const name = newName.trim() || t('maison.plan.etage_defaut', { n: draft.floors.length + 1 });
    const id = slug(name);
    draft.floors.push({ id, name, devices: [] });
    floorId = id;
    newName = '';
  }

  async function replaceImage(event) {
    const file = event.currentTarget.files?.[0];
    event.currentTarget.value = '';
    if (!file || !floor) return;
    try {
      busy = true;
      floor.image = await asHuman(t('maison.plan.changer_plan', { name: floor.name }), () => uploadImage(file));
    } catch (err) {
      if (err.message !== CANCELLED) note(err.message, 'error');
    } finally {
      busy = false;
    }
  }

  function removeFloor() {
    if (!floor || !confirm(t('maison.plan.retirer_confirme', { name: floor.name }))) return;
    draft.floors = draft.floors.filter((f) => f.id !== floor.id);
    floorId = draft.floors[0]?.id ?? null;
  }

  /** Where a pointer is on the plan: cm on a drawn floor, 0–1 on an image. */
  function at(event) {
    if (floor?.size && drawing) {
      const p = drawing.toWorld(event.clientX, event.clientY);
      const [w, h] = floor.size;
      return { x: Math.round(Math.min(w, Math.max(0, p.x))), y: Math.round(Math.min(h, Math.max(0, p.y))) };
    }
    const box = surface.getBoundingClientRect();
    const clamp = (v) => Math.min(1, Math.max(0, Math.round(v * 10000) / 10000));
    return { x: clamp((event.clientX - box.left) / box.width), y: clamp((event.clientY - box.top) / box.height) };
  }

  function placeHere(event) {
    if (!editing || !picking || !floor) return;
    floor.devices.push({ id: picking, ...at(event) });
    picking = null;
  }

  function grab(event, spot) {
    if (!editing) return;
    event.preventDefault();
    // Not a pan of the drawing: this device moves.
    event.stopPropagation();
    event.currentTarget.setPointerCapture(event.pointerId);
    dragging = spot;
  }

  function drag(event) {
    if (!dragging) return;
    Object.assign(dragging, at(event));
  }

  function drop() {
    dragging = null;
  }

  function unplace(spot) {
    floor.devices = floor.devices.filter((d) => d !== spot);
  }

  function placeAt(x, y) {
    if (!editing || !picking || !floor) return;
    floor.devices.push({ id: picking, x, y });
    picking = null;
  }

  // ---- drawn floor: fixtures and placing by room ---------------------------------

  function addItem() {
    const kind = ITEMS[addKind];
    const c = drawing.center();
    const item = { kind: addKind, x: Math.round(c.x - kind.w / 2), y: Math.round(c.y - kind.h / 2), w: kind.w, h: kind.h };
    floor.items = [...(floor.items ?? []), item];
    selected = floor.items[floor.items.length - 1];
  }

  function turnItem() {
    if (selected) selected.r = ((selected.r ?? 0) + 90) % 360;
  }

  function removeItem() {
    if (!selected) return;
    floor.items = floor.items.filter((it) => it !== selected);
    selected = null;
  }

  /** Every device not on the plan yet goes into its room, if this floor has it. */
  function placeByRoom() {
    let placedNow = 0;
    for (const r of floor.rooms ?? []) {
      if (!r.room) continue;
      const mine = unplaced.filter((d) => roomOf(d) === r.room);
      spread(r.poly, mine.length).forEach(([x, y], i) => {
        floor.devices.push({ id: mine[i].id, x, y });
        placedNow++;
      });
    }
    note(placedNow ? t('maison.plan.poses', { count: placedNow }) : t('maison.plan.rien_a_poser'));
  }

  async function save() {
    busy = true;
    try {
      const saved = await asHuman(t('maison.plan.enregistrer_plan'), async () => {
        const res = await fetch('/api/plan', { method: 'PUT', headers, body: JSON.stringify($state.snapshot(draft)) });
        const body = await res.json().catch(() => ({}));
        if (!res.ok) {
          const err = new Error(body.error ?? res.statusText);
          err.status = res.status;
          throw err;
        }
        return body;
      });
      plan = saved;
      editing = false;
      draft = null;
      picking = null;
      note(t('maison.plan.enregistre'));
    } catch (err) {
      if (err.message !== CANCELLED) note(err.message, 'error');
    } finally {
      busy = false;
    }
  }
</script>

{#snippet spotBody(spot, d)}
  <button
    class="token"
    class:lit={lit(d)}
    class:alarm={alarming(d)}
    class:open={opening(d)}
    class:off={d.online === false}
    onclick={(e) => { e.stopPropagation(); tap(d); }}
    onpointerdown={(e) => grab(e, spot)}
    aria-label={lit(d) ? t('maison.plan.jeton_allume', { name: nameOf(d.id) }) : nameOf(d.id)}
    title={nameOf(d.id)}
  >
    <Icon name={iconOf(d)} size={18} />
  </button>
  {#if reading(d)}<span class="reading">{reading(d)}</span>{/if}
  <span class="name">{nameOf(d.id)}</span>
  {#if editing}
    <button class="unplace" onclick={(e) => { e.stopPropagation(); unplace(spot); }} aria-label={t('maison.plan.retirer_du_plan', { name: nameOf(d.id) })}><Icon name="close" size={12} /></button>
  {/if}
  {#if opened === d.id}
    <div class="card" role="dialog" aria-label={nameOf(d.id)}>
      <b>{nameOf(d.id)}</b>
      {#if roomOf(d)}<small class="muted">{roomOf(d)}</small>{/if}
      {#each details(d.id) as row (row.label)}
        <p><span>{row.label}</span><b>{typeof row.v === 'number' ? num(row.v, 1) : row.v === true ? t('maison.plan.oui') : row.v === false ? t('maison.plan.non') : row.v}{row.unit ? ` ${row.unit}` : ''}</b></p>
      {/each}
    </div>
  {/if}
{/snippet}

<svelte:window bind:innerHeight={ih} />

<div class="plan" data-unsaved={editing || undefined}>
  <header>
    <div>
      <h1 class="page-title">{t('maison.plan.titre')}</h1>
      <p class="page-sub">{editing ? t('maison.plan.sous_titre_edition') : t('maison.plan.sous_titre')}</p>
    </div>
    <div class="actions">
      {#if editing}
        <button class="ghost" onclick={cancelEdit} disabled={busy}>{t('commun.annuler')}</button>
        <button class="primary" onclick={save} disabled={busy}><Icon name="lock" size={16} />{t('maison.plan.enregistrer')}</button>
      {:else if loaded && plan.floors.length}
        <button class="ghost" onclick={startEdit}><Icon name="tune" size={16} />{t('maison.plan.modifier')}</button>
      {/if}
    </div>
  </header>

  {#if shown?.floors.length}
    <div class="floors" role="tablist">
      {#if stack.length > 1 && view === '2d' && !editing}
        <button role="tab" aria-selected={overview} class:active={overview} onclick={() => (whole = true)}>{t('maison.plan.toute_maison')}</button>
      {/if}
      {#each shown.floors as f (f.id)}
        <button role="tab" aria-selected={!overview && floor?.id === f.id} class:active={!overview && floor?.id === f.id} onclick={() => pickFloor(f.id)}>{f.name}</button>
      {/each}
    </div>
  {/if}

  {#if floor && !editing}
    <FilterChips devices={(overview ? stack : [floor]).flatMap((f) => f.devices).map((s) => hub.devices[s.id]).filter(Boolean)} bind:filter key="plan-filter" />
  {/if}

  {#if floor?.size}
    <div class="skins" role="radiogroup" aria-label={t('maison.plan.habillage_plan')}>
      {#if !editing}
        <span class="views">
          <button class:active={view === '2d'} onclick={() => chooseView('2d')} aria-pressed={view === '2d'}>2D</button>
          <button class:active={view === '3d'} onclick={() => chooseView('3d')} aria-pressed={view === '3d'}>3D</button>
        </span>
      {/if}
      <span class="muted small">{editing ? t('maison.plan.habillage_maison') : t('maison.plan.habillage')}</span>
      {#each SKIN_IDS as id (id)}
        <button role="radio" aria-checked={skinId === id} class:active={skinId === id} onclick={() => chooseSkin(id)}>
          <i style="background:{SKINS[id].bg}; border-color:{SKINS[id].wall}"></i>{SKINS[id].name}
        </button>
      {/each}
    </div>
  {/if}

  {#if editing && floor?.size}
    <div class="tools">
      <label class="pick">
        <span class="muted small">{t('maison.plan.ajouter')}</span>
        <select bind:value={addKind} aria-label={t('maison.plan.element_a_ajouter')}>
          {#each Object.entries(ITEMS) as [id, it] (id)}<option value={id}>{it.name}</option>{/each}
        </select>
        <button class="link" onclick={addItem}><Icon name="plus" size={15} />{t('maison.plan.ajouter')}</button>
      </label>
      {#if selected}
        <button class="link" onclick={turnItem}>{t('maison.plan.tourner')}</button>
        <button class="link danger" onclick={removeItem}>{t('maison.plan.retirer_element')}</button>
      {/if}
      <button class="link" onclick={placeByRoom}>{t('maison.plan.poser_par_piece')}</button>
    </div>
  {/if}

  {#if editing}
    <div class="tools">
      <label class="add">
        <input placeholder={t('maison.plan.nom_etage_exemple')} bind:value={newName} aria-label={t('maison.plan.nom_nouvel_etage')} />
        <span class="file"><Icon name="plus" size={16} />{t('maison.plan.ajouter_etage')}
          <input type="file" accept="image/png,image/jpeg,image/webp,image/svg+xml" onchange={addFloor} disabled={busy} />
        </span>
        <button class="link" onclick={addBlankFloor} disabled={busy}>{t('maison.plan.sans_image')}</button>
      </label>
      {#if floor && !floor.size}
        <span class="file small"><Icon name="camera" size={15} />{t('maison.plan.changer_image')}
          <input type="file" accept="image/png,image/jpeg,image/webp,image/svg+xml" onchange={replaceImage} disabled={busy} />
        </span>
        <button class="link danger" onclick={removeFloor}>{t('maison.plan.retirer_etage')}</button>
      {/if}
    </div>
  {/if}

  {#if !loaded}
    <p class="muted">…</p>
  {:else if !floor}
    <section class="empty">
      <Icon name="home-roof" size={40} />
      <h2>{t('maison.plan.vide_titre')}</h2>
      <p>{t('maison.plan.vide_texte')}</p>
      {#if !editing}<button class="primary" onclick={startEdit}><Icon name="plus" size={16} />{t('maison.plan.creer')}</button>{/if}
    </section>
  {:else}
    <div class="board" class:editing>
      {#if overview}
        <div class="whole" bind:clientWidth={ow} style="--gap:{GAP}px">
          {#each stack as f (f.id)}
            <div class="storey" style="width:{f.size[0] * scale}px">
              <button class="storey-name" onclick={() => pickFloor(f.id)}>{f.name}<Icon name="arrow-top-right" size={14} /></button>
              <div style="margin-top:{(f.offset?.[1] ?? 0) * scale}px; height:{f.size[1] * scale}px">
                <Drawing floor={f} {skin} {keep} fill ontap={() => (opened = null)} token={spotBody} />
              </div>
            </div>
          {/each}
        </div>
      {:else if in3d}
        <House3D plan={shown} floorId={floor.id} {skin} {keep} ontap={tap3d} />
      {:else if floor.size}
        <div class="drawn" onpointermove={drag} onpointerup={drop} onpointercancel={drop} role="presentation">
          <Drawing bind:this={drawing} {floor} {skin} {editing} {picking} {keep} bind:selected onplace={placeAt} ontap={() => (opened = null)} token={spotBody} />
        </div>
      {:else}
      <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
      <div class="surface" class:picking={!!picking} class:blank={!floor.image} bind:this={surface} onclick={placeHere} onpointermove={drag} onpointerup={drop} onpointercancel={drop}>
        {#if floor.image}
          <img src="/api/plan/images/{floor.image}" alt={t('maison.plan.nom_etage', { name: floor.name })} draggable="false" />
        {/if}
        {#each floor.devices as spot (spot.id)}
          {@const d = hub.devices[spot.id]}
          {#if d && keep(d)}
            <div class="spot" style="left:{spot.x * 100}%; top:{spot.y * 100}%">
              {@render spotBody(spot, d)}
            </div>
          {/if}
        {/each}
      </div>
      {/if}

      {#if editing}
        <aside class="tray">
          <input placeholder={t('maison.plan.chercher')} bind:value={search} aria-label={t('maison.plan.chercher_aria')} />
          <p class="muted small">{t('maison.plan.a_placer', { n: unplaced.length })}{picking ? ` · ${t('maison.plan.toucher_pour_poser', { name: nameOf(picking) })}` : ''}</p>
          <ul>
            {#each unplaced as d (d.id)}
              <li>
                <button class:chosen={picking === d.id} onclick={() => (picking = picking === d.id ? null : d.id)}>
                  <Icon name={iconOf(d)} size={16} /><span>{nameOf(d.id)}</span>{#if roomOf(d)}<small>{roomOf(d)}</small>{/if}
                </button>
              </li>
            {/each}
          </ul>
        </aside>
      {/if}
    </div>
  {/if}
</div>

<style>
  .plan {
    display: grid;
    gap: 18px;
  }

  header {
    display: flex;
    flex-wrap: wrap;
    align-items: flex-end;
    justify-content: space-between;
    gap: 12px;
  }

  .actions {
    display: flex;
    gap: 8px;
  }

  button.primary,
  button.ghost {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 9px 16px;
    border-radius: 999px;
    border: none;
    font: inherit;
    font-weight: 600;
    cursor: pointer;
  }

  button.primary {
    background: var(--ink);
    color: var(--surface);
  }

  button.ghost {
    background: var(--surface);
    color: var(--ink);
    box-shadow: var(--shadow);
  }

  .floors {
    display: flex;
    gap: 6px;
    overflow-x: auto;
  }

  .floors button {
    padding: 7px 14px;
    border-radius: 999px;
    border: none;
    background: var(--surface-2);
    color: var(--ink-2);
    font: inherit;
    font-weight: 600;
    cursor: pointer;
    white-space: nowrap;
  }

  .floors button.active {
    background: var(--ink);
    color: var(--surface);
  }

  .tools {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 10px 16px;
  }

  .add {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    align-items: center;
  }

  .add input,
  .tray input {
    padding: 9px 12px;
    border-radius: var(--r-sm);
    border: 1px solid var(--line);
    background: var(--surface);
    color: var(--ink);
    font: inherit;
    min-width: 0;
  }

  .file {
    position: relative;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 9px 14px;
    border-radius: 999px;
    background: var(--warm-soft);
    color: var(--warm-ink);
    font-weight: 600;
    cursor: pointer;
  }

  .file.small {
    padding: 6px 12px;
    font-size: 13px;
  }

  .file input {
    position: absolute;
    inset: 0;
    opacity: 0;
    cursor: pointer;
  }

  .link {
    border: none;
    background: none;
    font: inherit;
    cursor: pointer;
    text-decoration: underline;
  }

  .link.danger {
    color: var(--alert);
  }

  .empty {
    display: grid;
    justify-items: center;
    gap: 10px;
    padding: 40px 20px;
    border-radius: var(--r-lg);
    background: var(--surface);
    box-shadow: var(--shadow);
    text-align: center;
    color: var(--ink-2);
  }

  .empty h2 {
    margin: 0;
    color: var(--ink);
  }

  .empty p {
    max-width: 46ch;
    margin: 0 0 6px;
  }

  .board {
    display: grid;
    gap: 16px;
  }

  .board.editing {
    grid-template-columns: minmax(0, 1fr) 280px;
    align-items: start;
  }

  .skins {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
  }

  .skins button {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 6px 12px;
    border-radius: 999px;
    border: 1px solid var(--line);
    background: var(--surface);
    color: var(--ink-2);
    font: inherit;
    font-size: 13px;
    cursor: pointer;
  }

  .skins button.active {
    border-color: var(--ink);
    color: var(--ink);
    font-weight: 650;
  }

  .views {
    display: inline-flex;
    margin-right: 6px;
    padding: 3px;
    border-radius: 999px;
    background: var(--surface);
    box-shadow: var(--shadow);
  }

  .views button {
    padding: 5px 12px;
    border: none;
    border-radius: 999px;
    background: none;
    color: var(--ink-2);
    font: inherit;
    font-size: 13px;
    font-weight: 700;
    cursor: pointer;
  }

  .views button.active {
    background: var(--ink);
    color: var(--surface);
  }

  .skins i {
    width: 14px;
    height: 14px;
    border-radius: 50%;
    border: 2px solid;
  }

  .pick {
    display: inline-flex;
    align-items: center;
    gap: 8px;
  }

  .pick select {
    padding: 6px 8px;
    border-radius: var(--r-sm);
    border: 1px solid var(--line);
    background: var(--surface);
    color: var(--ink);
    font: inherit;
  }

  .whole {
    display: flex;
    gap: var(--gap);
    align-items: flex-start;
    justify-content: center;
  }

  .storey {
    flex: none;
    display: grid;
    gap: 6px;
  }

  .storey-name {
    all: unset;
    display: inline-flex;
    align-items: center;
    gap: 4px;
    justify-self: start;
    font-size: 13px;
    font-weight: 650;
    color: var(--ink-2);
    cursor: pointer;
  }

  .drawn {
    min-width: 0;
  }

  .surface {
    position: relative;
    border-radius: var(--r-lg);
    background: var(--surface);
    box-shadow: var(--shadow);
    overflow: hidden;
    touch-action: none;
    user-select: none;
  }

  .surface.blank {
    aspect-ratio: 4 / 3;
    background-image:
      linear-gradient(var(--line) 1px, transparent 1px),
      linear-gradient(90deg, var(--line) 1px, transparent 1px);
    background-size: 32px 32px;
  }

  .surface.picking {
    cursor: crosshair;
    outline: 3px dashed var(--warm);
    outline-offset: -3px;
  }

  .surface img {
    display: block;
    width: 100%;
    height: auto;
    pointer-events: none;
  }

  .spot {
    position: absolute;
    display: grid;
    justify-items: center;
    transform: translate(-50%, -50%);
  }

  .token {
    display: grid;
    place-items: center;
    width: 38px;
    height: 38px;
    border-radius: 50%;
    border: none;
    background: var(--surface);
    color: var(--ink-2);
    box-shadow: var(--shadow-lift);
    cursor: pointer;
    transition:
      background 0.3s var(--ease),
      box-shadow 0.3s var(--ease),
      color 0.3s var(--ease);
  }

  .editing .token {
    cursor: grab;
  }

  .token.lit {
    background: var(--warm);
    color: #1c1407;
    box-shadow:
      0 0 0 6px var(--warm-glow),
      0 0 28px 6px var(--warm-glow);
  }

  .token.open {
    background: var(--cool-soft);
    color: var(--cool);
  }

  .token.alarm {
    background: var(--alert);
    color: #fff;
    animation: pulse 1.2s ease-in-out infinite;
  }

  .token.off {
    opacity: 0.45;
  }

  @keyframes pulse {
    50% {
      box-shadow: 0 0 0 10px var(--alert-soft);
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .token.alarm {
      animation: none;
    }
  }

  .reading {
    margin-top: 3px;
    padding: 1px 6px;
    border-radius: 999px;
    background: var(--surface);
    color: var(--ink);
    font-size: 11px;
    font-weight: 700;
    box-shadow: var(--shadow);
  }

  .name {
    max-width: 92px;
    margin-top: 2px;
    overflow: hidden;
    color: var(--ink);
    font-size: 10.5px;
    font-weight: 600;
    text-align: center;
    text-overflow: ellipsis;
    white-space: nowrap;
    text-shadow:
      0 0 3px var(--surface),
      0 0 6px var(--surface);
  }

  .unplace {
    position: absolute;
    top: -6px;
    right: -6px;
    display: grid;
    place-items: center;
    width: 18px;
    height: 18px;
    border-radius: 50%;
    border: none;
    background: var(--alert);
    color: #fff;
    cursor: pointer;
  }

  .card {
    position: absolute;
    top: 100%;
    z-index: 5;
    display: grid;
    gap: 4px;
    min-width: 180px;
    margin-top: 6px;
    padding: 12px 14px;
    border-radius: var(--r-sm);
    background: var(--surface);
    box-shadow: var(--shadow-lift);
    font-size: 13px;
  }

  .card p {
    display: flex;
    justify-content: space-between;
    gap: 12px;
    margin: 0;
    color: var(--ink-2);
  }

  .card p b {
    color: var(--ink);
  }

  .tray {
    display: grid;
    grid-template-rows: auto auto minmax(0, 1fr);
    gap: 8px;
    max-height: 70vh;
    overflow: hidden;
    padding: 12px;
    border-radius: var(--r-lg);
    background: var(--surface);
    box-shadow: var(--shadow);
  }

  .tray ul {
    display: grid;
    gap: 2px;
    margin: 0;
    padding: 0;
    overflow-y: auto;
    list-style: none;
  }

  .tray button {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    padding: 8px 10px;
    border-radius: var(--r-sm);
    border: none;
    background: none;
    color: var(--ink);
    font: inherit;
    text-align: left;
    cursor: pointer;
  }

  .tray button small {
    margin-left: auto;
    color: var(--ink-3);
  }

  .tray button:hover,
  .tray button.chosen {
    background: var(--warm-soft);
  }

  .muted {
    color: var(--ink-3);
  }

  .small {
    font-size: 12.5px;
    margin: 0;
  }

  @media (max-width: 760px) {
    .board.editing {
      grid-template-columns: 1fr;
    }

    .tray {
      max-height: 40vh;
    }

    .token {
      width: 32px;
      height: 32px;
    }

    /* Icons only while looking; names while arranging. */
    .board:not(.editing) .name {
      display: none;
    }
  }
</style>
