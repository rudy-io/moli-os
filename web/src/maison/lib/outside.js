// « Dehors » (#210): the weather outside as one device, `dehors:meteo`, fed
// meaning by meaning by the stations (measures), else the forecast. These
// helpers are pure: the page gives them the devices.

/** The device Moli builds when `[outdoor]` is declared. */
export const OUTSIDE = 'dehors:meteo';

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
