// The house in 3D, from the same drawn plan (cm): floors stacked by their
// elevation and offset, walls raised with their windows and doors (as high
// as the floor or the wall says), furnished, a tiled roof over the floor
// that has one, and the house's lights shining for real (point lights).
// Lit like a photo: an environment computed here (nothing downloaded), a
// sun that casts soft shadows (drawn again only when the scene changes).
// The « Réel » skin also draws each room as it really is (its floor, its
// paint, its painted walls, the facade, the window frames).
// Loaded only when the 3D view opens (three.js is a separate chunk).

import * as THREE from 'three';
import { OrbitControls } from 'three/addons/controls/OrbitControls.js';
import { RoomEnvironment } from 'three/addons/environments/RoomEnvironment.js';
import { mergeGeometries } from 'three/addons/utils/BufferGeometryUtils.js';
import { furnish } from './furniture3d.js';
import { marker, paint, HEIGHT as MARK_HEIGHT } from './markers3d.js';
import { floorTexture, wallTexture, waterTexture, roofTexture, FLOOR_TINT, FLOOR_OF } from './textures3d.js';

const M = 0.01; // cm → m
const WALL = 250;
const SILL = 90;
const LINTEL = 215;
const DOOR = 210;
const EAVES = 30;
const SLAB = 18;
const MAX_LIGHTS = 12;
const OUTDOOR = new Set(['garden', 'deck', 'pool', 'terrace', 'driveway']);
/** How shiny each floor is (0 a mirror, 1 matt). */
const ROUGH = { tile: 0.34, wood: 0.55, wood_dark: 0.5, concrete: 0.85, stone: 0.9, carpet: 1, grass: 1, deck: 0.8, gravel: 1 };

/** Fixtures' colours when a skin has no palette of its own: its own tones. */
const plainPalette = (skin) => {
  const c = skin.item === 'transparent' ? skin.itemLine : skin.item;
  return {
    wood: c, fabric: c, soft: c, stone: skin.itemLine, metal: skin.itemLine, dark: skin.itemLine,
    line: skin.itemLine, green: c, terracotta: c, water: skin.window, glass: skin.window, pad: skin.itemLine,
  };
};

