<script>
  import { hub, home, value, deviceKind, isOn, roomOf, num } from '../lib/home.svelte.js';
  import { area, bbox, inside, labelAt, points } from './geometry.js';
  import { t, locale } from '../../lib/i18n.svelte.js';

  /**
   * A drawn floor (cm): rooms, walls, openings, fixtures and the house's
   * devices, alive. Zoom (wheel, pinch, double tap, buttons) and pan (drag);
   * the skin is only colours. In edit mode, fixtures move, turn and resize.
   */
  let {
    floor,
    skin,
    editing = false,
    picking = null,
    selected = $bindable(null),
    token,
    keep = () => true,
    onplace = () => {},
    ontap = () => {},
    /** Fills its box (the overview sets the size) instead of keeping the floor's shape. */
    fill = false,
  } = $props();

  const uid = Math.random().toString(36).slice(2, 8);
  let box = $state(null);
  let W = $state(0);
  let H = $state(0);
  let cam = $state({ x: 0, y: 0, k: 1 });
  let fitK = $state(1);

  const size = $derived(floor.size ?? [1000, 1000]);

  /** The whole floor in view. */
  export function fit() {
    if (!W || !H) return;
    const [w, h] = size;
    const k = Math.min(W / w, H / h) * 0.97;
    fitK = k;
    cam = { k, x: (w - W / k) / 2, y: (h - H / k) / 2 };
  }

  // A new floor, or a new screen size: start from the whole floor.
  let fitted = '';
  $effect(() => {
    const key = `${floor.id}:${W}x${H}`;
    if (W && H && key !== fitted) {
      fitted = key;
      fit();
    }
  });

  /** Screen → floor (cm). */
  export function toWorld(clientX, clientY) {
    const r = box.getBoundingClientRect();
    return { x: cam.x + (clientX - r.left) / cam.k, y: cam.y + (clientY - r.top) / cam.k };
  }

  /** The middle of what is on screen (cm). */
  export function center() {
    return { x: cam.x + W / cam.k / 2, y: cam.y + H / cam.k / 2 };
  }

  function zoomAt(factor, clientX, clientY) {
    const k = Math.min(fitK * 14, Math.max(fitK * 0.6, cam.k * factor));
    const p = toWorld(clientX, clientY);
    const r = box.getBoundingClientRect();
    cam = { k, x: p.x - (clientX - r.left) / k, y: p.y - (clientY - r.top) / k };
  }

  export function zoom(factor) {
    const r = box.getBoundingClientRect();
    zoomAt(factor, r.left + W / 2, r.top + H / 2);
  }

  // ---- gestures -------------------------------------------------------------------

  const pointers = new Map();
  let start = null;
  let pinch = null;
  let moved = false;

  function down(e) {
    if (e.pointerType === 'mouse' && e.button !== 0) return;
    pointers.set(e.pointerId, { x: e.clientX, y: e.clientY });
    if (pointers.size === 1) {
      moved = false;
      start = { x: e.clientX, y: e.clientY, cam: { ...cam } };
    } else if (pointers.size === 2) {
      const [a, b] = [...pointers.values()];
      const mid = { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 };
      pinch = { d: Math.hypot(a.x - b.x, a.y - b.y) || 1, k: cam.k, at: toWorld(mid.x, mid.y) };
      moved = true;
    }
  }

  function move(e) {
    if (!pointers.has(e.pointerId)) return;
    pointers.set(e.pointerId, { x: e.clientX, y: e.clientY });
    if (pointers.size >= 2 && pinch) {
      const [a, b] = [...pointers.values()];
      const mid = { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 };
      const k = Math.min(fitK * 14, Math.max(fitK * 0.6, (pinch.k * Math.hypot(a.x - b.x, a.y - b.y)) / pinch.d));
      const r = box.getBoundingClientRect();
      cam = { k, x: pinch.at.x - (mid.x - r.left) / k, y: pinch.at.y - (mid.y - r.top) / k };
      return;
    }
    if (!start) return;
    const dx = e.clientX - start.x;
    const dy = e.clientY - start.y;
    if (!moved && Math.abs(dx) + Math.abs(dy) < 6) return;
    if (!moved) {
      moved = true;
      box.setPointerCapture?.(e.pointerId);
    }
    cam = { k: start.cam.k, x: start.cam.x - dx / start.cam.k, y: start.cam.y - dy / start.cam.k };
  }

  function up(e) {
    if (!pointers.has(e.pointerId)) return;
    pointers.delete(e.pointerId);
    if (pointers.size < 2) pinch = null;
    if (pointers.size === 1) {
      const [p] = [...pointers.values()];
      start = { x: p.x, y: p.y, cam: { ...cam } };
      return;
    }
    if (pointers.size === 0) {
      if (!moved) {
        const p = toWorld(e.clientX, e.clientY);
        if (editing && picking) onplace(Math.round(p.x), Math.round(p.y));
        else if (editing && !e.target.closest?.('.item')) selected = null;
        else if (!editing) ontap(p);
      }
      start = null;
    }
  }

  $effect(() => {
    if (!box) return;
    const wheel = (e) => {
      e.preventDefault();
      zoomAt(Math.exp(-e.deltaY * 0.0015), e.clientX, e.clientY);
    };
    box.addEventListener('wheel', wheel, { passive: false });
    return () => box.removeEventListener('wheel', wheel);
  });

  function dbl(e) {
    if (e.target.closest?.('.pin')) return;
    zoomAt(2, e.clientX, e.clientY);
  }

  // ---- fixtures (edit) ----------------------------------------------------------------

  let grip = null;

  function grabItem(e, item, mode = 'move') {
    if (!editing) return;
    e.stopPropagation();
    e.currentTarget.setPointerCapture?.(e.pointerId);
    selected = item;
    grip = { id: e.pointerId, mode, x: e.clientX, y: e.clientY, item: { ...$state.snapshot(item) } };
  }

  function dragItem(e, item) {
    if (!grip || grip.id !== e.pointerId) return;
    const dx = (e.clientX - grip.x) / cam.k;
    const dy = (e.clientY - grip.y) / cam.k;
    if (grip.mode === 'move') {
      item.x = Math.round(grip.item.x + dx);
      item.y = Math.round(grip.item.y + dy);
    } else {
      // Resize in the fixture's own frame (it may be turned).
      const a = (-(grip.item.r ?? 0) * Math.PI) / 180;
      const lx = dx * Math.cos(a) - dy * Math.sin(a);
      const ly = dx * Math.sin(a) + dy * Math.cos(a);
      item.w = Math.max(10, Math.round(grip.item.w + lx));
      item.h = Math.max(10, Math.round(grip.item.h + ly));
    }
  }

  function dropItem(e) {
    if (grip?.id === e.pointerId) grip = null;
  }

  // ---- what is alive -------------------------------------------------------------------

  const LIGHTS = ['light', 'group'];

  function level(id) {
    const b = value(id, 'brightness');
    return typeof b === 'number' ? Math.max(0.15, Math.min(1, b / 100)) : 1;
  }

  /** Lit lights placed on this floor: where they shine, how much, in which room. */
  const glows = $derived(
    (floor.devices ?? [])
      .map((s) => ({ s, d: hub.devices[s.id] }))
      .filter(({ d }) => d && LIGHTS.includes(deviceKind(d)) && isOn(d.id))
      .map(({ s, d }) => ({
        id: s.id,
        x: s.x,
        y: s.y,
        level: level(d.id),
        room: (floor.rooms ?? []).find((r) => inside(r.poly, s.x, s.y))?.id ?? null,
      })),
  );

  /** Rooms whose Moli room has a light on (placed or not): a warm tint. */
  const warm = $derived.by(() => {
    const on = new Map();
    for (const d of Object.values(hub.devices)) {
      if (!LIGHTS.includes(deviceKind(d)) || !isOn(d.id)) continue;
      const room = roomOf(d);
      if (room) on.set(room, Math.max(on.get(room) ?? 0, level(d.id)));
    }
    return on;
  });

  const label = (n) => n.replace(/\s+/g, ' ');
  /** Devices small when the whole floor shows, full size once zoomed in. */
  const pinScale = $derived(Math.min(1, Math.max(0.6, (cam.k / fitK) * 0.6)));
  const fs = $derived(12 / cam.k);
  const visible = (x, y) => x > cam.x - 80 / cam.k && y > cam.y - 80 / cam.k && x < cam.x + (W + 80) / cam.k && y < cam.y + (H + 80) / cam.k;

  // ---- the temperature of each place ------------------------------------------------

  /** A probe's temperature now (an unreachable one says nothing). */
  function tempOf(id) {
    const d = id ? hub.devices[id] : null;
    if (!d || d.online === false) return null;
    for (const key of ['temperature', 'temp_current']) {
      const v = value(id, key);
      if (typeof v === 'number') return v;
    }
    return null;
  }

  /** The lowest point of a room under x: where its temperature sits. */
  function bottomAt(poly, x) {
    const b = bbox(poly);
    for (let i = 1; i <= 40; i++) {
      const y = b.y + b.h - (b.h * i) / 40;
      if (inside(poly, x, y)) return y;
    }
    return b.y + b.h;
  }

  const INDOOR = new Set(['living', 'kitchen', 'bedroom', 'bath', 'wc', 'office', 'hall', 'veranda', 'storage', 'other']);

  /** Each room's temperature, from the probes placed in it (averaged);
   *  the garden without one takes the weather's. Shown at the room's
   *  foot, not where the probe hangs. */
  const climates = $derived.by(() => {
    const outside = tempOf(home.config?.outdoor?.weather);
    let garden = false;
    const out = [];
    const rooms = (floor.rooms ?? []).filter((r) => r.kind !== 'void' && r.kind !== 'roof');
    // A probe counts for the smallest room around it (the pool, not the
    // deck around the pool).
    const owner = new Map();
    for (const s of floor.devices ?? []) {
      const around = rooms.filter((r) => inside(r.poly, s.x, s.y));
      if (around.length) owner.set(s.id, around.reduce((a, b) => (area(b.poly) < area(a.poly) ? b : a)).id);
    }
    for (const r of rooms) {
      const temps = (floor.devices ?? []).filter((s) => owner.get(s.id) === r.id).map((s) => tempOf(s.id)).filter((t) => t != null);
      let t = temps.length ? temps.reduce((a, b) => a + b, 0) / temps.length : null;
      if (t == null && r.kind === 'garden' && !garden) t = outside;
      if (r.kind === 'garden') garden = true;
      if (t == null) continue;
      const [x] = labelAt(r.poly);
      out.push({ id: r.id, t, x, y: bottomAt(r.poly, x), w: bbox(r.poly).w, indoor: INDOOR.has(r.kind) });
    }
    return out;
  });

  /** The floor's own: the mean of its rooms (indoor ones). */
  const floorTemp = $derived.by(() => {
    const rooms = climates.filter((c) => c.indoor);
    return rooms.length ? rooms.reduce((a, c) => a + c.t, 0) / rooms.length : null;
  });

  /** Cold to hot, at a glance. */
  const tone = (t) => (t < 18 ? 'cold' : t < 20 ? 'cool' : t <= 24 ? 'good' : t <= 27 ? 'warm' : 'hot');
  const gaugeAt = (t) => Math.min(100, Math.max(0, ((t - 14) / 16) * 100));

  const TEXTURE = { garden: 'grass', pool: 'water', terrace: 'planks', deck: 'stone' };
  const dark = $derived(skin.bg.startsWith('#0') || skin.bg.startsWith('#1'));
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  class="drawing"
  class:fill
  class:picking={!!picking}
  bind:this={box}
  bind:clientWidth={W}
  bind:clientHeight={H}
  style="aspect-ratio: {size[0]} / {size[1]}; background: {skin.bg}"
  onpointerdown={down}
  onpointermove={move}
  onpointerup={up}
  onpointercancel={up}
  ondblclick={dbl}
