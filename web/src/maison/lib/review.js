// A camera's past, from its NVR (/api/devices/{id}/review…): what it saw
// (review items: alerts and detections), what it recorded (continuous
// stretches), and where on the frieze of one day each one falls. Times are
// Unix seconds, as the NVR gives them.

/** How much video one press plays: the browser fetches it whole (the NVR
 *  makes the MP4 on the fly, no `Range`), then seeks freely inside. */
export const WINDOW = 600;

/** Seconds of video before an item starts, so its beginning is seen. */
export const LEAD = 4;

/** The local day holding `t` (seconds), as [start, end). */
export function dayOf(t) {
  const d = new Date(t * 1000);
  const start =
    new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime() / 1000;
  const next =
    new Date(d.getFullYear(), d.getMonth(), d.getDate() + 1).getTime() / 1000;
  return [start, next];
}

/** Where `t` falls on the frieze of [after, before), from 0 to 1. */
export function at(t, after, before) {
  return Math.min(1, Math.max(0, (t - after) / (before - after)));
}

/** The time at `fraction` (0–1) of the frieze. */
export function timeAt(fraction, after, before) {
  return after + Math.min(1, Math.max(0, fraction)) * (before - after);
}

/** Recorded stretches as bars of the frieze (fractions), those closer than
 *  `join` (a fraction: about a pixel) drawn as one. */
export function bars(recorded, after, before, join = 0.001) {
  const out = [];
  for (const r of [...recorded].sort((a, b) => a.start - b.start)) {
    if (r.end <= after || r.start >= before) continue;
    const left = at(r.start, after, before);
    const right = at(r.end, after, before);
    const last = out.at(-1);
    if (last && left - (last.left + last.width) <= join) {
      last.width = Math.max(last.width, right - last.left);
    } else {
      out.push({ left, width: right - left });
    }
  }
  return out;
}

/** Whether something was recorded at `t`; if not, the start of the next
 *  stretch (null when nothing comes after). */
export function playable(t, recorded) {
  let next = null;
  for (const r of recorded) {
    if (r.start <= t && t < r.end) return t;
    if (r.start > t && (next === null || r.start < next)) next = r.start;
  }
  return next;
}

/** The video to fetch to show `t`: from `t` on, `WINDOW` long, never past
 *  `now` (the NVR has not written it yet). */
export function windowFrom(t, now) {
  const start = Math.floor(t);
  const end = Math.min(start + WINDOW, Math.floor(now) - 2);
  return end > start + 1 ? { start, end } : null;
}

/** The video of one item: a little before it, to its end (or the window). */
export function windowOf(item, now) {
  const start = Math.floor(item.start - LEAD);
  const stop = item.end ?? item.start + 60;
  const end = Math.min(
    Math.ceil(stop + LEAD),
    start + WINDOW,
    Math.floor(now) - 2,
  );
  return end > start + 1 ? { start, end } : null;
}

/** The items to list: newest first, those with `label` only when given. */
export function listed(items, label = null) {
  return [...items]
    .filter((i) => !label || i.labels.includes(label))
    .sort((a, b) => b.start - a.start);
}

/** Every label seen, most frequent first, for the filter. */
export function labelsOf(items) {
  const count = new Map();
  for (const i of items)
    for (const l of i.labels) count.set(l, (count.get(l) ?? 0) + 1);
  return [...count.entries()]
    .sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]))
    .map(([l]) => l);
}

/** Hour marks of the frieze, every `step` hours: [{ fraction, hour }]. */
export function hours(after, before, step = 3) {
  const out = [];
  for (let t = after; t <= before; t += 3600) {
    const h = new Date(t * 1000).getHours();
    if (h % step === 0) out.push({ fraction: at(t, after, before), hour: h });
  }
  return out;
}

/** A zone's outline as SVG points in a `w`×`h` picture. */
export function outline(zone, w, h) {
  return zone.points
    .map(([x, y]) => `${(x * w).toFixed(1)},${(y * h).toFixed(1)}`)
    .join(' ');
}

/** A Frigate name as people read it (`cour_interieure` → `cour interieure`). */
export function readable(name) {
  return name.replaceAll('_', ' ');
}

/** The newest items of several cameras, mixed: [{ camera, item }]. */
export function latest(byCamera, n = 20) {
  const out = [];
  for (const { camera, items } of byCamera) for (const item of items) out.push({ camera, item });
  return out.sort((a, b) => b.item.start - a.item.start).slice(0, n);
}
