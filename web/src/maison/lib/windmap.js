// The weather map, drawn by the dashboard itself (the Windy look): the land
// in relief, the clouds as a soft veil, the rain in colours, and the wind as
// streams of particles. Everything is smoothed between the grid's points:
// sharp at any size, no images, no flashes.
//
// Data: `/api/weather/map` (the area, a weather grid hour by hour, the
// land's elevations). A view is a window on the area, in fractions (0..1),
// so a narrow box shows a band of the map around the house.

const clamp = (v, a, b) => (v < a ? a : v > b ? b : v);
const smooth = (a, b, x) => {
  const t = clamp((x - a) / (b - a), 0, 1);
  return t * t * (3 - 2 * t);
};
const mix = (a, b, t) => a + (b - a) * t;

/** Bilinear sample of a row-major grid (`nx` × `ny`) at fractions fx, fy. */
function sample(values, nx, ny, fx, fy, offset = 0) {
  const x = clamp(fx, 0, 1) * (nx - 1);
  const y = clamp(fy, 0, 1) * (ny - 1);
  const i = Math.min(Math.floor(x), nx - 2);
  const j = Math.min(Math.floor(y), ny - 2);
  const tx = x - i;
  const ty = y - j;
  const k = offset + j * nx + i;
  const a = values[k];
  const b = values[k + 1];
  const c = values[k + nx];
  const d = values[k + nx + 1];
  return mix(mix(a, b, tx), mix(c, d, tx), ty);
}

// Value noise (fixed seed): the texture that makes a cloud look like one.
const NOISE = 64;
const lattice = (() => {
  let s = 1234567;
  const r = new Float32Array(NOISE * NOISE);
  for (let i = 0; i < r.length; i++) {
    s = (s * 16807) % 2147483647;
    r[i] = s / 2147483647;
  }
  return r;
})();
function noise(x, y) {
  const xi = Math.floor(x);
  const yi = Math.floor(y);
  const tx = x - xi;
  const ty = y - yi;
  const sx = tx * tx * (3 - 2 * tx);
  const sy = ty * ty * (3 - 2 * ty);
  const at = (i, j) => lattice[(((j % NOISE) + NOISE) % NOISE) * NOISE + (((i % NOISE) + NOISE) % NOISE)];
  return mix(mix(at(xi, yi), at(xi + 1, yi), sx), mix(at(xi, yi + 1), at(xi + 1, yi + 1), sx), sy);
}
const fbm = (x, y) => noise(x, y) * 0.55 + noise(x * 2.1, y * 2.1) * 0.3 + noise(x * 4.3, y * 4.3) * 0.15;

// The land's colours by height (metres), dark like a night chart.
const LAND = [
  [0, [34, 46, 38]],
  [250, [44, 54, 42]],
  [800, [58, 62, 52]],
  [1600, [84, 84, 76]],
  [2400, [128, 126, 120]],
  [3200, [170, 170, 166]],
];
function landColour(e) {
  for (let i = 1; i < LAND.length; i++) {
    if (e <= LAND[i][0]) {
      const t = (e - LAND[i - 1][0]) / (LAND[i][0] - LAND[i - 1][0]);
      return LAND[i - 1][1].map((c, k) => mix(c, LAND[i][1][k], t));
    }
  }
  return LAND.at(-1)[1];
}
const SEA = [18, 38, 58];
const SEA_DEEP = [7, 15, 27];

// Rain (mm/h): light blue, green, yellow, red, like a radar.
const RAIN = [
  [0.1, [90, 150, 255, 0.35]],
  [1, [70, 200, 140, 0.55]],
  [4, [240, 210, 70, 0.7]],
  [10, [240, 90, 70, 0.8]],
];
function rainColour(mm) {
  if (mm < RAIN[0][0]) return null;
  for (let i = 1; i < RAIN.length; i++) {
    if (mm <= RAIN[i][0]) {
      const t = (mm - RAIN[i - 1][0]) / (RAIN[i][0] - RAIN[i - 1][0]);
      return RAIN[i - 1][1].map((c, k) => mix(c, RAIN[i][1][k], t));
    }
  }
  return RAIN.at(-1)[1];
}

// The wind's colours by speed (km/h): calm white, then warmer.
const WIND_BANDS = [
  [15, 'rgba(255,255,255,0.26)'],
  [30, 'rgba(215,236,255,0.36)'],
  [50, 'rgba(255,214,120,0.5)'],
  [Infinity, 'rgba(255,120,92,0.65)'],
];