>
  {#if W && H}
    <svg width={W} height={H} viewBox="{cam.x} {cam.y} {W / cam.k} {H / cam.k}" role="img" aria-label={t('maison.plan.nom_etage', { name: floor.name })}>
      <defs>
        {#if skin.grid}
          <pattern id="grid-{uid}" width="100" height="100" patternUnits="userSpaceOnUse">
            <path d="M 100 0 L 0 0 0 100" fill="none" stroke={skin.grid} stroke-width="2" />
          </pattern>
        {/if}
        <pattern id="grass-{uid}" width="60" height="60" patternUnits="userSpaceOnUse">
          <circle cx="12" cy="14" r="3" fill={dark ? '#2b4a2e' : '#9fbf86'} opacity="0.55" />
          <circle cx="42" cy="38" r="2.5" fill={dark ? '#2b4a2e' : '#9fbf86'} opacity="0.45" />
          <circle cx="30" cy="52" r="2" fill={dark ? '#2b4a2e' : '#9fbf86'} opacity="0.4" />
        </pattern>
        <pattern id="water-{uid}" width="80" height="36" patternUnits="userSpaceOnUse">
          <path d="M0 18 Q 20 8 40 18 T 80 18" fill="none" stroke="#ffffff" stroke-opacity="0.35" stroke-width="3" />
        </pattern>
        <pattern id="planks-{uid}" width="200" height="24" patternUnits="userSpaceOnUse">
          <path d="M0 24 H200 M120 0 V24" stroke={dark ? '#000' : '#b89b74'} stroke-opacity="0.35" stroke-width="2" />
        </pattern>
        <pattern id="stone-{uid}" width="50" height="50" patternUnits="userSpaceOnUse">
          <path d="M0 50 H50 V0" fill="none" stroke={dark ? '#000' : '#bdb5a6'} stroke-opacity="0.4" stroke-width="2" />
        </pattern>
        <radialGradient id="glow-{uid}">
          <stop offset="0" stop-color={skin.glow} stop-opacity="1" />
          <stop offset="0.55" stop-color={skin.glow} stop-opacity="0.35" />
          <stop offset="1" stop-color={skin.glow} stop-opacity="0" />
        </radialGradient>
        {#each floor.rooms ?? [] as r (r.id)}
          <clipPath id="clip-{uid}-{r.id}"><polygon points={points(r.poly)} /></clipPath>
        {/each}
      </defs>

      {#if skin.grid}
        <rect x={-2000} y={-2000} width={size[0] + 4000} height={size[1] + 4000} fill="url(#grid-{uid})" />
      {/if}

      <!-- rooms, their texture, their warmth -->
      {#each floor.rooms ?? [] as r (r.id)}
        <polygon class="room" points={points(r.poly)} fill={skin.rooms[r.kind] ?? skin.rooms.other} stroke={skin.roomLine} stroke-width="1.2" vector-effect="non-scaling-stroke" stroke-dasharray={skin.roomLine === 'none' ? null : '6 4'} />
        {#if TEXTURE[r.kind]}
          <polygon points={points(r.poly)} fill="url(#{TEXTURE[r.kind]}-{uid})" />
        {/if}
        {#if r.room && warm.has(r.room)}
          <polygon class="warm" points={points(r.poly)} fill={skin.glow} opacity={0.1 * skin.glowStrength * warm.get(r.room)} />
        {/if}
      {/each}

      <!-- each lit light shines in its room -->
      {#each glows as g (g.id)}
        <circle
          class="glow"
          cx={g.x}
          cy={g.y}
          r={150 + 170 * g.level}
          fill="url(#glow-{uid})"
          opacity={skin.glowStrength * (0.45 + 0.55 * g.level)}
          clip-path={g.room ? `url(#clip-${uid}-${g.room})` : null}
        />
      {/each}

      <!-- fixtures -->
      {#each floor.items ?? [] as it, i (i)}
        {@const cx = it.x + it.w / 2}
        {@const cy = it.y + it.h / 2}
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <g
          class="item"
          class:chosen={editing && selected === it}
          transform="rotate({it.r ?? 0} {cx} {cy})"
          onpointerdown={(e) => grabItem(e, it)}
          onpointermove={(e) => dragItem(e, it)}
          onpointerup={dropItem}
          onpointercancel={dropItem}
        >
          {#if it.kind === 'plant'}
            <circle {cx} {cy} r={Math.min(it.w, it.h) / 2} fill={dark ? '#1f3323' : '#cfe3bf'} stroke={skin.itemLine} stroke-width="1.2" vector-effect="non-scaling-stroke" />
          {:else if it.kind === 'trampoline'}
            <ellipse {cx} {cy} rx={it.w / 2} ry={it.h / 2} fill={skin.item} stroke={skin.itemLine} stroke-width="1.2" vector-effect="non-scaling-stroke" />
            <ellipse {cx} {cy} rx={it.w * 0.42} ry={it.h * 0.42} fill="none" stroke={skin.itemLine} stroke-width="1" vector-effect="non-scaling-stroke" />
          {:else if it.kind === 'toilet' || it.kind === 'sink'}
            <ellipse {cx} {cy} rx={it.w / 2} ry={it.h / 2} fill={skin.item} stroke={skin.itemLine} stroke-width="1.2" vector-effect="non-scaling-stroke" />
          {:else}
            <rect x={it.x} y={it.y} width={it.w} height={it.h} rx={Math.min(it.w, it.h) * (it.kind === 'car' ? 0.3 : 0.08)} fill={skin.item} stroke={skin.itemLine} stroke-width="1.2" vector-effect="non-scaling-stroke" />
            {#if it.kind === 'stairs'}
              {@const steps = Math.max(2, Math.round(Math.max(it.w, it.h) / 26))}
              {#each Array(steps - 1) as _, n (n)}
                {#if it.h >= it.w}
                  <line x1={it.x} x2={it.x + it.w} y1={it.y + (it.h * (n + 1)) / steps} y2={it.y + (it.h * (n + 1)) / steps} stroke={skin.itemLine} stroke-width="1" vector-effect="non-scaling-stroke" />
                {:else}
                  <line y1={it.y} y2={it.y + it.h} x1={it.x + (it.w * (n + 1)) / steps} x2={it.x + (it.w * (n + 1)) / steps} stroke={skin.itemLine} stroke-width="1" vector-effect="non-scaling-stroke" />
                {/if}
              {/each}
            {:else if it.kind === 'bed'}
              <rect x={it.x + it.w * 0.08} y={it.y + it.h * 0.05} width={it.w * 0.38} height={it.h * 0.16} rx="6" fill="none" stroke={skin.itemLine} stroke-width="1" vector-effect="non-scaling-stroke" />
              <rect x={it.x + it.w * 0.54} y={it.y + it.h * 0.05} width={it.w * 0.38} height={it.h * 0.16} rx="6" fill="none" stroke={skin.itemLine} stroke-width="1" vector-effect="non-scaling-stroke" />
              <line x1={it.x} x2={it.x + it.w} y1={it.y + it.h * 0.32} y2={it.y + it.h * 0.32} stroke={skin.itemLine} stroke-width="1" vector-effect="non-scaling-stroke" />
            {:else if it.kind === 'sofa'}
              <rect x={it.x} y={it.y} width={it.w} height={it.h * 0.28} fill="none" stroke={skin.itemLine} stroke-width="1" vector-effect="non-scaling-stroke" />
              <line x1={it.x + it.w * 0.14} x2={it.x + it.w * 0.14} y1={it.y + it.h * 0.28} y2={it.y + it.h} stroke={skin.itemLine} stroke-width="1" vector-effect="non-scaling-stroke" />
              <line x1={it.x + it.w * 0.86} x2={it.x + it.w * 0.86} y1={it.y + it.h * 0.28} y2={it.y + it.h} stroke={skin.itemLine} stroke-width="1" vector-effect="non-scaling-stroke" />
            {:else if it.kind === 'car'}
              <line x1={it.x + it.w * 0.12} x2={it.x + it.w * 0.88} y1={it.y + it.h * 0.27} y2={it.y + it.h * 0.27} stroke={skin.itemLine} stroke-width="1.5" vector-effect="non-scaling-stroke" />
              <line x1={it.x + it.w * 0.12} x2={it.x + it.w * 0.88} y1={it.y + it.h * 0.8} y2={it.y + it.h * 0.8} stroke={skin.itemLine} stroke-width="1.5" vector-effect="non-scaling-stroke" />
            {:else if it.kind === 'bunk'}
              <rect x={it.x + it.w * 0.15} y={it.y + it.h * 0.05} width={it.w * 0.7} height={it.h * 0.14} rx="6" fill="none" stroke={skin.itemLine} stroke-width="1" vector-effect="non-scaling-stroke" />
              <line x1={it.x} x2={it.x + it.w} y1={it.y + it.h * 0.25} y2={it.y + it.h * 0.25} stroke={skin.itemLine} stroke-width="1" vector-effect="non-scaling-stroke" />
              <line x1={it.x} x2={it.x + it.w} y1={it.y} y2={it.y + it.h} stroke={skin.itemLine} stroke-width="1" vector-effect="non-scaling-stroke" stroke-dasharray="4 4" />
            {:else if it.kind === 'armchair'}
              <rect x={it.x} y={it.y} width={it.w} height={it.h * 0.26} fill="none" stroke={skin.itemLine} stroke-width="1" vector-effect="non-scaling-stroke" />
              <line x1={it.x + it.w * 0.2} x2={it.x + it.w * 0.2} y1={it.y + it.h * 0.26} y2={it.y + it.h} stroke={skin.itemLine} stroke-width="1" vector-effect="non-scaling-stroke" />
              <line x1={it.x + it.w * 0.8} x2={it.x + it.w * 0.8} y1={it.y + it.h * 0.26} y2={it.y + it.h} stroke={skin.itemLine} stroke-width="1" vector-effect="non-scaling-stroke" />
            {:else if it.kind === 'chair'}
              <line x1={it.x} x2={it.x + it.w} y1={it.y + it.h * 0.18} y2={it.y + it.h * 0.18} stroke={skin.itemLine} stroke-width="1" vector-effect="non-scaling-stroke" />
            {:else if it.kind === 'desk'}
              <line x1={it.x + it.w * 0.3} x2={it.x + it.w * 0.7} y1={it.y + it.h * 0.2} y2={it.y + it.h * 0.2} stroke={skin.itemLine} stroke-width="2.5" vector-effect="non-scaling-stroke" />
            {:else if it.kind === 'tv'}
              <line x1={it.x + it.w * 0.07} x2={it.x + it.w * 0.93} y1={it.y + it.h * 0.2} y2={it.y + it.h * 0.2} stroke={skin.itemLine} stroke-width="2.5" vector-effect="non-scaling-stroke" />
            {:else if it.kind === 'washer'}
              <circle {cx} {cy} r={Math.min(it.w, it.h) * 0.3} fill="none" stroke={skin.itemLine} stroke-width="1" vector-effect="non-scaling-stroke" />
            {:else if it.kind === 'fridge' || it.kind === 'bin'}
              <line x1={it.x} x2={it.x + it.w} y1={it.y + it.h * 0.15} y2={it.y + it.h * 0.15} stroke={skin.itemLine} stroke-width="1" vector-effect="non-scaling-stroke" />
            {:else if it.kind === 'lounger'}
              {#each Array(Math.max(2, Math.round(it.h / 14))) as _, n (n)}
                <line x1={it.x} x2={it.x + it.w} y1={it.y + 7 + n * 14} y2={it.y + 7 + n * 14} stroke={skin.itemLine} stroke-width="1" vector-effect="non-scaling-stroke" />
              {/each}
            {:else if it.kind === 'stairs_u'}
              {@const turn = Math.min(it.h / 2, it.w * 0.4)}
              {@const run = it.w - turn}
              {@const n = Math.max(2, Math.round(run / 26))}
              <line x1={it.x} x2={it.x + run} y1={cy} y2={cy} stroke={skin.itemLine} stroke-width="1.6" vector-effect="non-scaling-stroke" />
              {#each Array(n - 1) as _, k (k)}
                <line x1={it.x + (run * (k + 1)) / n} x2={it.x + (run * (k + 1)) / n} y1={it.y} y2={it.y + it.h} stroke={skin.itemLine} stroke-width="1" vector-effect="non-scaling-stroke" />
              {/each}
              {#each [0.2, 0.4, 0.6, 0.8] as t (t)}
                <line x1={it.x + run} y1={cy} x2={it.x + it.w} y2={it.y + it.h * t} stroke={skin.itemLine} stroke-width="1" vector-effect="non-scaling-stroke" />
              {/each}
            {:else if it.kind === 'bathtub'}
              <rect x={it.x + it.w * 0.08} y={it.y + it.h * 0.12} width={it.w * 0.84} height={it.h * 0.76} rx={it.h * 0.3} fill="none" stroke={skin.itemLine} stroke-width="1" vector-effect="non-scaling-stroke" />
            {/if}
          {/if}
          {#if editing && selected === it}
            <rect class="frame" x={it.x - 6} y={it.y - 6} width={it.w + 12} height={it.h + 12} fill="none" stroke={skin.glow} stroke-width="2" stroke-dasharray="6 4" vector-effect="non-scaling-stroke" />
            <circle
              class="handle"
              cx={it.x + it.w}
              cy={it.y + it.h}
              r={9 / cam.k}
              fill={skin.glow}
              onpointerdown={(e) => grabItem(e, it, 'size')}
              onpointermove={(e) => dragItem(e, it)}
              onpointerup={dropItem}
            />
          {/if}
        </g>
      {/each}

      <!-- walls, then what opens in them -->
      {#each floor.walls ?? [] as w, i (i)}
        <rect x={w.x} y={w.y} width={w.w} height={w.h} fill={skin.wall} transform={w.r ? `rotate(${w.r} ${w.x + w.w / 2} ${w.y + w.h / 2})` : null} opacity={w.height && w.height < 120 ? 0.55 : null} />
      {/each}
      {#each floor.openings ?? [] as o, i (i)}
        {@const along = o.w >= o.h}
        {@const fill = skin.rooms.hall}
        <g class="opening">
          <rect x={o.x} y={o.y} width={o.w} height={o.h} fill={o.kind === 'door' ? fill : o.kind === 'gate' ? skin.door : skin.bg} />
          {#if o.kind === 'window' || o.kind === 'bay'}
            <rect x={o.x} y={o.y} width={o.w} height={o.h} fill={skin.window} opacity="0.22" />
            {#if along}
              <line x1={o.x} x2={o.x + o.w} y1={o.y + o.h / 2} y2={o.y + o.h / 2} stroke={skin.window} stroke-width="1.6" vector-effect="non-scaling-stroke" />
            {:else}
              <line y1={o.y} y2={o.y + o.h} x1={o.x + o.w / 2} x2={o.x + o.w / 2} stroke={skin.window} stroke-width="1.6" vector-effect="non-scaling-stroke" />
            {/if}
          {:else if o.kind === 'door'}
            {#if along}
              <line x1={o.x} x2={o.x + o.w} y1={o.y + o.h / 2} y2={o.y + o.h / 2} stroke={skin.door} stroke-width="1" stroke-dasharray="4 3" vector-effect="non-scaling-stroke" />
            {:else}
              <line y1={o.y} y2={o.y + o.h} x1={o.x + o.w / 2} x2={o.x + o.w / 2} stroke={skin.door} stroke-width="1" stroke-dasharray="4 3" vector-effect="non-scaling-stroke" />
            {/if}
          {/if}
        </g>
      {/each}

      <!-- names -->
      {#each floor.rooms ?? [] as r (r.id)}
        {@const [lx, ly] = labelAt(r.poly)}
        {@const m2 = area(r.poly)}
        {#if (r.kind !== 'void' && r.kind !== 'deck') || cam.k > fitK * 2}
          <text class="label" x={lx} y={ly} font-size={fs} fill={skin.label} text-anchor="middle">{label(r.name)}</text>
          {#if m2 >= 2 && cam.k > fitK * 1.2}
            <text class="label" x={lx} y={ly + fs * 1.25} font-size={fs * 0.85} fill={skin.sub} text-anchor="middle">{m2.toLocaleString(locale(), { minimumFractionDigits: 1, maximumFractionDigits: 1 })} m²</text>
          {/if}
        {/if}
      {/each}
    </svg>

    <!-- the devices: always the same size on screen -->
    {#each floor.devices ?? [] as spot (spot.id)}
      {@const d = hub.devices[spot.id]}
      {#if d && keep(d) && visible(spot.x, spot.y)}
        <div class="pin" style="left:{(spot.x - cam.x) * cam.k}px; top:{(spot.y - cam.y) * cam.k}px; --s:{pinScale}" class:named={cam.k > fitK * 1.7}>
          {@render token(spot, d)}
        </div>
      {/if}
    {/each}
  {/if}

  {#if W && H}
    <!-- each room's temperature, at its foot -->
    {#each climates as c (c.id)}
      {#if c.w * cam.k >= 56 && visible(c.x, c.y)}
        <div class="climate {tone(c.t)}" style="left:{(c.x - cam.x) * cam.k}px; top:{(c.y - cam.y) * cam.k}px">
          <b>{num(c.t, 1)}°</b>
          <i class="gauge"><i style="left:{gaugeAt(c.t)}%"></i></i>
        </div>
      {/if}
    {/each}
    {#if floorTemp != null}
      <div class="climate floor-temp {tone(floorTemp)}">
        <span>{floor.name}</span>
        <b>{num(floorTemp, 1)}°</b>
        <i class="gauge"><i style="left:{gaugeAt(floorTemp)}%"></i></i>
      </div>
    {/if}
  {/if}

  {#if !fill}
    <div class="zoom" onpointerdown={(e) => e.stopPropagation()}>
      <button onclick={() => zoom(1.6)} aria-label={t('maison.plan.zoom_avant')}>+</button>
      <button onclick={() => zoom(1 / 1.6)} aria-label={t('maison.plan.zoom_arriere')}>−</button>
      <button onclick={fit} aria-label={t('maison.plan.tout_le_plan')}>⤢</button>
    </div>
  {/if}
</div>

<style>
  .drawing.fill {
    height: 100%;
    max-height: none;
    aspect-ratio: auto !important;
  }

  /* A room's temperature: a pill at its foot, a cold-to-hot gauge. */
  .climate {
    position: absolute;
    transform: translate(-50%, calc(-100% - 6px));
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 3px 9px;
    border-radius: 999px;
    background: color-mix(in srgb, var(--surface) 90%, transparent);
    box-shadow: 0 1px 6px rgb(0 0 0 / 14%);
    font-size: 12px;
    line-height: 1.2;
    pointer-events: none;
    white-space: nowrap;
    z-index: 1;
  }

  .climate b {
    font-weight: 700;
    font-variant-numeric: tabular-nums;
  }

  .climate span {
    color: var(--ink-3);
    font-weight: 600;
  }

  .floor-temp {
    left: 12px;
    top: 12px;
    transform: none;
  }

  .gauge {
    position: relative;
    width: 34px;
    height: 5px;
    border-radius: 3px;
    background: linear-gradient(90deg, #4f8cff 0%, #3fc1d9 25%, #4cc26a 40%, #4cc26a 62%, #ffb340 80%, #ff5a3c 100%);
  }

  .gauge i {
    position: absolute;
    top: -3px;
    width: 3px;
    height: 11px;
    border-radius: 2px;
    background: var(--ink);
    box-shadow: 0 0 0 1.5px var(--surface);
    transform: translateX(-50%);
  }

  .cold b {
    color: #3b7bea;
  }

  .cool b {
    color: #2aa5bd;
  }

  .good b {
    color: #2f9e55;
  }

  .warm b {
    color: #e08a00;
  }

  .hot b {
    color: #e5482b;
  }

  .drawing {
    position: relative;
    width: 100%;
    max-height: max(320px, calc(100dvh - 360px));
    overflow: hidden;
    border-radius: var(--r-lg);
    box-shadow: var(--shadow);
    touch-action: none;
    user-select: none;
    -webkit-user-select: none;
  }

  .drawing.picking {
    cursor: crosshair;
    outline: 3px dashed var(--warm);
    outline-offset: -3px;
  }

  svg {
    display: block;
  }

  .label {
    font-family: inherit;
    font-weight: 650;
    pointer-events: none;
    paint-order: stroke;
  }

  .glow {
    pointer-events: none;
    mix-blend-mode: screen;
    transition: opacity 0.6s;
  }

  .warm {
    pointer-events: none;
    transition: opacity 0.6s;
  }

  .item {
    cursor: default;
  }

  :global(.editing) .item {
    cursor: grab;
  }

  .handle {
    cursor: nwse-resize;
  }

  .pin {
    position: absolute;
    display: grid;
    justify-items: center;
    transform: translate(-50%, -50%) scale(var(--s, 1));
  }

  .pin:not(.named) :global(.name) {
    display: none;
  }

  .zoom {
    position: absolute;
    right: 10px;
    bottom: 10px;
    display: grid;
    gap: 6px;
  }

  .zoom button {
    width: 36px;
    height: 36px;
    border-radius: 50%;
    border: none;
    background: var(--surface);
    color: var(--ink);
    box-shadow: var(--shadow-lift);
    font: inherit;
    font-size: 18px;
    font-weight: 700;
    cursor: pointer;
  }
</style>
