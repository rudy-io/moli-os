// Small geometry for drawn floors (cm).

/** Area of a polygon, m². */
export function area(poly) {
  let s = 0;
  for (let i = 0; i < poly.length; i++) {
    const [x1, y1] = poly[i];
    const [x2, y2] = poly[(i + 1) % poly.length];
    s += x1 * y2 - x2 * y1;
  }
  return Math.abs(s) / 2 / 10000;
}

export function bbox(poly) {
  const xs = poly.map((p) => p[0]);
  const ys = poly.map((p) => p[1]);
  return { x: Math.min(...xs), y: Math.min(...ys), w: Math.max(...xs) - Math.min(...xs), h: Math.max(...ys) - Math.min(...ys) };
}

/** Whether (x, y) is inside the polygon (ray casting). */
export function inside(poly, x, y) {
  let hit = false;
  for (let i = 0, j = poly.length - 1; i < poly.length; j = i++) {
    const [xi, yi] = poly[i];
    const [xj, yj] = poly[j];
    if (yi > y !== yj > y && x < ((xj - xi) * (y - yi)) / (yj - yi) + xi) hit = !hit;
  }
  return hit;
}

/** Where a label goes: the middle of the polygon's largest box, nudged
 *  inside when the shape is an L. */
export function labelAt(poly) {
  const b = bbox(poly);
  const c = [b.x + b.w / 2, b.y + b.h / 2];
  if (inside(poly, c[0], c[1])) return c;
  // An L: try a grid of points, keep the one farthest from the edges' box.
  let best = c;
  let score = -1;
  for (let i = 1; i < 8; i++) {
    for (let j = 1; j < 8; j++) {
      const x = b.x + (b.w * i) / 8;
      const y = b.y + (b.h * j) / 8;
      if (!inside(poly, x, y)) continue;
      const s = Math.min(x - b.x, b.x + b.w - x, y - b.y, b.y + b.h - y);
      if (s > score) {
        score = s;
        best = [x, y];
      }
    }
  }
  return best;
}

/** `n` points spread inside a polygon (to place devices of a room). */
export function spread(poly, n) {
  const b = bbox(poly);
  const pad = 0.18;
  const cols = Math.max(1, Math.round(Math.sqrt((n * b.w) / Math.max(b.h, 1))));
  const rows = Math.ceil(n / cols);
  const out = [];
  for (let k = 0; k < n; k++) {
    const r = Math.floor(k / cols);
    const c = k % cols;
    let x = b.x + b.w * (pad + ((1 - 2 * pad) * (c + 0.5)) / cols);
    let y = b.y + b.h * (pad + ((1 - 2 * pad) * (r + 0.5)) / rows);
    if (!inside(poly, x, y)) [x, y] = labelAt(poly);
    out.push([Math.round(x), Math.round(y)]);
  }
  return out;
}

export const points = (poly) => poly.map(([x, y]) => `${x},${y}`).join(' ');
