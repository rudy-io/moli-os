// The solar forecast (the `solcast` driver's `curve` point) set against what
// the panels made (the energy series, meters of role `solar`), hour by hour.
// Pure: the page gives the devices, the series and the time.
//
// `curve`: {"start": ms, "step": 1800, "kw": [...], "lo": [...], "hi": [...]},
// from yesterday's midnight to tomorrow's end, kW per half-hour, null where
// nothing is known.

const HOUR = 3_600_000;

/** The forecast device: the first one that carries a curve. */
export function forecastDevice(devices, value) {
  for (const d of Object.values(devices ?? {})) {
    if (d?.manufacturer === 'Solcast' && typeof value(d.id, 'curve') === 'string') return d;
  }
  return null;
}

/** The curve as half-hours `{ start, end, kw, lo, hi }` (ms), or null. */
export function parseCurve(text) {
  let c;
  try {
    c = JSON.parse(text);
  } catch {
    return null;
  }
  if (!c || !Number.isFinite(c.start) || !Number.isFinite(c.step) || c.step <= 0 || !Array.isArray(c.kw)) return null;
  const step = c.step * 1000;
  const out = [];
  c.kw.forEach((kw, i) => {
    if (typeof kw !== 'number') return;
    const lo = typeof c.lo?.[i] === 'number' ? c.lo[i] : kw;
    const hi = typeof c.hi?.[i] === 'number' ? c.hi[i] : kw;
    out.push({ start: c.start + i * step, end: c.start + (i + 1) * step, kw, lo, hi });
  });
  return out;
}

/** The forecast by hour between `from` and `to` (ms): `{ start, kwh, lo, hi }`,
 *  only the hours it speaks of. */
export function forecastHours(slots, from, to) {
  const hours = new Map();
  for (const s of slots ?? []) {
    if (s.end <= from || s.start >= to) continue;
    const h = Math.floor(s.start / HOUR) * HOUR;
    const share = (s.end - s.start) / HOUR;
    const cur = hours.get(h) ?? { start: h, kwh: 0, lo: 0, hi: 0 };
    cur.kwh += s.kw * share;
    cur.lo += s.lo * share;
    cur.hi += s.hi * share;
    hours.set(h, cur);
  }
  return [...hours.values()].sort((a, b) => a.start - b.start);
}

/** What the panels made, by hour start (ms): the meters of role `solar`
 *  summed. `series`: the answer of /api/energy/series?step=hour. */
export function producedHours(series) {
  const solar = new Set((series?.meters ?? []).filter((m) => m.role === 'solar').map((m) => m.id));
  const out = new Map();
  if (!solar.size) return out;
  for (const b of series?.buckets ?? []) {
    let kwh = 0;
    let any = false;
    for (const [id, m] of Object.entries(b.meters ?? {})) {
      if (solar.has(id) && typeof m?.kwh === 'number') {
        kwh += m.kwh;
        any = true;
      }
    }
    if (any) out.set(b.start, kwh);
  }
  return out;
}

/** The local day holding `now` (ms), as [start, end). */
export function dayOf(now) {
  const d = new Date(now);
  const start = new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
  const end = new Date(d.getFullYear(), d.getMonth(), d.getDate() + 1).getTime();
  return [start, end];
}

/** The hours worth drawing: from the first to the last that produce,
 *  forecast or real, an hour of margin each side. */
export function lightWindow(hours, produced, [from, to]) {
  const lit = [
    ...hours.filter((h) => h.hi > 0.005).map((h) => h.start),
    ...[...produced.entries()].filter(([s, kwh]) => s >= from && s < to && kwh > 0.005).map(([s]) => s),
  ];
  if (!lit.length) return null;
  return [Math.max(from, Math.min(...lit) - HOUR), Math.min(to, Math.max(...lit) + 2 * HOUR)];
}

/** Today so far: what was forecast up to `now` (the current hour pro rata)
 *  against what was made, and a verdict: `above`, `within` (inside the low
 *  and high estimates, or less than 10 % apart), `below`; null before there
 *  is enough to say (less than 0.3 kWh expected). */
export function soFar(hours, produced, now) {
  let expected = 0;
  let lo = 0;
  let hi = 0;
  let made = 0;
  for (const h of hours) {
    if (h.start >= now) continue;
    const part = Math.min(1, (now - h.start) / HOUR);
    expected += h.kwh * part;
    lo += h.lo * part;
    hi += h.hi * part;
    made += produced.get(h.start) ?? 0;
  }
  if (expected < 0.3) return null;
  const pct = Math.round((made / expected - 1) * 100);
  const verdict = Math.abs(pct) < 10 || (made >= lo && made <= hi) ? 'within' : pct > 0 ? 'above' : 'below';
  return { expected, made, pct, verdict };
}
