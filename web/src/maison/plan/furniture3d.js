// Fixtures in 3D: each kind a small model of boxes and cylinders, in its
// own frame (cm): centred, the floor at y = 0, its width along x, its depth
// along z. Its back (headboard, backrest, screen, wardrobe back) is the
// plan's top edge (-z); `r` turns it on the plan.

import * as THREE from 'three';
import { RoundedBoxGeometry } from 'three/addons/geometries/RoundedBoxGeometry.js';

const M = 0.01; // cm → m

/** Default heights (cm), when a fixture does not say its own. */
export const HEIGHT = {
  bed: 55, bunk: 170, sofa: 80, armchair: 80, chair: 88, table: 75, desk: 75, wardrobe: 200,
  tv: 110, plant: 90, counter: 92, fridge: 180, washer: 85, bathtub: 55, shower: 200, sink: 85,
  toilet: 75, car: 145, lounger: 45, trampoline: 240, bin: 105, rug: 1.5, bookcase: 200,
  pendant: 180, hammock: 120, slide: 120, tree: 520, other: 60,
};

/** Which of a fixture's materials its own colours (« Réel ») replace:
 *  `color` the main one, `accent` the second. */
const MAIN = {
  bed: 'fabric', bunk: 'wood', sofa: 'fabric', armchair: 'fabric', chair: 'wood', table: 'wood',
  desk: 'wood', tv: 'wood', wardrobe: 'soft', counter: 'soft', fridge: 'metal', washer: 'soft',
  plant: 'green', bathtub: 'soft', shower: 'glass', sink: 'wood', toilet: 'soft', lounger: 'wood',
  trampoline: 'pad', rug: 'fabric', bookcase: 'wood', pendant: 'fabric', hammock: 'fabric',
  slide: 'pad', tree: 'green', stairs: 'wood', stairs_u: 'wood', other: 'fabric',
};
const SECOND = {
  bed: 'wood', bunk: 'soft', sofa: 'soft', armchair: 'soft', chair: 'dark', table: 'dark',
  desk: 'metal', tv: 'dark', wardrobe: 'metal', counter: 'stone', sink: 'soft', lounger: 'fabric',
  plant: 'terracotta', bookcase: 'soft', hammock: 'wood', slide: 'soft', tree: 'wood', rug: 'line',
  stairs: 'dark', stairs_u: 'dark',
};

const CAR_PAINT = ['#f2f1ee', '#b9a98b', '#5d6c7c'];
const BIN_PAINT = ['#3b7a45', '#e0b12d', '#6b6f75'];

/**
 * The model of a fixture. ctx: { mat(color, extra?), palette, rise (cm, for
 * stairs), nth (how many of this kind came before, for colours) }.
 */
