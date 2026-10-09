// Fixtures in 3D: each kind a small model of boxes and cylinders, in its
// own frame (cm): centred, the floor at y = 0, its width along x, its depth
// along z. Its back (headboard, backrest, screen, wardrobe back) is the
// plan's top edge (-z); `r` turns it on the plan.

import * as THREE from 'three';

const M = 0.01; // cm → m

/** Default heights (cm), when a fixture does not say its own. */
export const HEIGHT = {
  bed: 55, bunk: 170, sofa: 80, armchair: 80, chair: 88, table: 75, desk: 75, wardrobe: 200,
  tv: 110, plant: 90, counter: 92, fridge: 180, washer: 85, bathtub: 55, shower: 200, sink: 85,
  toilet: 75, car: 145, lounger: 45, trampoline: 240, bin: 105, other: 60,
};

const CAR_PAINT = ['#f2f1ee', '#b9a98b', '#5d6c7c'];
const BIN_PAINT = ['#3b7a45', '#e0b12d', '#6b6f75'];

/**
 * The model of a fixture. ctx: { mat(color, extra?), palette, rise (cm, for
 * stairs), nth (how many of this kind came before, for colours) }.
 */
export function furnish(it, ctx) {
  const g = new THREE.Group();
  const { mat, palette: p } = ctx;
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
  const legs = (lw, lh, inset, color) => {
    for (const sx of [-1, 1]) for (const sz of [-1, 1]) box(lw, lh, lw, sx * (w / 2 - inset), 0, sz * (d / 2 - inset), color);
  };

  switch (it.kind) {
    case 'bed': {
      box(w, 26, d, 0, 0, 0, p.wood);
      box(w - 6, 20, d - 8, 0, 26, 2, p.soft);
      box(w - 2, 7, d * 0.6, 0, 44, d / 2 - d * 0.3 - 1, p.fabric);
      for (const sx of w > 120 ? [-1, 1] : [0]) box(w > 120 ? w * 0.38 : w * 0.7, 11, 22, sx * w * 0.22, 46, -d / 2 + 20, p.soft);
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
      box(w, 40, d, 0, 0, 0, p.fabric);
      box(w - 2 * arm, 10, d * 0.68, 0, 40, d * 0.13, p.soft);
      box(w, H, d * 0.26, 0, 0, -d / 2 + d * 0.13, p.fabric);
      for (const sx of [-1, 1]) box(arm, Math.min(62, H - 10), d, sx * (w / 2 - arm / 2), 0, 0, p.fabric);
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
      box(w * 0.86, Math.max(40, H - 55), 3, 0, 52, -d / 2 + 6, p.dark);
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
      box(w, H - 4, d, 0, 0, 0, p.soft);
      box(w + 1, 4, d + 2, 0, H - 4, 1, p.stone);
      break;
    }
    case 'fridge': {
      box(w, H, d, 0, 0, 0, p.metal);
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
      const leaves = new THREE.Mesh(new THREE.SphereGeometry(r * M, 14, 10), mat(p.green));
      leaves.scale.y = Math.max(1, (H - 32) / (2 * r));
      leaves.position.y = (32 + (H - 32) / 2) * M;
      g.add(leaves);
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
    case 'stairs':
      stairs(g, it, ctx);
      break;
    case 'stairs_u':
      stairsU(g, it, ctx);
      break;
    default:
      box(w, H, d, 0, 0, 0, p.fabric);
  }
  return g;
}

/** A straight flight up to the next floor: along the longer side, rising
 *  towards the plan's top edge (or its right one when wider than deep). */
function stairs(g, it, { mat, palette: p, rise }) {
  const R = it.height ?? rise;
  const n = Math.max(3, Math.round(R / 19));
  const along = it.h >= it.w;
  const m = mat(p.wood);
  for (let s = 0; s < n; s++) {
    const hgt = (R / n) * (s + 1);
    const t = (s + 0.5) / n - 0.5;
    const step = new THREE.Mesh(
      along ? new THREE.BoxGeometry(it.w * M, hgt * M, (it.h / n) * M) : new THREE.BoxGeometry((it.w / n) * M, hgt * M, it.h * M),
      m,
    );
    step.position.set(along ? 0 : t * it.w * M, (hgt / 2) * M, along ? -t * it.h * M : 0);
    g.add(step);
  }
}

/** A half-turn (U) staircase: in at the bottom-left, up along the bottom
 *  half to the right, turning at the right end, back along the top half
 *  to the left where it comes out. */
function stairsU(g, it, { mat, palette: p, rise }) {
  const R = it.height ?? rise;
  const n = Math.max(6, Math.round(R / 19));
  const m = mat(p.wood);
  const w = it.w;
  const d = it.h;
  const turnW = Math.min(d / 2, w * 0.4);
  const run = w - turnW;
  const nTurn = 4;
  const nRun = Math.floor((n - nTurn) / 2);
  let s = 0;
  const step = (bw, bd, x, z) => {
    const hgt = (R / n) * (s + 1);
    const mesh = new THREE.Mesh(new THREE.BoxGeometry(bw * M, hgt * M, bd * M), m);
    mesh.position.set(x * M, (hgt / 2) * M, z * M);
    g.add(mesh);
    s++;
  };
  for (let k = 0; k < nRun; k++) step(run / nRun, d / 2, -w / 2 + (run / nRun) * (k + 0.5), d / 4);
  for (let k = 0; k < nTurn; k++) step(turnW, d / nTurn, w / 2 - turnW / 2, d / 2 - (d / nTurn) * (k + 0.5));
  const back = n - s;
  for (let k = 0; k < back; k++) step(run / back, d / 2, w / 2 - turnW - (run / back) * (k + 0.5), -d / 4);
}
