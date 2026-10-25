// The temperatures of the house, room by room, over time (the
// Températures page). Pure functions: which devices measure a room, a
// history series (/api/history) as a line, the scale the rooms share, and
// the value at a moment.

const KEYS = ['temperature', 'temp_current'];

/** The point that holds a device's temperature, or null. */
export function tempKey(d) {
  return KEYS.find((k) => d?.points?.some((p) => p.key === k)) ?? null;
}

/** What measures a room, as its card says its temperature: its sensors,
 *  else its heaters' and units' own probes. `[{ id, point }]`. */
export function probesOf(room) {
  const of = (list) => list.filter((d) => tempKey(d)).map((d) => ({ id: d.id, point: `${d.id}/${tempKey(d)}` }));
  const sensed = of(room.sensors ?? []);
  return sensed.length ? sensed : of(room.climates ?? []);
}

/** A history series as `[[ts, °C]…]` in time order: the averages of its
 *  buckets, else its changes as steps, from the value it had when the
 *  window opened to the value it still has at its end. */
export function lineOf(series) {
  if (!series) return [];
  if (series.buckets?.length) {
    const line = series.buckets.filter((b) => Number.isFinite(b.avg)).map((b) => [b.ts, b.avg]);
    // Still that value now.
    if (line.length && typeof series.last?.[1] === 'number') line.push([series.to, series.last[1]]);
    return line;
  }
  let raw = (series.raw ?? []).filter(([, v]) => typeof v === 'number');
  if (series.before && typeof series.before[1] === 'number') raw.unshift([series.from, series.before[1]]);
  raw = withoutBlips(raw);
  if (raw.length) raw.push([series.to, raw.at(-1)[1]]);
  // Recorded at each change: a value holds until the next one (a step,
  // never a slope between two readings hours apart).
  return raw.flatMap((p, i) => (i && raw[i - 1][1] !== p[1] ? [[p[0], raw[i - 1][1]], p] : [p]));
}

/** A reading held less than a minute that falls straight back to the one
 *  before is a glitch, not the room (a radiator module says 20° for four
 *  seconds every ten minutes): left out. */
const BLIP_MS = 60_000;
function withoutBlips(raw) {
  const out = [];
  for (let i = 0; i < raw.length; i++) {
    const [prev, next] = [out.at(-1), raw[i + 1]];
    if (prev && next && next[0] - raw[i][0] < BLIP_MS && next[1] === prev[1]) {
      i++; // the return to the value before: nothing changed
      continue;
    }
    out.push(raw[i]);
  }
  return out;
}

/** The lowest, highest and last value of a line, or null. */
export function statsOf(line) {
  if (!line.length) return null;
  const vs = line.map(([, v]) => v);
  return { min: Math.min(...vs), max: Math.max(...vs), last: vs.at(-1) };
}

/** A scale for every room: whole degrees around what all lines reach
 *  (at least 4 °C tall), and its ticks every 1, 2 or 5 °C. */
export function scaleOf(lines) {
  const vs = lines.flatMap((l) => l.map(([, v]) => v));
  if (!vs.length) return { min: 15, max: 25, ticks: [15, 20, 25] };
  let min = Math.floor(Math.min(...vs) - 0.5);
  let max = Math.ceil(Math.max(...vs) + 0.5);
  if (max - min < 4) {
    const mid = (min + max) / 2;
    min = Math.floor(mid - 2);
    max = min + 4;
  }
  const step = [1, 2, 5, 10].find((s) => (max - min) / s <= 5) ?? 10;
  min = Math.floor(min / step) * step;
  max = Math.ceil(max / step) * step;
  const ticks = [];
  for (let v = min; v <= max + 1e-9; v += step) ticks.push(v);
  return { min, max, ticks };
}

/** A line's value at `ts`: the nearest point no further than `gap` ms. */
export function valueAt(line, ts, gap) {
  if (!line.length) return null;
  let lo = 0;
  let hi = line.length - 1;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (line[mid][0] < ts) lo = mid + 1;
    else hi = mid;
  }
  const near = [line[lo - 1], line[lo]].filter(Boolean).sort((a, b) => Math.abs(a[0] - ts) - Math.abs(b[0] - ts))[0];
  return Math.abs(near[0] - ts) <= gap ? near[1] : null;
}