export function furnish(it, ctx) {
  const g = new THREE.Group();
  const { mat } = ctx;
  const p = { ...ctx.palette };
  if (ctx.real) {
    if (it.color) p[MAIN[it.kind] ?? 'fabric'] = it.color;
    if (it.accent && SECOND[it.kind]) p[SECOND[it.kind]] = it.accent;
  }
  const w = it.w;
  const d = it.h;
  const H = it.height ?? HEIGHT[it.kind] ?? 60;

  /** A box: size (cm), centre on the plan (x, z), from height y up. */
  const box = (bw, bh, bd, x, y, z, color, extra) => {
    const m = new THREE.Mesh(new THREE.BoxGeometry(Math.max(bw, 0.5) * M, Math.max(bh, 0.5) * M, Math.max(bd, 0.5) * M), mat(color, extra));
    m.position.set(x * M, (y + bh / 2) * M, z * M);
    g.add(m);
    return m;
  };
  /** An upright cylinder: radius, height (cm), from height y up. */
  const cyl = (r, ch, x, y, z, color, extra, top = r) => {
    const m = new THREE.Mesh(new THREE.CylinderGeometry(top * M, r * M, ch * M, 20), mat(color, extra));
    m.position.set(x * M, (y + ch / 2) * M, z * M);
    g.add(m);
    return m;
  };
  /** A box with rounded edges (cushions, mattresses, soft things). */
  const soft = (bw, bh, bd, x, y, z, color, radius = 4, extra) => {
    const r = Math.min(radius, bw / 2 - 0.1, bh / 2 - 0.1, bd / 2 - 0.1);
    const geo = r > 0.5
      ? new RoundedBoxGeometry(Math.max(bw, 0.5) * M, Math.max(bh, 0.5) * M, Math.max(bd, 0.5) * M, 2, r * M)
      : new THREE.BoxGeometry(Math.max(bw, 0.5) * M, Math.max(bh, 0.5) * M, Math.max(bd, 0.5) * M);
    const m = new THREE.Mesh(geo, mat(color, extra));
    m.position.set(x * M, (y + bh / 2) * M, z * M);
    g.add(m);
    return m;
  };
  const legs = (lw, lh, inset, color) => {
    for (const sx of [-1, 1]) for (const sz of [-1, 1]) box(lw, lh, lw, sx * (w / 2 - inset), 0, sz * (d / 2 - inset), color);
  };

  switch (it.kind) {
    case 'bed': {
      box(w, 26, d, 0, 0, 0, p.wood);
      soft(w - 6, 20, d - 8, 0, 26, 2, p.soft, 6);
      soft(w - 2, 8, d * 0.62, 0, 42, d / 2 - d * 0.31 - 1, p.fabric, 4);
      for (const sx of w > 120 ? [-1, 1] : [0]) soft(w > 120 ? w * 0.38 : w * 0.7, 12, 24, sx * w * 0.22, 45, -d / 2 + 20, p.soft, 5);
      box(w, Math.max(H, 90), 6, 0, 0, -d / 2 + 3, p.wood);
      break;
    }
    case 'bunk': {
      const top = Math.max(110, H - 50);
      for (const y of [12, top]) {
        box(w - 4, 14, d - 4, 0, y, 0, p.wood);
        box(w - 10, 16, d - 12, 0, y + 14, 0, p.soft);
        box(w - 8, 5, d * 0.55, 0, y + 30, d / 2 - d * 0.28 - 4, p.fabric);
        box(w * 0.6, 9, 20, 0, y + 30, -d / 2 + 18, p.soft);
      }
      for (const sx of [-1, 1]) for (const sz of [-1, 1]) box(6, H, 6, sx * (w / 2 - 3), 0, sz * (d / 2 - 3), p.dark);
      box(w - 6, 18, 4, 0, top + 30, -d / 2 + 2, p.dark);
      box(4, 18, d * 0.6, w / 2 - 2, top + 30, -d * 0.15, p.dark);
      // the ladder, at the foot
      for (const sx of [-1, 1]) box(4, top + 20, 4, sx * 20, 0, d / 2 - 2, p.dark);
      for (let y = 30; y < top; y += 30) box(40, 3, 3, 0, y, d / 2 - 2, p.dark);
      break;
    }
    case 'sofa':
    case 'armchair': {
      const arm = it.kind === 'armchair' ? Math.min(18, w * 0.18) : Math.min(22, w * 0.1);
      // feet, a base, seat cushions, back cushions, the arms
      for (const sx of [-1, 1]) for (const sz of [-1, 1]) box(3, 10, 3, sx * (w / 2 - 6), 0, sz * (d / 2 - 6), p.dark);
      soft(w, 26, d, 0, 10, 0, p.fabric, 5);
      const seats = it.kind === 'armchair' ? 1 : Math.max(1, Math.round((w - 2 * arm) / 75));
      const sw = (w - 2 * arm) / seats;
      for (let k = 0; k < seats; k++) {
        const sx = -w / 2 + arm + sw * (k + 0.5);
        soft(sw - 1.5, 13, d * 0.7, sx, 35, d * 0.13, p.fabric, 6);
        soft(sw - 2, H - 40, d * 0.22, sx, 36, -d / 2 + d * 0.13, p.fabric, 7);
      }
      box(w, H - 30, d * 0.08, 0, 10, -d / 2 + d * 0.04, p.fabric);
      for (const sx of [-1, 1]) soft(arm, Math.min(62, H - 10), d, sx * (w / 2 - arm / 2), 0, 0, p.fabric, 6);
      // a cushion or two, in the second colour
      if (it.kind === 'sofa') for (const sx of [-1, 1]) soft(40, 38, 12, sx * (w / 2 - arm - 26), 46, -d / 2 + d * 0.26, p.soft, 6);
      break;
    }
    case 'chair': {
      box(w, 5, d, 0, 44, 2, p.wood);
      box(w, H - 49, 4, 0, 49, -d / 2 + 3, p.wood);
      legs(3, 44, 4, p.dark);
      break;
    }
    case 'table': {
      box(w, 4, d, 0, H - 4, 0, p.wood);
      legs(5, H - 4, 7, p.wood);
      break;
    }
    case 'desk': {
      box(w, 3, d, 0, H - 3, 0, p.wood);
      for (const sx of [-1, 1]) box(3, H - 3, d - 8, sx * (w / 2 - 3), 0, 0, p.metal);
      // the computer: a screen at the back, a keyboard in front
      box(4, 14, 4, 0, H, -d / 2 + 14, p.dark);
      box(Math.min(80, w * 0.45), 36, 2.5, 0, H + 10, -d / 2 + 12, p.dark);
      box(Math.min(46, w * 0.3), 1.5, 14, 0, H, d * 0.12, p.soft);
      break;
    }
    case 'tv': {
      box(w, 45, d, 0, 0, 0, p.wood);
      box(w * 0.86, Math.max(40, H - 55), 3, 0, 52, -d / 2 + 6, p.dark, { roughness: 0.15 });
      box(10, 7, 10, 0, 45, -d / 2 + 6, p.dark);
      break;
    }
    case 'wardrobe': {
      box(w, H, d, 0, 0, 0, p.soft);
      box(0.6, H - 12, 0.6, 0, 6, d / 2, p.line);
      for (const sx of [-1, 1]) box(1.5, 18, 2, sx * 4, H * 0.45, d / 2 + 1, p.metal);
      break;
    }
    case 'counter': {
      box(w, 10, d - 6, 0, 0, -3, p.dark);
      box(w, H - 14, d, 0, 10, 0, p.soft, { roughness: 0.25 });
      // the fronts' joints
      for (let x = -w / 2 + 60; x < w / 2 - 5; x += 60) box(0.6, H - 16, 0.6, x, 11, d / 2, p.line);
      box(w + 1, 4, d + 2, 0, H - 4, 1, p.stone, { roughness: 0.5 });
      break;
    }
    case 'fridge': {
      box(w, H, d, 0, 0, 0, p.metal, { metalness: 0.6, roughness: 0.35 });
      box(0.6, 0.6, d + 0.4, 0, H * 0.62, 0, p.line);
      box(2, 40, 2, w / 2 - 6, H * 0.68, d / 2 + 1, p.line);
      break;
    }
    case 'washer': {
      box(w, H, d, 0, 0, 0, p.soft);
      const door = new THREE.Mesh(new THREE.CylinderGeometry(Math.min(w, H) * 0.3 * M, Math.min(w, H) * 0.3 * M, 3 * M, 24), mat(p.dark));
      door.rotation.x = Math.PI / 2;
      door.position.set(0, H * 0.48 * M, (d / 2 + 1) * M);
      g.add(door);
      break;
    }
    case 'plant': {
      const r = Math.min(w, d) / 2;
      cyl(r * 0.55, 32, 0, 0, 0, p.terracotta, undefined, r * 0.7);
      crown(g, r, 32 + (H - 32) / 2, Math.max(1, (H - 32) / (2 * r)), mat(p.green), ctx.nth);
      break;
    }
    case 'bathtub': {
      box(w, H, d, 0, 0, 0, p.soft);
      box(w - 14, 1, d - 14, 0, H - 9, 0, p.water, { transparent: true, opacity: 0.85 });
      break;
    }
    case 'shower': {
      box(w, 6, d, 0, 0, 0, p.soft);
      const glass = [p.glass, { transparent: true, opacity: 0.25 }];
      box(w, H - 6, 1, 0, 6, d / 2, ...glass);
      box(1, H - 6, d, w / 2, 6, 0, ...glass);
      box(3, 3, 22, -w / 2 + 12, H - 15, -d / 2 + 11, p.metal);
      break;
    }
    case 'sink': {
      box(w, H - 6, d, 0, 0, 0, p.wood);
      box(w, 6, d, 0, H - 6, 0, p.soft);
      box(w * 0.6, 1, d * 0.55, 0, H - 0.5, 2, p.water);
      break;
    }
    case 'toilet': {
      box(w, 80, d * 0.24, 0, 0, -d / 2 + d * 0.12, p.soft);
      const bowl = cyl(w * 0.42, 40, 0, 0, d * 0.12, p.soft);
      bowl.scale.z = (d * 0.7) / (w * 0.84);
      break;
    }
    case 'car': {
      const paint = CAR_PAINT[ctx.nth % CAR_PAINT.length];
      const wheel = Math.min(34, w * 0.19);
      box(w - 4, 62, d, 0, 22, 0, paint);
      box(w - 18, 46, d * 0.52, 0, 84, d * 0.04, p.dark, { transparent: true, opacity: 0.75 });
      box(w - 22, 4, d * 0.46, 0, 130, d * 0.04, paint);
      for (const sx of [-1, 1]) {
        for (const sz of [-1, 1]) {
          const t = new THREE.Mesh(new THREE.CylinderGeometry(wheel * M, wheel * M, 22 * M, 18), mat(p.dark));
          t.rotation.z = Math.PI / 2;
          t.position.set(sx * (w / 2 - 12) * M, wheel * M, sz * (d / 2 - d * 0.18) * M);
          g.add(t);
        }
      }
      // lights: white at the front (the plan's top edge), red behind
      for (const sx of [-1, 1]) {
        box(w * 0.18, 8, 2, sx * w * 0.32, 66, -d / 2, '#fff7d6');
        box(w * 0.18, 8, 2, sx * w * 0.32, 66, d / 2, '#c0392b');
      }
      break;
    }
    case 'lounger': {
      for (const sz of [-1, 1]) for (const sx of [-1, 1]) box(4, 24, 4, sx * (w / 2 - 4), 0, sz * (d / 2 - 8), p.dark);
      box(w, 7, d * 0.66, 0, 24, d / 2 - d * 0.33, p.wood);
      const back = box(w, 6, d * 0.36, 0, 0, 0, p.wood);
      back.rotation.x = -0.65;
      back.position.set(0, (24 + Math.sin(0.65) * d * 0.18 + 3) * M, (-d / 2 + d * 0.17 + d * 0.02) * M);
      box(w - 8, 4, d * 0.62, 0, 31, d / 2 - d * 0.33, p.fabric);
      break;
    }
    case 'trampoline': {
      const r = Math.min(w, d) / 2;
      const ring = new THREE.Mesh(new THREE.TorusGeometry((r - 6) * M, 7 * M, 8, 40), mat(p.pad));
      ring.rotation.x = Math.PI / 2;
      ring.position.y = 88 * M;
      g.add(ring);
      cyl(r - 10, 1.5, 0, 86, 0, p.dark);
      const net = new THREE.Mesh(new THREE.CylinderGeometry(r * M, r * M, (H - 95) * M, 32, 1, true), mat(p.dark, { transparent: true, opacity: 0.16, side: THREE.DoubleSide }));
      net.position.y = ((95 + H) / 2) * M;
      g.add(net);
      for (let k = 0; k < 8; k++) {
        const a = (k / 8) * Math.PI * 2;
        cyl(2, H, Math.cos(a) * r, 0, Math.sin(a) * r, p.metal);
      }
      break;
    }
    case 'bin': {
      const paint = BIN_PAINT[ctx.nth % BIN_PAINT.length];
      box(w - 4, H - 6, d - 4, 0, 0, 0, paint);
      box(w, 6, d, 0, H - 6, 0, paint);
      box(w * 0.5, 4, 4, 0, H - 12, -d / 2, p.dark);
      break;
    }
    case 'rug':
      soft(w, Math.max(H, 1), d, 0, 0, 0, p.fabric, 0.6, { roughness: 1 });
      break;
    case 'bookcase': {
      // sides, top, back, shelves every 36 cm, books on them
      for (const sx of [-1, 1]) box(3, H, d, sx * (w / 2 - 1.5), 0, 0, p.wood);
      box(w, 3, d, 0, H - 3, 0, p.wood);
      box(w - 6, H - 3, 1.5, 0, 0, -d / 2 + 0.75, p.soft);
      const BOOKS = ['#b8442f', '#2f5d8a', '#e3b341', '#3d7a57', '#efe6d6', '#7a3f6b', '#d9774a'];
      let k = ctx.nth * 7;
      for (let y = 0; y < H - 20; y += 36) {
        box(w - 6, 2.5, d - 2, 0, y, 0.5, p.wood);
        for (let x = -w / 2 + 6; x < w / 2 - 12; ) {
          const bw = 3 + ((k * 7) % 5);
          const bh = 20 + ((k * 13) % 11);
          if ((k * 5) % 9 > 1) box(bw, bh, d * 0.7, x + bw / 2, y + 2.5, 0, BOOKS[k % BOOKS.length]);
          x += bw + 0.5;
          k++;
        }
      }
      break;
    }
    case 'pendant': {
      // a shade (its bottom `height` cm up) hanging on a cord from the
      // ceiling, as long as it is wide, longer with `accent` (a woven one)
      const r = Math.min(w, d) / 2;
      const len = r * 2 * (it.accent ? 1.7 : 1);
      const ceiling = Math.max(ctx.height ?? 250, H + len + 20);
      const pts = [];
      for (let i = 0; i <= 10; i++) {
        const t = i / 10;
        pts.push(new THREE.Vector2(Math.max(0.5, r * Math.sin(Math.PI * (0.1 + 0.8 * t))) * M, (len - t * len) * M));
      }
      const shade = new THREE.Mesh(new THREE.LatheGeometry(pts, 18), mat(p.fabric, { side: THREE.DoubleSide, roughness: 0.95 }));
      shade.position.y = H * M;
      g.add(shade);
      cyl(0.5, ceiling - H - len, 0, H + len, 0, p.dark);
      break;
    }
    case 'hammock': {
      // a wooden arc along its width, the cloth sagging between its ends
      const arc = new THREE.Mesh(new THREE.TorusGeometry((w / 2) * M, 4 * M, 6, 24, Math.PI), mat(p.wood));
      arc.scale.y = H / (w / 2);
      arc.rotation.z = Math.PI;
      arc.position.y = H * M;
      g.add(arc);
      for (const sz of [-1, 1]) box(w * 0.5, 6, 6, 0, 0, sz * d * 0.35, p.wood);
      const cloth = new THREE.Mesh(new THREE.CylinderGeometry((w * 0.4) * M, (w * 0.4) * M, d * 0.75 * M, 18, 1, true, Math.PI / 2 + 0.6, Math.PI - 1.2), mat(p.fabric, { side: THREE.DoubleSide }));
      cloth.rotation.x = Math.PI / 2;
      cloth.rotation.z = Math.PI / 2;
      cloth.scale.set(1, 1, 0.3);
      cloth.position.y = (H * 0.95) * M;
      g.add(cloth);
      break;
    }
    case 'slide': {
      // steps at the back (top edge), the slide running down to the front
      const top = H * 0.75;
      for (const sx of [-1, 1]) box(4, top, d * 0.3, sx * (w / 2 - 2), 0, -d / 2 + d * 0.15, p.soft);
      for (let y = 25; y < top; y += 25) box(w - 8, 3, d * 0.22, 0, y, -d / 2 + d * 0.15, p.soft);
      const run = Math.hypot(d * 0.7, top);
      const chute = soft(w * 0.8, 4, run, 0, 0, 0, p.pad, 2, { roughness: 0.4 });
      chute.rotation.x = Math.atan2(top, d * 0.7);
      chute.position.set(0, (top / 2) * M, (d * 0.15) * M);
      break;
    }
    case 'tree': {
      const r = Math.max(w, d) / 2;
      const trunk = Math.max(8, r * 0.09);
      cyl(trunk, H * 0.55, 0, 0, 0, p.wood, { roughness: 1 }, trunk * 0.7);
      crown(g, r * 0.62, H * 0.66, 0.8, mat(p.green, { roughness: 0.95 }), ctx.nth, 3);
      break;
    }
    case 'stairs':
      stairs(g, it, { ...ctx, palette: p });
      break;
    case 'stairs_u':
      stairsU(g, it, { ...ctx, palette: p });
      break;
    default:
      box(w, H, d, 0, 0, 0, p.fabric);
  }
  return g;
}