export class WindMap {
  /** `land` and `air`: two stacked canvases of the same box. */
  constructor(land, air, data, { still = false } = {}) {
    this.landCanvas = land;
    this.airCanvas = air;
    this.data = data;
    this.still = still;
    this.view = { x0: 0, y0: 0, x1: 1, y1: 1 };
    this.hour = 0;
    this.drawnHour = null;
    this.particles = [];
    this.running = false;
    this.frame = this.frame.bind(this);
  }

  /** The box's size (CSS pixels) and the window of the map it shows. */
  resize(width, height, view = this.view) {
    if (!width || !height) return;
    this.view = view;
    this.w = width;
    this.h = height;
    this.dpr = Math.min(window.devicePixelRatio || 1, 2);
    for (const c of [this.landCanvas, this.airCanvas]) {
      c.width = Math.round(width * this.dpr);
      c.height = Math.round(height * this.dpr);
    }
    this.base = null;
    this.drawnHour = null;
    const count = clamp(Math.round((width * height) / 3200), 50, 600);
    this.particles = Array.from({ length: count }, () => this.spawn({}));
    this.draw();
  }

  setData(data) {
    this.data = data;
    this.base = null;
    this.drawnHour = null;
    this.draw();
  }

  /** The hour shown: 0 = now, fractions in between (smooth). */
  setHour(hour) {
    this.hour = hour;
    this.draw();
  }

  start() {
    if (this.running || this.still) return;
    this.running = true;
    requestAnimationFrame(this.frame);
  }

  stop() {
    this.running = false;
  }

  // Box pixel → area fraction.
  fx(x) {
    return this.view.x0 + (x / this.w) * (this.view.x1 - this.view.x0);
  }

  fy(y) {
    return this.view.y0 + (y / this.h) * (this.view.y1 - this.view.y0);
  }

  hours() {
    return this.data?.weather?.hours?.length ?? 0;
  }

  /** A weather value at the shown hour, smoothed between hours. */
  at(key, fx, fy) {
    const w = this.data.weather;
    const [nx, ny] = w.grid;
    const n = nx * ny;
    const last = this.hours() - 1;
    const h = clamp(this.hour, 0, last);
    const h0 = Math.floor(h);
    const h1 = Math.min(h0 + 1, last);
    const a = sample(w[key], nx, ny, fx, fy, h0 * n);
    return h1 === h0 ? a : mix(a, sample(w[key], nx, ny, fx, fy, h1 * n), h - h0);
  }

  spawn(p) {
    p.x = Math.random() * this.w;
    p.y = Math.random() * this.h;
    p.age = Math.floor(Math.random() * 90);
    return p;
  }

  /** The land (decoded tiles, `loadLand`), then the base is redrawn. */
  setLand(land) {
    this.land = land;
    this.base = null;
    this.drawnHour = null;
    this.draw();
  }

  /** Elevation (metres, negative at sea) at area fractions, from the tiles. */
  elevation(fx, fy) {
    const { area } = this.data;
    const lon = area.west + fx * (area.east - area.west);
    const lat = area.north - fy * (area.north - area.south);
    return this.land.at(lon, lat);
  }

  /** The land, drawn once per size (half resolution, smoothed up). */
  drawBase() {
    const scale = 2;
    const bw = Math.max(2, Math.round((this.w * this.dpr) / scale));
    const bh = Math.max(2, Math.round((this.h * this.dpr) / scale));
    const base = new OffscreenCanvas(bw, bh);
    const ctx = base.getContext('2d');
    const img = ctx.createImageData(bw, bh);
    const px = img.data;
    // One base pixel, in area fractions: the step of the hill shading.
    const sx = (this.view.x1 - this.view.x0) / bw;
    const sy = (this.view.y1 - this.view.y0) / bh;
    // Metres a base pixel spans: slopes read the same at any zoom.
    const metres = ((this.data.km?.[0] ?? 400) * 1000 * sx) || 1000;
    for (let y = 0; y < bh; y++) {
      const fy = this.fy((y / bh) * this.h);
      for (let x = 0; x < bw; x++) {
        const fx = this.fx((x / bw) * this.w);
        let rgb = SEA;
        if (this.land) {
          const e = this.elevation(fx, fy);
          const t = smooth(-1, 6, e);
          // The sea: lighter near the shore, darker in the deep.
          const depth = smooth(0, -900, e);
          const sea = SEA.map((c, k) => mix(c, SEA_DEEP[k], depth));
          const dzx = (this.elevation(fx + sx, fy) - this.elevation(fx - sx, fy)) / (2 * metres);
          const dzy = (this.elevation(fx, fy + sy) - this.elevation(fx, fy - sy)) / (2 * metres);
          // Light from the north-west.
          const shade = clamp(1 + (-dzx - dzy) * 2.2, 0.55, 1.5);
          const ground = landColour(Math.max(0, e)).map((c) => c * shade);
          rgb = sea.map((c, k) => mix(c, ground[k], t));
          // The coast, a fine light line.
          const coast = 1 - Math.abs(t - 0.5) * 2;
          rgb = rgb.map((c) => c + coast * coast * 46);
        }
        const o = (y * bw + x) * 4;
        px[o] = rgb[0];
        px[o + 1] = rgb[1];
        px[o + 2] = rgb[2];
        px[o + 3] = 255;
      }
    }
    ctx.putImageData(img, 0, 0);
    this.base = base;
  }