/** One mesh per material: a floor's hundreds of boxes in a few draws. */
function merged(group, shadows = true) {
  group.updateMatrixWorld(true);
  const byMat = new Map();
  group.traverse((o) => {
    if (!o.isMesh) return;
    const geo = o.geometry.index ? o.geometry.toNonIndexed() : o.geometry.clone();
    geo.applyMatrix4(o.matrixWorld);
    for (const k of Object.keys(geo.attributes)) if (!['position', 'normal', 'uv'].includes(k)) geo.deleteAttribute(k);
    geo.clearGroups();
    if (!byMat.has(o.material)) byMat.set(o.material, []);
    byMat.get(o.material).push(geo);
    o.geometry.dispose();
  });
  const out = new THREE.Group();
  for (const [material, geos] of byMat) {
    const geo = mergeGeometries(geos);
    for (const g of geos) g.dispose();
    if (!geo) continue;
    const mesh = new THREE.Mesh(geo, material);
    // Glass and water let the light through.
    mesh.castShadow = shadows && !material.transparent;
    mesh.receiveShadow = shadows;
    out.add(mesh);
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

/** A thin panel along a wall: from `a` (cm, on the plan) `len` cm towards
 *  `dir`, `inset` cm into the room (`n`), from height z0 to z1. Its UVs in
 *  metres (a print repeats along the wall). */
function panel(a, dir, n, t0, t1, z0, z1, inset, thick, material) {
  const len = t1 - t0;
  const geo = new THREE.BoxGeometry(len * M, (z1 - z0) * M, thick * M);
  const uv = geo.attributes.uv;
  for (let i = 0; i < uv.count; i++) uv.setXY(i, uv.getX(i) * len * M, uv.getY(i) * (z1 - z0) * M + z0 * M);
  const mesh = new THREE.Mesh(geo, material);
  const mid = (t0 + t1) / 2;
  mesh.position.set((a[0] + dir[0] * mid + n[0] * inset) * M, ((z0 + z1) / 2) * M, (a[1] + dir[1] * mid + n[1] * inset) * M);
  mesh.rotation.y = -Math.atan2(dir[1], dir[0]);
  return mesh;
}

/** [t0, t1] intervals, merged when they touch. */
function union(spans) {
  const out = [];
  for (const s of [...spans].sort((p, q) => p[0] - q[0])) {
    const last = out[out.length - 1];
    if (last && s[0] <= last[1] + 1) {
      last[1] = Math.max(last[1], s[1]);
      last[2] = Math.min(last[2], s[2]);
    } else out.push([...s]);
  }
  return out;
}

/** Where a straight (axis-aligned) side of a room touches a box (a wall,
 *  an opening): its span along the side, or null. */
function along(a, b, s, slack = 4) {
  const horizontal = Math.abs(a[1] - b[1]) <= 1;
  const [lo, hi] = horizontal ? [Math.min(a[0], b[0]), Math.max(a[0], b[0])] : [Math.min(a[1], b[1]), Math.max(a[1], b[1])];
  const line = horizontal ? a[1] : a[0];
  const [s0, s1] = horizontal ? [s.y, s.y + s.h] : [s.x, s.x + s.w];
  if (line < s0 - slack || line > s1 + slack) return null;
  const [p0, p1] = horizontal ? [s.x, s.x + s.w] : [s.y, s.y + s.h];
  const from = Math.max(lo, p0);
  const to = Math.min(hi, p1);
  if (to - from < 2) return null;
  // As a distance from `a`.
  const start = horizontal ? a[0] : a[1];
  const forward = (horizontal ? b[0] : b[1]) >= start;
  return forward ? [from - start, to - start] : [start - to, start - from];
}

export function mount(host, { plan, onTap, shapeOf = () => 'sensor' }) {
  const renderer = new THREE.WebGLRenderer({ antialias: true, powerPreference: 'low-power' });
  renderer.setPixelRatio(Math.min(window.devicePixelRatio || 1, 2));
  renderer.outputColorSpace = THREE.SRGBColorSpace;
  // Colours as given (the skins' palettes), highlights rolled off softly.
  renderer.toneMapping = THREE.NeutralToneMapping;
  renderer.shadowMap.enabled = true;
  renderer.shadowMap.type = THREE.PCFSoftShadowMap;
  // The shadows are drawn when the scene changes, not at every frame.
  renderer.shadowMap.autoUpdate = false;
  host.appendChild(renderer.domElement);
  renderer.domElement.style.display = 'block';
  renderer.domElement.style.touchAction = 'none';

  const scene = new THREE.Scene();
  // Soft light from all around, as in a bright room: computed once here.
  const pmrem = new THREE.PMREMGenerator(renderer);
  const room = new RoomEnvironment();
  const environment = pmrem.fromScene(room, 0.04).texture;
  room.dispose();
  pmrem.dispose();
  scene.environment = environment;

  const camera = new THREE.PerspectiveCamera(42, 1, 0.1, 400);
  const controls = new OrbitControls(camera, renderer.domElement);
  controls.enableDamping = true;
  controls.dampingFactor = 0.09;
  controls.maxPolarAngle = Math.PI * 0.47;
  controls.minDistance = 2;
  controls.maxDistance = 80;

  const hemi = new THREE.HemisphereLight(0xffffff, 0x8a8170, 1.6);
  const sun = new THREE.DirectionalLight(0xffffff, 1.4);
  sun.castShadow = true;
  // A small screen gets a lighter shadow map.
  const big = Math.min(window.screen?.width ?? 1024, window.screen?.height ?? 1024) >= 700;
  sun.shadow.mapSize.set(big ? 2048 : 1024, big ? 2048 : 1024);
  sun.shadow.bias = -0.0005;
  sun.shadow.normalBias = 0.03;
  sun.shadow.radius = 3;
  scene.add(hemi, sun, sun.target);

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
  let ground = null;

  let background = '#ffffff';

  function material(color, extra = {}) {
    const m = new THREE.MeshStandardMaterial({ color: col(color, background), roughness: 0.85, metalness: 0, ...extra });
    materials.push(m);
    return m;
  }

  // Fixtures share their materials (by colour and look); so do textures.
  let shared = new Map();
  function sharedMaterial(color, extra = {}) {
    const key = `${color}|${JSON.stringify(extra)}`;
    if (!shared.has(key)) shared.set(key, material(color, extra));
    return shared.get(key);
  }
  function textured(key, make, extra = {}) {
    if (!shared.has(key)) {
      const map = make();
      map.repeat.set(1 / map.userData.size, 1 / map.userData.size);
      shared.set(key, material('#ffffff', { map, ...extra }));
    }
    return shared.get(key);
  }

  function clear() {
    for (const { group } of floors.values()) {
      scene.remove(group);
      group.traverse((o) => o.geometry?.dispose());
    }
    if (ground) {
      scene.remove(ground);
      ground.geometry.dispose();
      ground = null;
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
    const real = !!skin.real;
    hemi.color.set(real ? '#e4ecf5' : '#ffffff');
    hemi.groundColor.set(real ? '#8c7c64' : '#8a8170');
    hemi.intensity = night ? 0.2 : real ? 0.45 : 1.05;
    sun.color.set(real ? '#fff0da' : '#ffffff');
    sun.intensity = night ? 0.05 : real ? 2.1 : 1.5;
    scene.environmentIntensity = night ? 0.06 : real ? 0.38 : 0.45;

    // Walls: light sides, the skin's wall colour on top (a cut model).
    const capMat = material(skin.wall);
    const glassMat = material(skin.window, { transparent: true, opacity: real ? 0.28 : 0.35, roughness: 0.05 });
    const gateMat = material(skin.door ?? skin.itemLine);
    const palette = { ...plainPalette(skin), ...(skin.palette ?? {}) };
    const roofMat = material('#ffffff', { map: roofTexture(skin.roof ?? skin.wall, real), roughness: 0.8 });
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
      // Seen from outside: the facade's colour (« Réel »), else the skin's.
      const wallMat = material(real && f.facade ? f.facade : (skin.wall3d ?? '#f1eee8'));
      const roomMat = new Map();
      const matFor = (kind) => {
        if (!roomMat.has(kind)) roomMat.set(kind, material(skin.rooms[kind] ?? skin.rooms.other, { side: THREE.DoubleSide }));
        return roomMat.get(kind);
      };
      const floorMat = (kind, tint) =>
        textured(`floor|${kind}|${tint}`, () => floorTexture(kind, tint), { roughness: ROUGH[kind] ?? 0.8, side: THREE.DoubleSide });
      const waterMat = () =>
        textured(`water|${skin.rooms.pool}`, () => waterTexture(real ? '#4fb3d9' : skin.rooms.pool), { transparent: true, opacity: 0.9, roughness: 0.2, envMapIntensity: 0.25, side: THREE.DoubleSide });

      // What sits on the floor: the rooms' floors (the outside only at
      // ground level), a slab under an upper floor, a raised pool's sides.
      const ground0 = new THREE.Group();
      const slabs = new THREE.Group();
      for (const r of f.rooms ?? []) {
        if (r.kind === 'void' || (OUTDOOR.has(r.kind) && elevation > 0)) continue;
        const fin = (real && r.finish) || {};
        const shape = new THREE.Shape(r.poly.map(([x, y]) => new THREE.Vector2(x * M, y * M)));
        if (elevation > 0) {
          const s = new THREE.ExtrudeGeometry(shape, { depth: SLAB * M, bevelEnabled: false });
          s.rotateX(Math.PI / 2);
          const m = new THREE.Mesh(s, wallMat);
          slabs.add(m);
        }
        if (r.kind === 'pool') {
          const raise = fin.raise ?? 0;
          const geo = new THREE.ShapeGeometry(shape);
          geo.rotateX(Math.PI / 2);
          const water = new THREE.Mesh(geo, real ? waterMat() : material(skin.rooms.pool, { transparent: true, opacity: 0.85, side: THREE.DoubleSide }));
          water.position.y = raise ? (raise - 12) * M : 0.005;
          group.add(water);
          // An above-ground pool: clad sides, as high as it is raised.
          if (raise) {
            const clad = textured(`clad|${fin.tint ?? '#eef0ec'}`, () => floorTexture('deck', fin.tint ?? '#eef0ec'), { roughness: 0.85 });
            const sign = Math.sign(area(r.poly)) || 1;
            r.poly.forEach((a, i) => {
              const b = r.poly[(i + 1) % r.poly.length];
              const len = Math.hypot(b[0] - a[0], b[1] - a[1]);
              if (len < 1) return;
              const dir = [(b[0] - a[0]) / len, (b[1] - a[1]) / len];
              const n = [-dir[1] * sign, dir[0] * sign];
              ground0.add(panel(a, dir, n, -10, len + 10, 0, raise, -5, 10, clad));
            });
          }
          continue;
        }
        const kind = fin.floor ?? FLOOR_OF[r.kind] ?? 'tile';
        const mat = real ? floorMat(kind, fin.tint ?? FLOOR_TINT[kind]) : matFor(r.kind);
        const lift = fin.raise ?? 0;
        const geo = lift ? new THREE.ExtrudeGeometry(shape, { depth: lift * M, bevelEnabled: false }) : new THREE.ShapeGeometry(shape);
        geo.rotateX(Math.PI / 2);
        const mesh = new THREE.Mesh(geo, mat);
        // The outside just under the floors.
        mesh.position.y = lift ? lift * M : r.kind === 'roof' ? 0.02 : OUTDOOR.has(r.kind) ? -0.02 : 0;
        mesh.receiveShadow = true;
        group.add(mesh);
      }
      group.add(merged(ground0));
      if (slabs.children.length) {
        const s = merged(slabs);
        group.add(s);
      }

      // Walls (their gaps are the openings), then what fills the openings.
      // In their own group: the floor looked at is cut lower to see inside.
      const raw = new THREE.Group();
      const frames = new Map();
      const frameMat = (c) => {
        if (!frames.has(c)) frames.set(c, material(c, { roughness: 0.45 }));
        return frames.get(c);
      };
      // A wall as high as it says (a low garden wall, a railing), turned
      // by `r` degrees around its centre; in its own colour if it has one.
      for (const w of f.walls ?? []) {
        const top = w.height ?? H;
        const pivot = new THREE.Group();
        pivot.position.set((w.x + w.w / 2) * M, 0, (w.y + w.h / 2) * M);
        pivot.rotation.y = (-(w.r ?? 0) * Math.PI) / 180;
        pivot.add(slab(-w.w / 2, -w.h / 2, w.w, w.h, 0, top - 4, real && w.color ? frameMat(w.color) : wallMat));
        pivot.add(slab(-w.w / 2, -w.h / 2, w.w, w.h, top - 4, top, capMat));
        raw.add(pivot);
      }
      const sill = Math.min(SILL, H * 0.4);
      const lintel = Math.min(LINTEL, H - 25);
      const door = Math.min(DOOR, H - 25);
      /** Glass in a frame (« Réel »): thin, set in the middle of the wall. */
      const glazing = (o, z0, z1) => {
        if (!real) {
          raw.add(slab(o.x, o.y, o.w, o.h, z0, z1, glassMat));
          return;
        }
        const across = o.w >= o.h;
        const L = across ? o.w : o.h;
        const T = across ? o.h : o.w;
        const fm = frameMat(o.color ?? skin.frame ?? '#f2f2f0');
        const at = (t0, t1, d0, d1, za, zb, m) =>
          raw.add(across ? slab(o.x + t0, o.y + d0, t1 - t0, d1 - d0, za, zb, m) : slab(o.x + d0, o.y + t0, d1 - d0, t1 - t0, za, zb, m));
        const mid = T / 2;
        at(0, L, mid - 1, mid + 1, z0, z1, glassMat);
        const f = 5;
        at(0, L, mid - 3, mid + 3, z0, z0 + f, fm);
        at(0, L, mid - 3, mid + 3, z1 - f, z1, fm);
        at(0, f, mid - 3, mid + 3, z0, z1, fm);
        at(L - f, L, mid - 3, mid + 3, z0, z1, fm);
        // Two leaves when it is wide.
        if (L > 100) at(L / 2 - 2.5, L / 2 + 2.5, mid - 3, mid + 3, z0, z1, fm);
        // The sill outside, the wall's depth in plaster inside.
        if (z0 > 0) at(-3, L + 3, -2, T + 2, z0 - 3, z0, fm);
      };
      for (const o of f.openings ?? []) {
        const cap = () => raw.add(slab(o.x, o.y, o.w, o.h, H - 4, H, capMat));
        if (o.kind === 'window') {
          raw.add(slab(o.x, o.y, o.w, o.h, 0, sill, wallMat));
          raw.add(slab(o.x, o.y, o.w, o.h, lintel, H - 4, wallMat));
          glazing(o, sill, lintel);
          cap();
        } else if (o.kind === 'bay') {
          raw.add(slab(o.x, o.y, o.w, o.h, lintel, H - 4, wallMat));
          glazing(o, 0, lintel);
          cap();
        } else if (o.kind === 'door') {
          raw.add(slab(o.x, o.y, o.w, o.h, door, H - 4, wallMat));
          cap();
        } else if (o.kind === 'gate') {
          // A garage door or a gate: closed, in the middle of the wall.
          const across = o.w > o.h;
          const t = Math.min(o.w, o.h) * 0.4;
          const gm = real && o.color ? frameMat(o.color) : gateMat;
          raw.add(slab(across ? o.x : o.x + (o.w - t) / 2, across ? o.y + (o.h - t) / 2 : o.y, across ? o.w : t, across ? t : o.h, 0, door, gm));
          raw.add(slab(o.x, o.y, o.w, o.h, door, H - 4, wallMat));
          cap();
        }
      }
      if (real) paintRooms(f, raw, { H, sill, lintel, door, skin });
      const walls = merged(raw);
      group.add(walls);

      // Fixtures, each its small model.
      const furniture = new THREE.Group();
      const seen = {};
      for (const it of f.items ?? []) {
        const nth = seen[it.kind] ?? 0;
        seen[it.kind] = nth + 1;
        const pivot = furnish(it, { mat: sharedMaterial, palette, rise, nth, real, height: H });
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
        const trimMat = material(real ? (f.trim ?? skin.trim ?? '#e9e4da') : (skin.wall3d ?? '#f1eee8'), { roughness: 0.6 });
        for (const side of [-1, 1]) {
          const plane = new THREE.Mesh(new THREE.BoxGeometry((uLen + 2 * EAVES) * M, 5 * M, length * M), roofMat);
          plane.rotation.x = side * slope;
          plane.position.set(0, (H + f.roof.rise / 2 - (EAVES * Math.tan(slope)) / 2) * M, ((side * (half + EAVES)) / 2) * M);
          plane.castShadow = true;
          plane.receiveShadow = true;
          // The board along the eaves.
          const fascia = new THREE.Mesh(new THREE.BoxGeometry((uLen + 2 * EAVES) * M, 16 * M, 3 * M), trimMat);
          fascia.position.set(0, -6 * M, (side * length * M) / 2);
          plane.add(fascia);
          roof.add(plane);
        }
        // The ridge's cap tiles.
        const ridge = new THREE.Mesh(new THREE.CylinderGeometry(9 * M, 9 * M, (uLen + 2 * EAVES) * M, 10, 1, false, 0, Math.PI), roofMat);
        ridge.rotation.z = Math.PI / 2;
        ridge.position.set(0, (H + f.roof.rise + 1) * M, 0);
        roof.add(ridge);
        const tri = new THREE.Shape([new THREE.Vector2(-half * M, 0), new THREE.Vector2(half * M, 0), new THREE.Vector2(0, f.roof.rise * M)]);
        for (const end of [-1, 1]) {
          const gable = new THREE.Mesh(new THREE.ShapeGeometry(tri), material(real && f.facade ? f.facade : (skin.wall3d ?? '#f1eee8'), { side: THREE.DoubleSide }));
          gable.rotation.y = Math.PI / 2;
          gable.position.set((end * uLen * M) / 2, H * M, 0);
          gable.castShadow = true;
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

    // The sun over the whole house, its shadows just covering it.
    const all = houseBounds();
    if (!all.isEmpty()) {
      const c = all.getCenter(new THREE.Vector3());
      const s = all.getSize(new THREE.Vector3());
      const span = Math.max(s.x, s.z) * 0.75 + 2;
      sun.target.position.copy(c);
      sun.position.copy(c).add(new THREE.Vector3(-0.55, 1, -0.35).normalize().multiplyScalar(40));
      const cam = sun.shadow.camera;
      cam.left = -span;
      cam.right = span;
      cam.top = span;
      cam.bottom = -span;
      cam.near = 1;
      cam.far = 90;
      cam.updateProjectionMatrix();
      sun.target.updateMatrixWorld();
      // The street and the neighbours' plots, plain, under everything.
      if (real) {
        ground = new THREE.Mesh(new THREE.PlaneGeometry(s.x + 60, s.z + 60), material('#b3aea4', { roughness: 1 }));
        ground.rotation.x = -Math.PI / 2;
        ground.position.set(c.x, -0.05, c.z);
        ground.receiveShadow = true;
        scene.add(ground);
      }
    }
    renderer.shadowMap.needsUpdate = true;
  }

  /** The paint of each room on its walls (« Réel »): where a wall runs
   *  along a side of the room, a coat of paint on that side (not over its
   *  openings), skirting at the foot, and its painted bands or prints. */
  function paintRooms(f, raw, { H, sill, lintel, door, skin }) {
    const coats = new Map();
    const coat = (c, pattern) => {
      const key = `${c}|${pattern ?? ''}`;
      if (!coats.has(key)) {
        coats.set(key, pattern ? textured(`print|${key}`, () => wallTexture(pattern, c), { roughness: 0.9 }) : material(c, { roughness: 0.92 }));
      }
      return coats.get(key);
    };
    const skirting = material('#f7f6f3', { roughness: 0.6 });
    // A wall in its own colour (a wooden railing) is not painted over.
    const straight = (f.walls ?? []).filter((w) => !w.r && !w.color);
    for (const r of f.rooms ?? []) {
      if (OUTDOOR.has(r.kind) || r.kind === 'void' || r.kind === 'roof') continue;
      const fin = r.finish ?? {};
      const base = fin.paint ?? skin.paint ?? '#f4f2ee';
      const sign = Math.sign(area(r.poly)) || 1;
      r.poly.forEach((a, i) => {
        const b = r.poly[(i + 1) % r.poly.length];
        if (Math.abs(a[0] - b[0]) > 1 && Math.abs(a[1] - b[1]) > 1) return;
        const len = Math.hypot(b[0] - a[0], b[1] - a[1]);
        if (len < 5) return;
        const dir = [(b[0] - a[0]) / len, (b[1] - a[1]) / len];
        const n = [-dir[1] * sign, dir[0] * sign];
        // The walls along this side, each with its height.
        const spans = union(straight.map((w) => {
          const s = along(a, b, w);
          return s && [...s, w.height ?? H];
        }).filter(Boolean));
        if (!spans.length) return;
        // What the openings leave of the wall: [t0, t1, z0, z1].
        const holes = (f.openings ?? []).map((o) => {
          const s = along(a, b, o, 6);
          if (!s) return null;
          const keep = o.kind === 'window' ? [[0, sill], [lintel, H]] : [[o.kind === 'bay' ? lintel : door, H]];
          return { s, keep };
        }).filter(Boolean);
        const bands = [];
        for (const [t0, t1, top] of spans) {
          const cuts = [t0, t1, ...holes.flatMap((h) => h.s)].filter((t) => t >= t0 && t <= t1).sort((p, q) => p - q);
          for (let k = 0; k < cuts.length - 1; k++) {
            const [u0, u1] = [cuts[k], cuts[k + 1]];
            if (u1 - u0 < 1) continue;
            const m = (u0 + u1) / 2;
            const hole = holes.find((h) => m > h.s[0] && m < h.s[1]);
            for (const [z0, z1] of hole ? hole.keep : [[0, H]]) {
              const zt = Math.min(z1, top - 4);
              if (zt - z0 > 1) bands.push([u0, u1, z0, zt]);
            }
          }
        }
        const accents = (fin.accents ?? []).filter((x) => x.edge === i);
        for (const [u0, u1, z0, z1] of bands) {
          raw.add(panel(a, dir, n, u0, u1, z0, z1, 0.6, 1, coat(base)));
          if (z0 === 0) raw.add(panel(a, dir, n, u0, u1, 0, 8, 1.3, 1.2, skirting));
          for (const x of accents) {
            const lo = Math.max(z0, x.from ?? 0);
            const hi = Math.min(z1, x.to ?? H);
            if (hi - lo > 1) raw.add(panel(a, dir, n, u0, u1, Math.max(lo, z0 === 0 && lo === 0 ? 8 : lo), hi, 1, 1, coat(x.color, x.pattern)));
          }
        }
      });
    }
  }

  const houseBounds = () => {
    const all = new THREE.Box3();
    for (const f of floors.values()) if (!f.bounds.isEmpty()) all.union(f.bounds);
    return all;
  };

  /** The whole house (its walls) in view, from the south-east, above. */
  function frame() {
    const all = houseBounds();
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
      renderer.shadowMap.needsUpdate = true;

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
        // By day (« Réel »), a lamp lights its corner, not the whole room.
        l.intensity = (skin.glowStrength >= 0.85 ? 9 : skin.real ? 1.6 : 4) * (0.4 + g.level);
        l.distance = skin.real ? 4.5 : 7;
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
      environment.dispose();
      renderer.domElement.removeEventListener('pointerdown', onDown);
      renderer.domElement.removeEventListener('pointerup', onUp);
      renderer.dispose();
      renderer.domElement.remove();
    },
  };
}

/** Twice the signed area of an outline: its sign says which way it turns. */
function area(poly) {
  let s = 0;
  poly.forEach(([x, y], i) => {
    const [x2, y2] = poly[(i + 1) % poly.length];
    s += x * y2 - x2 * y;
  });
  return s;
}