/** Foliage: a few lumpy balls of leaves around a centre (cm), `stretch`
 *  tall, `n` of them. */
function crown(g, r, y, stretch, material, seed, n = 1) {
  let s = (seed * 9301 + 49297) % 233280;
  const rnd = () => (s = (s * 9301 + 49297) % 233280) / 233280;
  for (let k = 0; k < n; k++) {
    const geo = new THREE.IcosahedronGeometry(r * (k ? 0.7 : 1) * M, 2);
    const pos = geo.attributes.position;
    for (let i = 0; i < pos.count; i++) {
      const f = 0.86 + rnd() * 0.24;
      pos.setXYZ(i, pos.getX(i) * f, pos.getY(i) * f, pos.getZ(i) * f);
    }
    geo.computeVertexNormals();
    const m = new THREE.Mesh(geo, material);
    const a = (k / Math.max(1, n)) * Math.PI * 2 + rnd();
    const off = k ? r * 0.55 : 0;
    m.scale.y = stretch;
    m.position.set(Math.cos(a) * off * M, (y + (k ? -r * 0.15 : r * 0.1)) * M, Math.sin(a) * off * M);
    g.add(m);
  }
}

/** A straight flight up to the next floor: along the longer side, rising
 *  towards the plan's top edge (or its right one when wider than deep). */
