// Materials of the « Réel » skin, drawn once on a canvas (nothing to
// download): floors (tiles, planks, grass…), wall prints, water, roof
// tiles. Each texture covers `size` metres and repeats; the caller sets
// `repeat` from it (ShapeGeometry's UVs are in metres).

import * as THREE from 'three';

/** A small seeded random: the same house always looks the same. */
function random(seed) {
  let s = seed >>> 0 || 1;
  return () => (s = (s * 1664525 + 1013904223) >>> 0) / 4294967296;
}

const hash = (text) => [...text].reduce((h, c) => (h * 31 + c.charCodeAt(0)) >>> 0, 7);

/** A colour a little lighter (k > 0) or darker (k < 0). */
const tone = (hex, k) => `#${new THREE.Color(hex).offsetHSL(0, 0, k).getHexString()}`;

function sheet(px = 256) {
  const c = document.createElement('canvas');
  c.width = px;
  c.height = px;
  return [c, c.getContext('2d')];
}

function finish(c, size) {
  const t = new THREE.CanvasTexture(c);
  t.colorSpace = THREE.SRGBColorSpace;
  t.wrapS = THREE.RepeatWrapping;
  t.wrapT = THREE.RepeatWrapping;
  t.anisotropy = 4;
  t.userData.size = size;
  return t;
}

/** Fine grain all over: no surface is perfectly flat. */
function grain(x, px, rnd, strength = 0.05, dots = 1800) {
  for (let i = 0; i < dots; i++) {
    x.fillStyle = rnd() < 0.5 ? `rgba(0,0,0,${strength * rnd()})` : `rgba(255,255,255,${strength * rnd()})`;
    const s = 1 + rnd() * 2;
    x.fillRect(rnd() * px, rnd() * px, s, s);
  }
}

/** Big tiles (80 cm), two by two, a joint around each. */
function tiles(x, px, rnd, tint) {
  const n = 2;
  const s = px / n;
  x.fillStyle = tone(tint, 0.06);
  x.fillRect(0, 0, px, px);
  for (let i = 0; i < n; i++) {
    for (let j = 0; j < n; j++) {
      x.fillStyle = tone(tint, (rnd() - 0.5) * 0.05);
      x.fillRect(i * s + 1, j * s + 1, s - 2, s - 2);
      // a mottled, stone-like face
      for (let k = 0; k < 40; k++) {
        const r = 6 + rnd() * 26;
        const g = x.createRadialGradient(0, 0, 0, 0, 0, r);
        g.addColorStop(0, rnd() < 0.5 ? 'rgba(0,0,0,0.035)' : 'rgba(255,255,255,0.05)');
        g.addColorStop(1, 'rgba(0,0,0,0)');
        x.save();
        x.translate(i * s + 1 + rnd() * (s - 2), j * s + 1 + rnd() * (s - 2));
        x.fillStyle = g;
        x.fillRect(-r, -r, 2 * r, 2 * r);
        x.restore();
      }
    }
  }
}

/** Planks (20 × 120 cm), staggered, with their grain. */
function planks(x, px, rnd, tint, rows = 6, gap = 1, gapColor = null) {
  const h = px / rows;
  for (let r = 0; r < rows; r++) {
    const shift = rnd() * px;
    for (let k = -1; k < 2; k++) {
      const x0 = shift + k * px;
      x.fillStyle = tone(tint, (rnd() - 0.5) * 0.09);
      x.fillRect(x0, r * h, px, h);
      for (let g = 0; g < 7; g++) {
        x.strokeStyle = `rgba(0,0,0,${0.03 + rnd() * 0.06})`;
        x.lineWidth = 0.6 + rnd();
        x.beginPath();
        const y = r * h + 2 + rnd() * (h - 4);
        x.moveTo(x0, y);
        x.bezierCurveTo(x0 + px * 0.3, y + (rnd() - 0.5) * 4, x0 + px * 0.6, y + (rnd() - 0.5) * 4, x0 + px, y);
        x.stroke();
      }
      x.fillStyle = gapColor ?? 'rgba(0,0,0,0.28)';
      x.fillRect(x0, r * h, gap, h);
    }
    x.fillStyle = gapColor ?? 'rgba(0,0,0,0.22)';
    x.fillRect(0, r * h + h - gap, px, gap);
  }
}

/** Square pavers (40 cm) for a terrace. */
function pavers(x, px, rnd, tint) {
  const n = 4;
  const s = px / n;
  x.fillStyle = tone(tint, -0.12);
  x.fillRect(0, 0, px, px);
  for (let i = 0; i < n; i++) {
    for (let j = 0; j < n; j++) {
      x.fillStyle = tone(tint, (rnd() - 0.5) * 0.1);
      x.fillRect(i * s + 2, j * s + 2, s - 4, s - 4);
    }
  }
  grain(x, px, rnd, 0.08, 2500);
}

/** Blades of grass (or a synthetic lawn: the same, more even). */
function grass(x, px, rnd, tint) {
  x.fillStyle = tint;
  x.fillRect(0, 0, px, px);
  for (let i = 0; i < 4200; i++) {
    x.strokeStyle = tone(tint, (rnd() - 0.45) * 0.2);
    x.lineWidth = 1;
    const gx = rnd() * px;
    const gy = rnd() * px;
    x.beginPath();
    x.moveTo(gx, gy);
    x.lineTo(gx + (rnd() - 0.5) * 3, gy - 2 - rnd() * 4);
    x.stroke();
  }
}