  /** Land, then clouds and rain at the shown hour (third resolution). */
  draw() {
    if (!this.w || !this.data?.weather?.hours?.length) return;
    if (!this.base) this.drawBase();
    if (this.drawnHour != null && Math.abs(this.drawnHour - this.hour) < 0.02) return;
    this.drawnHour = this.hour;
    const scale = 3;
    const ow = Math.max(2, Math.round((this.w * this.dpr) / scale));
    const oh = Math.max(2, Math.round((this.h * this.dpr) / scale));
    const sky = new OffscreenCanvas(ow, oh);
    const sctx = sky.getContext('2d');
    const img = sctx.createImageData(ow, oh);
    const px = img.data;
    // The texture drifts with the wind over the hours (km/h → map widths).
    const [kmW, kmH] = this.data.km ?? [400, 300];
    const drift = this.meanWind();
    const dx = (drift[0] * this.hour) / kmW;
    const dy = (-drift[1] * this.hour) / kmH;
    for (let y = 0; y < oh; y++) {
      const fy = this.fy((y / oh) * this.h);
      for (let x = 0; x < ow; x++) {
        const fx = this.fx((x / ow) * this.w);
        const cover = this.at('cloud', fx, fy) / 100;
        const n = fbm((fx - dx) * 9, (fy - dy) * 7);
        let a = clamp(cover * 1.35 - 0.45 + (n - 0.5) * 0.9, 0, 1);
        a = a * a * (3 - 2 * a) * 0.82;
        let r = 236;
        let g = 240;
        let b = 246;
        const mm = this.at('rain', fx, fy);
        const rain = rainColour(mm);
        if (rain) {
          const ra = rain[3] * clamp(0.6 + n * 0.6, 0, 1);
          const out = ra + a * (1 - ra);
          r = (rain[0] * ra + r * a * (1 - ra)) / out;
          g = (rain[1] * ra + g * a * (1 - ra)) / out;
          b = (rain[2] * ra + b * a * (1 - ra)) / out;
          a = out;
        }
        const o = (y * ow + x) * 4;
        px[o] = r;
        px[o + 1] = g;
        px[o + 2] = b;
        px[o + 3] = a * 255;
      }
    }
    sctx.putImageData(img, 0, 0);
    const ctx = this.landCanvas.getContext('2d');
    ctx.imageSmoothingEnabled = true;
    ctx.imageSmoothingQuality = 'high';
    const W = this.landCanvas.width;
    const H = this.landCanvas.height;
    ctx.drawImage(this.base, 0, 0, W, H);
    ctx.drawImage(sky, 0, 0, W, H);
    if (this.still) this.drawStill();
  }

  meanWind() {
    const w = this.data.weather;
    const n = w.grid[0] * w.grid[1];
    const h = clamp(Math.round(this.hour), 0, this.hours() - 1) * n;
    let u = 0;
    let v = 0;
    for (let i = 0; i < n; i++) {
      u += w.u[h + i];
      v += w.v[h + i];
    }
    return [u / n, v / n];
  }

  /** Pixels per frame for a speed: the stream reads well at any size. */
  speed() {
    const kmAcross = (this.data.km?.[0] ?? 400) * (this.view.x1 - this.view.x0);
    return (this.w / kmAcross) * 0.022;
  }

  /** Without motion: short strokes along the wind, drawn once. */
  drawStill() {
    const ctx = this.airCanvas.getContext('2d');
    ctx.setTransform(this.dpr, 0, 0, this.dpr, 0, 0);
    ctx.clearRect(0, 0, this.w, this.h);
    ctx.lineWidth = 1;
    ctx.strokeStyle = 'rgba(255,255,255,0.5)';
    ctx.beginPath();
    const k = this.speed() * 14;
    for (const p of this.particles.slice(0, 400)) {
      const fx = this.fx(p.x);
      const fy = this.fy(p.y);
      ctx.moveTo(p.x, p.y);
      ctx.lineTo(p.x + this.at('u', fx, fy) * k, p.y - this.at('v', fx, fy) * k);
    }
    ctx.stroke();
  }