/** A step up to `hgt` (cm): a solid block, or (« Réel ») an open tread
 *  on a steel bracket, the light passing under it. */
function step(g, bw, bd, x, z, hgt, wood, steel, real) {
  const at = (w, h, d, y, m) => {
    const mesh = new THREE.Mesh(new THREE.BoxGeometry(w * M, h * M, d * M), m);
    mesh.position.set(x * M, y * M, z * M);
    g.add(mesh);
  };
  if (!real) return at(bw, hgt, bd, hgt / 2, wood);
  at(bw, 4, bd, hgt - 2, wood);
  at(bw * 0.92, Math.min(16, hgt - 4), bd * 0.25, hgt - 4 - Math.min(16, hgt - 4) / 2, steel);
}

function stairs(g, it, { mat, palette: p, rise, real }) {
  const R = it.height ?? rise;
  const n = Math.max(3, Math.round(R / 19));
  const along = it.h >= it.w;
  const m = mat(p.wood);
  const steel = mat(p.dark, { roughness: 0.5 });
  for (let s = 0; s < n; s++) {
    const hgt = (R / n) * (s + 1);
    const t = (s + 0.5) / n - 0.5;
    if (along) step(g, it.w, it.h / n, 0, -t * it.h, hgt, m, steel, real);
    else step(g, it.w / n, it.h, t * it.w, 0, hgt, m, steel, real);
  }
}