function gravel(x, px, rnd, tint) {
  x.fillStyle = tint;
  x.fillRect(0, 0, px, px);
  for (let i = 0; i < 2600; i++) {
    x.fillStyle = tone(tint, (rnd() - 0.5) * 0.25);
    x.beginPath();
    x.arc(rnd() * px, rnd() * px, 1 + rnd() * 2.2, 0, Math.PI * 2);
    x.fill();
  }
}

/** Where a floor kind repeats (m) and how it is drawn. */
const FLOORS = {
  tile: [1.6, tiles],
  wood: [1.2, (x, px, rnd, t) => planks(x, px, rnd, t)],
  wood_dark: [1.2, (x, px, rnd, t) => planks(x, px, rnd, t)],
  concrete: [2, (x, px, rnd, t) => {
    x.fillStyle = t;
    x.fillRect(0, 0, px, px);
    grain(x, px, rnd, 0.06, 4000);
  }],
  stone: [1.6, pavers],
  carpet: [0.5, (x, px, rnd, t) => {
    x.fillStyle = t;
    x.fillRect(0, 0, px, px);
    grain(x, px, rnd, 0.07, 6000);
  }],
  grass: [1, grass],
  deck: [1.4, (x, px, rnd, t) => planks(x, px, rnd, t, 10, 2, 'rgba(40,30,20,0.55)')],
  gravel: [0.8, gravel],
};

/** The default colour of each floor kind (when the room gives no tint). */
export const FLOOR_TINT = {
  tile: '#c4c3bf', wood: '#a87b52', wood_dark: '#6e4a31', concrete: '#a9a7a2', stone: '#c9b89c',
  carpet: '#d8cfbf', grass: '#6f9a47', deck: '#a88b66', gravel: '#b9b2a5',
};

/** What a room is floored with, when it does not say. */
export const FLOOR_OF = {
  living: 'tile', kitchen: 'tile', hall: 'tile', veranda: 'tile', bath: 'tile', wc: 'tile',
  bedroom: 'wood', office: 'wood', storage: 'concrete', garage: 'concrete', other: 'tile',
  garden: 'grass', deck: 'deck', terrace: 'stone', driveway: 'gravel', roof: 'gravel',
};

export function floorTexture(kind, tint) {
  const [size, draw] = FLOORS[kind] ?? FLOORS.tile;
  const [c, x] = sheet();
  const rnd = random(hash(kind + tint));
  draw(x, 256, rnd, tint);
  if (kind !== 'grass' && kind !== 'gravel') grain(x, 256, rnd, 0.035);
  return finish(c, size);
}

/** A wall's print: big leaves (a jungle wallpaper) or stripes; 1 m wide. */
export function wallTexture(pattern, color) {
  const [c, x] = sheet();
  const rnd = random(hash(pattern + color));
  if (pattern === 'stripes') {
    x.fillStyle = tone(color, 0.32);
    x.fillRect(0, 0, 256, 256);
    x.fillStyle = color;
    for (let i = 0; i < 8; i += 2) x.fillRect(i * 32, 0, 32, 256);
    return finish(c, 1);
  }
  x.fillStyle = tone(color, 0.38);
  x.fillRect(0, 0, 256, 256);
  for (let i = 0; i < 26; i++) {
    const cx = rnd() * 256;
    const cy = rnd() * 256;
    const len = 40 + rnd() * 70;
    const a = rnd() * Math.PI * 2;
    x.save();
    x.translate(cx, cy);
    x.rotate(a);
    x.fillStyle = tone(color, (rnd() - 0.6) * 0.22);
    x.globalAlpha = 0.55 + rnd() * 0.35;
    x.beginPath();
    x.moveTo(0, 0);
    x.quadraticCurveTo(len * 0.5, -len * 0.32, len, 0);
    x.quadraticCurveTo(len * 0.5, len * 0.32, 0, 0);
    x.fill();
    x.restore();
  }
  x.globalAlpha = 1;
  return finish(c, 1);
}

/** Water: its colour, light ripples. */
export function waterTexture(color) {
  const [c, x] = sheet();
  const rnd = random(hash(color));
  x.fillStyle = color;
  x.fillRect(0, 0, 256, 256);
  x.strokeStyle = 'rgba(255,255,255,0.18)';
  for (let i = 0; i < 70; i++) {
    x.lineWidth = 0.8 + rnd() * 1.4;
    const y = rnd() * 256;
    const x0 = rnd() * 256;
    x.beginPath();
    x.moveTo(x0, y);
    x.bezierCurveTo(x0 + 12, y - 5, x0 + 26, y + 5, x0 + 40, y);
    x.stroke();
  }
  return finish(c, 1.5);
}

/** Rows of round clay tiles, each a little different; 0.6 m. */
export function roofTexture(color, varied) {
  const [c, x] = sheet(128);
  const rnd = random(hash(color));
  x.fillStyle = tone(color, -0.12);
  x.fillRect(0, 0, 128, 128);
  for (let row = 0; row < 6; row++) {
    for (let col = 0; col < 6; col++) {
      const w = 128 / 6;
      const cx = col * w + (row % 2) * (w / 2) + w / 2;
      const base = varied ? tone(color, (rnd() - 0.5) * 0.12) : color;
      for (const dx of [-128, 0, 128]) {
        const g = x.createLinearGradient(cx + dx - w / 2, 0, cx + dx + w / 2, 0);
        g.addColorStop(0, tone(base, -0.14));
        g.addColorStop(0.45, tone(base, 0.08));
        g.addColorStop(1, tone(base, -0.18));
        x.fillStyle = g;
        x.fillRect(cx + dx - w / 2 + 0.5, row * w, w - 1, w - 1.5);
      }
    }
  }
  return finish(c, 0.6);
}
