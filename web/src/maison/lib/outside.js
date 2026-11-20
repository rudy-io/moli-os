// « Dehors » (#210): the weather outside as one device, `dehors:meteo`, fed
// meaning by meaning by the stations (measures), else the forecast. These
// helpers are pure: the page gives them the devices.

/** The device Moli builds when `[outdoor]` is declared. */
export const OUTSIDE = 'dehors:meteo';

/** An outside value for a meaning: `dehors:meteo`'s when it has one (it
 *  already chose the fresh measure or the forecast), else the forecast's own
 *  point, as before `[outdoor]`. Decides nothing itself. `read(id, key)`
 *  reads a point (the store's `value`). */
export function readOutside(read, key, forecast, forecastKey = key) {
  const v = read(OUTSIDE, key);
  return v != null ? v : read(forecast, forecastKey);
}

/** The ids in a comma-separated config point (`stations`, `forecast`). */
export function idsOf(text) {
  return String(text ?? '')
    .split(',')
    .map((s) => s.trim())
    .filter(Boolean);
}

/** The point of a device that gives a meaning, as the hub picks it: the
 *  readable number with that meaning named after it, else the only one. */
export function pick(points, semantic, key) {
  const withIt = (points ?? []).filter((p) => p.semantic === semantic && p.access?.read !== false && p.kind?.type === 'numeric');
  return withIt.find((p) => p.key === key) ?? (withIt.length === 1 ? withIt[0] : null);
}

/** Where a value of the outside device comes from: `measured` (a station),
 *  `forecast`, or `null` (not known yet). */
export function originOf(source, stations) {
  if (!source) return null;
  return stations.includes(source) ? 'measured' : 'forecast';
}

/** The points of a station worth showing as measures: readable numbers and
 *  states, not its settings (what can be set, or says so) nor its radio. */
export function measures(d) {
  return (d?.points ?? []).filter(
    (p) =>
      p.access?.read !== false &&
      !p.access?.write &&
      (p.kind?.type === 'numeric' || p.kind?.type === 'binary') &&
      !['config', 'signal_strength'].includes(p.semantic),
  );
}

/** A wind direction in degrees → one of 16 compass points (0 = N). */
export function compass(deg) {
  if (deg == null || !Number.isFinite(Number(deg))) return null;
  return Math.round((((Number(deg) % 360) + 360) % 360) / 22.5) % 16;
}

// « Soleil » (#258): the sun as Moli computes it in the house, `dehors:soleil`.

/** The device Moli builds when the house's position is known. */
export const SUN = 'dehors:soleil';

/** The façades of `dehors:soleil`, in their order, from its `facades` point
 *  (one `id=name` per line). */
export function facadesOf(text) {
  return String(text ?? '')
    .split('\n')
    .map((line) => {
      const at = line.indexOf('=');
      if (at <= 0) return null;
      const id = line.slice(0, at).trim();
      const name = line.slice(at + 1).trim();
      return id ? { id, name: name || id } : null;
    })
    .filter(Boolean);
}

/** Where the day stands between sunrise and sunset, 0 to 1; `null` before
 *  sunrise, after sunset, or without both times. */
export function dayFraction(rise, set, now) {
  const r = Date.parse(rise);
  const s = Date.parse(set);
  if (!Number.isFinite(r) || !Number.isFinite(s) || s <= r || now < r || now > s) return null;
  return (now - r) / (s - r);
}

/** A point on the sun's arc: `f` from 0 (sunrise, left) to 1 (sunset,
 *  right), on the arc `M0 h Q w/2 -h w h` the page draws (`width` wide, top
 *  at `y = 0`, feet at `y = height`). */
export function onArc(f, width, height) {
  return { x: f * width, y: height * (1 - 2 * f) ** 2 };
}