/** A half-turn (U) staircase: in at the bottom-left, up along the bottom
 *  half to the right, turning at the right end, back along the top half
 *  to the left where it comes out. */
function stairsU(g, it, { mat, palette: p, rise, real }) {
  const R = it.height ?? rise;
  const n = Math.max(6, Math.round(R / 19));
  const m = mat(p.wood);
  const steel = mat(p.dark, { roughness: 0.5 });
  const w = it.w;
  const d = it.h;
  const turnW = Math.min(d / 2, w * 0.4);
  const run = w - turnW;
  const nTurn = 4;
  const nRun = Math.floor((n - nTurn) / 2);
  let s = 0;
  const next = (bw, bd, x, z) => {
    step(g, bw, bd, x, z, (R / n) * (s + 1), m, steel, real);
    s++;
  };
  for (let k = 0; k < nRun; k++) next(run / nRun, d / 2, -w / 2 + (run / nRun) * (k + 0.5), d / 4);
  for (let k = 0; k < nTurn; k++) next(turnW, d / nTurn, w / 2 - turnW / 2, d / 2 - (d / nTurn) * (k + 0.5));
  const back = n - s;
  for (let k = 0; k < back; k++) next(run / back, d / 2, w / 2 - turnW - (run / back) * (k + 0.5), -d / 4);
}