  frame() {
    if (!this.running) return;
    requestAnimationFrame(this.frame);
    if (!this.w || !this.data?.weather?.hours?.length) return;
    const ctx = this.airCanvas.getContext('2d');
    ctx.setTransform(this.dpr, 0, 0, this.dpr, 0, 0);
    // The trails fade a little each frame.
    ctx.globalCompositeOperation = 'destination-in';
    ctx.fillStyle = 'rgba(0,0,0,0.94)';
    ctx.fillRect(0, 0, this.w, this.h);
    ctx.globalCompositeOperation = 'source-over';
    ctx.lineWidth = 0.85;
    ctx.lineCap = 'round';
    const k = this.speed();
    const paths = WIND_BANDS.map(() => new Path2D());
    for (const p of this.particles) {
      const fx = this.fx(p.x);
      const fy = this.fy(p.y);
      const u = this.at('u', fx, fy);
      const v = this.at('v', fx, fy);
      const s = Math.hypot(u, v);
      const nx = p.x + u * k;
      const ny = p.y - v * k;
      if (p.age++ > 100 || nx < 0 || ny < 0 || nx > this.w || ny > this.h) {
        this.spawn(p);
        continue;
      }
      const band = WIND_BANDS.findIndex(([max]) => s < max);
      paths[band].moveTo(p.x, p.y);
      paths[band].lineTo(nx, ny);
      p.x = nx;
      p.y = ny;
    }
    paths.forEach((path, i) => {
      ctx.strokeStyle = WIND_BANDS[i][1];
      ctx.stroke(path);
    });
  }
}

/** The window of the map that fills a `width` × `height` box with the
 *  house at (tx, ty) of it, never showing past the map's edges. */
export function viewAround(map, width, height, tx = 0.5, ty = 0.5) {
  const [kmW, kmH] = map?.km ?? [400, 300];
  const [hx, hy] = map?.house ?? [0.5, 0.5];
  const aspect = kmW / kmH;
  // Map size in box pixels (mw × mh), as small as covering allows.
  let mw = Math.max(width, height * aspect);
  mw = Math.max(mw, (tx * width) / hx, ((1 - tx) * width) / (1 - hx));
  mw = Math.max(mw, ((ty * height) / hy) * aspect, (((1 - ty) * height) / (1 - hy)) * aspect);
  const mh = mw / aspect;
  const x0 = hx - (tx * width) / mw;
  const y0 = hy - (ty * height) / mh;
  return { x0, y0, x1: x0 + width / mw, y1: y0 + height / mh };
}

/** The land's tiles (Terrarium PNG: elevation = R·256 + G + B/256 − 32768)
 *  decoded into one grid of metres; `at(lon, lat)` reads it (bilinear). */
export async function loadLand(land) {
  const cols = land.x1 - land.x0 + 1;
  const rows = land.y1 - land.y0 + 1;
  const size = 256;
  const canvas = new OffscreenCanvas(cols * size, rows * size);
  const ctx = canvas.getContext('2d', { willReadFrequently: true });
  await Promise.all(
    Array.from({ length: cols * rows }, async (_, i) => {
      const x = land.x0 + (i % cols);
      const y = land.y0 + Math.floor(i / cols);
      const res = await fetch(`/api/weather/land/${land.z}/${x}/${y}.png`);
      if (!res.ok) throw new Error(`land ${x}/${y}: ${res.status}`);
      const bitmap = await createImageBitmap(await res.blob(), { colorSpaceConversion: 'none', premultiplyAlpha: 'none' });
      ctx.drawImage(bitmap, (x - land.x0) * size, (y - land.y0) * size);
    }),
  );
  const w = cols * size;
  const h = rows * size;
  const rgba = ctx.getImageData(0, 0, w, h).data;
  const elev = new Float32Array(w * h);
  for (let i = 0; i < elev.length; i++) elev[i] = rgba[i * 4] * 256 + rgba[i * 4 + 1] + rgba[i * 4 + 2] / 256 - 32768;
  const n = 2 ** land.z;
  return {
    at(lon, lat) {
      const r = (lat * Math.PI) / 180;
      const px = (((lon + 180) / 360) * n - land.x0) * size;
      const py = (((1 - Math.log(Math.tan(r) + 1 / Math.cos(r)) / Math.PI) / 2) * n - land.y0) * size;
      const x = clamp(px - 0.5, 0, w - 1.001);
      const y = clamp(py - 0.5, 0, h - 1.001);
      const i = Math.floor(x);
      const j = Math.floor(y);
      const tx = x - i;
      const ty = y - j;
      const k = j * w + i;
      return mix(mix(elev[k], elev[k + 1], tx), mix(elev[k + w], elev[k + w + 1], tx), ty);
    },
  };
}