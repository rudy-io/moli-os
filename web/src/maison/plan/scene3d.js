// The house in 3D, from the same drawn plan (cm): floors stacked by their
// elevation and offset, walls raised with their windows and doors (as high
// as the floor or the wall says), furnished, a tiled roof over the floor
// that has one, and the house's lights shining for real (point lights).
// Loaded only when the 3D view opens (three.js is a separate chunk).

import * as THREE from 'three';
import { OrbitControls } from 'three/addons/controls/OrbitControls.js';
import { mergeGeometries } from 'three/addons/utils/BufferGeometryUtils.js';
import { furnish } from './furniture3d.js';
import { marker, paint, HEIGHT as MARK_HEIGHT } from './markers3d.js';

const M = 0.01; // cm → m
const WALL = 250;
const SILL = 90;
const LINTEL = 215;
const DOOR = 210;
const EAVES = 30;
const MAX_LIGHTS = 12;
const OUTDOOR = new Set(['garden', 'deck', 'pool', 'terrace', 'driveway']);

/** Fixtures' colours when a skin has no palette of its own: its own tones. */
const plainPalette = (skin) => {
  const c = skin.item === 'transparent' ? skin.itemLine : skin.item;
  return {
    wood: c, fabric: c, soft: c, stone: skin.itemLine, metal: skin.itemLine, dark: skin.itemLine,
    line: skin.itemLine, green: c, terracotta: c, water: skin.window, glass: skin.window, pad: skin.itemLine,
  };
};

/** Rows of round tiles, drawn once: the roof's texture. */
function tiles(color) {
  const c = document.createElement('canvas');
  c.width = 64;
  c.height = 64;
  const x = c.getContext('2d');
  x.fillStyle = color;
  x.fillRect(0, 0, 64, 64);
  for (let row = 0; row < 4; row++) {
    for (let col = 0; col < 4; col++) {
      const cx = col * 16 + (row % 2) * 8 + 8;
      const g = x.createLinearGradient(cx - 8, 0, cx + 8, 0);
      g.addColorStop(0, 'rgba(0,0,0,0.22)');
      g.addColorStop(0.45, 'rgba(255,255,255,0.12)');
      g.addColorStop(1, 'rgba(0,0,0,0.28)');
      x.fillStyle = g;
      x.fillRect(cx - 8, row * 16, 16, 15);
    }
    x.fillStyle = 'rgba(0,0,0,0.3)';
    x.fillRect(0, row * 16 + 15, 64, 1);
  }
  const t = new THREE.CanvasTexture(c);
  t.colorSpace = THREE.SRGBColorSpace;
  t.wrapS = THREE.RepeatWrapping;
  t.wrapT = THREE.RepeatWrapping;
  return t;
}

/** One mesh per material: a floor's hundreds of boxes in a few draws. */
function merged(group) {
  group.updateMatrixWorld(true);
  const byMat = new Map();
  group.traverse((o) => {
    if (!o.isMesh) return;
    const geo = o.geometry.index ? o.geometry.toNonIndexed() : o.geometry.clone();
    geo.applyMatrix4(o.matrixWorld);
    for (const k of Object.keys(geo.attributes)) if (!['position', 'normal', 'uv'].includes(k)) geo.deleteAttribute(k);
    if (!byMat.has(o.material)) byMat.set(o.material, []);
    byMat.get(o.material).push(geo);
    o.geometry.dispose();
  });
  const out = new THREE.Group();
  for (const [material, geos] of byMat) {
    const geo = mergeGeometries(geos);
    for (const g of geos) g.dispose();
    if (geo) out.add(new THREE.Mesh(geo, material));
  }
  return out;
}

/** A skin colour; a translucent one (rgba) is mixed into the background. */
const col = (c, bg = '#ffffff') => {
  const m = /^rgba?\(([^)]+)\)/.exec(c);
  if (!m) return new THREE.Color(c);
  const [r, g, b, a = 1] = m[1].split(',').map((v) => Number(v.trim()));
  return new THREE.Color(bg).lerp(new THREE.Color(r / 255, g / 255, b / 255), Math.min(1, a * 4));
};

function box(w, h, d, material) {
  return new THREE.Mesh(new THREE.BoxGeometry(Math.max(w, 0.01), Math.max(h, 0.01), Math.max(d, 0.01)), material);
}

/** A box spanning [x, x+w] × [y, y+h] (cm) from height z0 to z1 (cm). */
function slab(x, y, w, h, z0, z1, material) {
  const mesh = box(w * M, (z1 - z0) * M, h * M, material);
  mesh.position.set((x + w / 2) * M, ((z0 + z1) / 2) * M, (y + h / 2) * M);
  return mesh;
}

export function mount(host, { plan, onTap, shapeOf = () => 'sensor' }) {
  const renderer = new THREE.WebGLRenderer({ antialias: true, powerPreference: 'low-power' });
  renderer.setPixelRatio(Math.min(window.devicePixelRatio || 1, 2));
  renderer.outputColorSpace = THREE.SRGBColorSpace;
  host.appendChild(renderer.domElement);
  renderer.domElement.style.display = 'block';
  renderer.domElement.style.touchAction = 'none';

  const scene = new THREE.Scene();
  const camera = new THREE.PerspectiveCamera(42, 1, 0.1, 400);
  const controls = new OrbitControls(camera, renderer.domElement);
  controls.enableDamping = true;
  controls.dampingFactor = 0.09;
  controls.maxPolarAngle = Math.PI * 0.47;
  controls.minDistance = 2;
  controls.maxDistance = 80;

  const hemi = new THREE.HemisphereLight(0xffffff, 0x8a8170, 1.6);
  const sun = new THREE.DirectionalLight(0xffffff, 1.4);
  sun.position.set(-12, 20, -8);
  scene.add(hemi, sun);

  const lights = [];
  for (let i = 0; i < MAX_LIGHTS; i++) {
    const l = new THREE.PointLight(0xffc46b, 0, 7, 1.6);
    l.visible = false;
    scene.add(l);
    lights.push(l);
  }

  const floors = new Map(); // id → { group, elevation, offset }
  const markers = new Map(); // device id → { m, shape, floor, elevation, parent, x, y }
  let materials = [];

  let background = '#ffffff';

  function material(color, extra = {}) {
    const m = new THREE.MeshLambertMaterial({ color: col(color, background), ...extra });
    materials.push(m);
    return m;
  }

  // Fixtures share their materials (by colour and look).
  let shared = new Map();
  function sharedMaterial(color, extra = {}) {
    const key = `${color}|${JSON.stringify(extra)}`;
    if (!shared.has(key)) shared.set(key, material(color, extra));
    return shared.get(key);
  }

  function clear() {
    for (const { group } of floors.values()) {
      scene.remove(group);
      group.traverse((o) => o.geometry?.dispose());
    }
    for (const m of materials) {
      m.map?.dispose();
      m.dispose();
    }
    materials = [];
    shared = new Map();
    floors.clear();
    markers.clear();
  }

  /** (Re)builds a device's marker in a given shape. */
  function place(id, entry, shape) {
    if (entry.m) {
      entry.parent.remove(entry.m.group);
      entry.m.group.traverse((o) => o.geometry?.dispose());
      for (const mat of entry.m.materials) mat.dispose();
    }
    const m = marker(shape, id);
    materials.push(...m.materials);
    m.group.position.set(entry.x * M, MARK_HEIGHT[shape] ?? 1.2, entry.y * M);
    // A little larger than life: read at a glance from above.
    m.group.scale.setScalar(1.4);
    entry.parent.add(m.group);
    entry.m = m;
    entry.shape = shape;
  }

  /** Everything, in a skin's colours. */
  function build(skin) {
    clear();
    background = skin.bg;
    scene.background = col(skin.bg);
    const night = skin.glowStrength >= 0.85;
    hemi.intensity = night ? 0.25 : 1.6;
    sun.intensity = night ? 0.05 : 1.4;

    // Walls: light sides, the skin's wall colour on top (a cut model).
    const wallMat = material(skin.wall3d ?? '#f1eee8');
    const capMat = material(skin.wall);
    const glassMat = material(skin.window, { transparent: true, opacity: 0.35 });
    const gateMat = material(skin.door ?? skin.itemLine);
    const palette = { ...plainPalette(skin), ...(skin.palette ?? {}) };
    const roofMat = material('#ffffff', { map: tiles(skin.roof ?? skin.wall) });
    const elevations = plan.floors.map((f, i) => (f.size ? (f.elevation ?? i * 280) : null)).filter((e) => e != null);

    plan.floors.forEach((f, index) => {
      if (!f.size) return;
      const group = new THREE.Group();
      const elevation = f.elevation ?? index * 280;
      const H = f.height ?? WALL;
      // Stairs climb to the next floor up.
      const above = elevations.filter((e) => e > elevation);
      const rise = above.length ? Math.min(...above) - elevation : 280;
      const [ox, oy] = f.offset ?? [0, 0];
      group.position.set(ox * M, elevation * M, oy * M);
      const roomMat = new Map();
      const matFor = (kind) => {
        if (!roomMat.has(kind)) roomMat.set(kind, material(skin.rooms[kind] ?? skin.rooms.other, { side: THREE.DoubleSide }));
        return roomMat.get(kind);
      };

      // Floors of the rooms (the outside only at ground level).
      for (const r of f.rooms ?? []) {
        if (r.kind === 'void' || (OUTDOOR.has(r.kind) && elevation > 0)) continue;
        const shape = new THREE.Shape(r.poly.map(([x, y]) => new THREE.Vector2(x * M, y * M)));
        const geo = new THREE.ShapeGeometry(shape);
        geo.rotateX(Math.PI / 2);
        const mesh = new THREE.Mesh(geo, r.kind === 'pool' ? material(skin.rooms.pool, { transparent: true, opacity: 0.85, side: THREE.DoubleSide }) : matFor(r.kind));
        // The water over the pool's edge, the outside just under the floors.
        mesh.position.y = r.kind === 'pool' ? 0.005 : r.kind === 'roof' ? 0.02 : OUTDOOR.has(r.kind) ? -0.02 : 0;
        group.add(mesh);
      }

      // Walls (their gaps are the openings), then what fills the openings.
      // In their own group: the floor looked at is cut lower to see inside.
      const raw = new THREE.Group();
      // A wall as high as it says (a low garden wall, a railing), turned
      // by `r` degrees around its centre.
      for (const w of f.walls ?? []) {
        const top = w.height ?? H;
        const pivot = new THREE.Group();
        pivot.position.set((w.x + w.w / 2) * M, 0, (w.y + w.h / 2) * M);
        pivot.rotation.y = (-(w.r ?? 0) * Math.PI) / 180;
        pivot.add(slab(-w.w / 2, -w.h / 2, w.w, w.h, 0, top - 4, wallMat));
        pivot.add(slab(-w.w / 2, -w.h / 2, w.w, w.h, top - 4, top, capMat));
        raw.add(pivot);
      }
      const sill = Math.min(SILL, H * 0.4);
      const lintel = Math.min(LINTEL, H - 25);
      const door = Math.min(DOOR, H - 25);
      for (const o of f.openings ?? []) {
        const cap = () => raw.add(slab(o.x, o.y, o.w, o.h, H - 4, H, capMat));
        if (o.kind === 'window') {
          raw.add(slab(o.x, o.y, o.w, o.h, 0, sill, wallMat));
          raw.add(slab(o.x, o.y, o.w, o.h, lintel, H - 4, wallMat));
          raw.add(slab(o.x, o.y, o.w, o.h, sill, lintel, glassMat));
          cap();
        } else if (o.kind === 'bay') {
          raw.add(slab(o.x, o.y, o.w, o.h, lintel, H - 4, wallMat));
          raw.add(slab(o.x, o.y, o.w, o.h, 0, lintel, glassMat));
          cap();
        } else if (o.kind === 'door') {
          raw.add(slab(o.x, o.y, o.w, o.h, door, H - 4, wallMat));
          cap();
        } else if (o.kind === 'gate') {
          // A garage door or a gate: closed, in the middle of the wall.
          const across = o.w > o.h;
          const t = Math.min(o.w, o.h) * 0.4;
          raw.add(slab(across ? o.x : o.x + (o.w - t) / 2, across ? o.y + (o.h - t) / 2 : o.y, across ? o.w : t, across ? t : o.h, 0, door, gateMat));
          raw.add(slab(o.x, o.y, o.w, o.h, door, H - 4, wallMat));
          cap();
        }
      }
      const walls = merged(raw);
      group.add(walls);

      // Fixtures, each its small model.
      const furniture = new THREE.Group();
      const seen = {};
      for (const it of f.items ?? []) {
        const nth = seen[it.kind] ?? 0;
        seen[it.kind] = nth + 1;
        const pivot = furnish(it, { mat: sharedMaterial, palette, rise, nth });
        pivot.position.set((it.x + it.w / 2) * M, 0, (it.y + it.h / 2) * M);
        pivot.rotation.y = (-(it.r ?? 0) * Math.PI) / 180;
        furniture.add(pivot);
      }
      group.add(merged(furniture));

      // A two-sided roof over the floor's walls (or rooms), its gables in
      // the walls' colour, the eaves out past the walls.
      let roof = null;
      const outline = (f.walls ?? []).length
        ? f.walls.flatMap((w) => [[w.x, w.y], [w.x + w.w, w.y + w.h]])
        : (f.rooms ?? []).flatMap((r) => r.poly);
      if (f.roof && outline.length) {
        const pts = outline;
        const xs = pts.map((q) => q[0]);
        const ys = pts.map((q) => q[1]);
        const [x0, x1, y0, y1] = [Math.min(...xs), Math.max(...xs), Math.min(...ys), Math.max(...ys)];
        roof = new THREE.Group();
        const along = f.roof.ridge !== 'y';
        // In the ridge's frame: u along it, v across.
        const [uLen, vLen] = along ? [x1 - x0, y1 - y0] : [y1 - y0, x1 - x0];
        const half = vLen / 2;
        const slope = Math.atan2(f.roof.rise, half);
        const length = (half + EAVES) / Math.cos(slope);
        roofMat.map.repeat.set((uLen + 2 * EAVES) / 60, length / 60);
        for (const side of [-1, 1]) {
          const plane = new THREE.Mesh(new THREE.BoxGeometry((uLen + 2 * EAVES) * M, 5 * M, length * M), roofMat);
          plane.rotation.x = side * slope;
          plane.position.set(0, (H + f.roof.rise / 2 - (EAVES * Math.tan(slope)) / 2) * M, ((side * (half + EAVES)) / 2) * M);
          roof.add(plane);
        }
        const tri = new THREE.Shape([new THREE.Vector2(-half * M, 0), new THREE.Vector2(half * M, 0), new THREE.Vector2(0, f.roof.rise * M)]);
        for (const end of [-1, 1]) {
          const gable = new THREE.Mesh(new THREE.ShapeGeometry(tri), material(skin.wall3d ?? '#f1eee8', { side: THREE.DoubleSide }));
          gable.rotation.y = Math.PI / 2;
          gable.position.set((end * uLen * M) / 2, H * M, 0);
          roof.add(gable);
        }
        roof.position.set(((x0 + x1) / 2) * M, 0, ((y0 + y1) / 2) * M);
        if (!along) roof.rotation.y = Math.PI / 2;
        group.add(roof);
      }

      // The devices, each in its own shape (lamp, switch, socket…).
      for (const s of f.devices ?? []) {
        const entry = { m: null, shape: null, floor: f.id, elevation, parent: group, x: s.x, y: s.y };
        markers.set(s.id, entry);
        place(s.id, entry, shapeOf(s.id));
      }

      scene.add(group);
      // Where the walls really are (with the floor's offset and elevation).
      group.updateMatrixWorld(true);
      const bounds = new THREE.Box3().setFromObject(walls);
      floors.set(f.id, { group, walls, roof, elevation, offset: [ox, oy], bounds });
    });
  }

  /** The whole house (its walls) in view, from the south-east, above. */
  function frame() {
    const all = new THREE.Box3();
    for (const f of floors.values()) if (!f.bounds.isEmpty()) all.union(f.bounds);
    if (all.isEmpty()) return;
    const c = all.getCenter(new THREE.Vector3());
    const s = all.getSize(new THREE.Vector3());
    const span = Math.max(s.x, s.z) * 1.1;
    const v = (camera.fov * Math.PI) / 360;
    const h = Math.atan(Math.tan(v) * camera.aspect);
    // Seen from above at an angle, the house looks smaller than its span.
    const dist = (span / 2 / Math.tan(Math.min(v, h))) * 0.88;
    const dir = new THREE.Vector3(0.45, 1.05, 0.75).normalize();
    controls.target.set(c.x, 0.8, c.z);
    camera.position.copy(controls.target).addScaledVector(dir, dist);
    controls.update();
  }

  // ---- on demand rendering --------------------------------------------------------

  let raf = 0;
  const render = () => renderer.render(scene, camera);
  function loop() {
    raf = 0;
    if (controls.update()) raf = requestAnimationFrame(loop);
    render();
  }
  const wake = () => {
    if (!raf) raf = requestAnimationFrame(loop);
  };
  controls.addEventListener('change', wake);

  const resize = () => {
    const w = host.clientWidth || 1;
    const h = host.clientHeight || 1;
    renderer.setSize(w, h, false);
    renderer.domElement.style.width = `${w}px`;
    renderer.domElement.style.height = `${h}px`;
    camera.aspect = w / h;
    camera.updateProjectionMatrix();
    wake();
  };
  const observer = new ResizeObserver(resize);
  observer.observe(host);

  // A tap (not a drag) on a bead: the device.
  const ray = new THREE.Raycaster();
  let downAt = null;
  const onDown = (e) => {
    downAt = { x: e.clientX, y: e.clientY };
  };
  const onUp = (e) => {
    if (!downAt || Math.hypot(e.clientX - downAt.x, e.clientY - downAt.y) > 6) return;
    const r = renderer.domElement.getBoundingClientRect();
    ray.setFromCamera(new THREE.Vector2(((e.clientX - r.left) / r.width) * 2 - 1, -((e.clientY - r.top) / r.height) * 2 + 1), camera);
    const shown = [...markers.values()].filter((e) => e.m.group.visible && e.parent.visible).map((e) => e.m.group);
    const hit = ray.intersectObjects(shown, true).find((h) => h.object.userData.device);
    if (hit) onTap(hit.object.userData.device);
  };
  renderer.domElement.addEventListener('pointerdown', onDown);
  renderer.domElement.addEventListener('pointerup', onUp);

  let skinShown = null;

  return {
    /**
     * floorId: the floor looked at (higher floors hidden: a doll's house).
     * devices: [{ id, shape: lamp|switch|plug|camera|climate|media|sensor,
     *   state: off|idle|on|lit|alarm|open, level (0–1), shines (it lights
     *   the room: a lamp, or a switch that drives one), hidden }].
     */
    update({ skin, floorId, devices }) {
      if (skin !== skinShown) {
        build(skin);
        if (!skinShown) {
          resize();
          frame();
        }
        skinShown = skin;
      }
      const top = floors.get(floorId)?.elevation ?? Infinity;
      for (const { group, walls, roof, elevation } of floors.values()) {
        group.visible = elevation <= top;
        // The floor looked at: walls cut at 60 % (a doll's house); below
        // it, full height (they carry the floor above). A floor under its
        // roof is seen whole, roof on: the house from outside.
        walls.scale.y = elevation === top && !roof ? 0.6 : 1;
      }

      const lit = [];
      for (const d of devices) {
        const entry = markers.get(d.id);
        if (!entry) continue;
        if (d.shape && d.shape !== entry.shape) place(d.id, entry, d.shape);
        paint(entry.m, entry.shape, d.state, d.level ?? 0, skin.glow);
        entry.m.group.visible = !d.hidden;
        const shining = d.shines && (d.state === 'lit' || d.state === 'on');
        if (shining && floors.get(entry.floor)?.group.visible) lit.push({ entry, level: d.state === 'lit' ? d.level : 1 });
      }
      lit.sort((a, b) => b.level - a.level);
      lights.forEach((l, i) => {
        const g = lit[i];
        l.visible = !!g;
        if (!g) return;
        const p = new THREE.Vector3();
        g.entry.m.group.getWorldPosition(p);
        // A switch drives a ceiling light: the light shines from up there.
        if (g.entry.shape !== 'lamp') p.y += 0.6;
        l.position.copy(p);
        l.color.set(skin.glow);
        l.intensity = (skin.glowStrength >= 0.85 ? 9 : 4) * (0.4 + g.level);
      });
      wake();
    },
    reset() {
      frame();
      wake();
    },
    dispose() {
      cancelAnimationFrame(raf);
      observer.disconnect();
      controls.dispose();
      clear();
      renderer.domElement.removeEventListener('pointerdown', onDown);
      renderer.domElement.removeEventListener('pointerup', onUp);
      renderer.dispose();
      renderer.domElement.remove();
    },
  };
}
