// The house's alarm (driver `alarm`): its device, its modes and sensors
// (point `setup`), and how its state reads on screen.

/** The alarm's device: Moli's own, with `mode`, `state` and `setup`. */
export function isAlarm(d) {
  const keys = new Set((d?.points ?? []).map((p) => p.key));
  return d?.manufacturer === 'Moli' && keys.has('mode') && keys.has('state') && keys.has('setup');
}

/** Its modes and sensors: `{ modes: { night: {exit, entry, trigger} }, sensors: [...] }`. */
export function setupOf(d) {
  try {
    const s = JSON.parse(d?.state?.setup?.value ?? '');
    return { modes: s.modes ?? {}, sensors: s.sensors ?? [] };
  } catch {
    return { modes: {}, sensors: [] };
  }
}

/** The order the modes are offered in, lightest watch first. */
export const MODES = ['home', 'night', 'away'];

/** Whether a sensor's value says it sees something (as the alarm reads
 *  it): a `contact` is true when closed, so open is what counts. */
export function sensorActive(point, value) {
  if (point.endsWith('/contact') && typeof value === 'boolean') return !value;
  if (typeof value === 'boolean') return value;
  if (typeof value === 'number') return value > 0;
  if (typeof value === 'string') return ['on', 'open', 'true', 'detected', 'motion', 'occupied'].includes(value.toLowerCase());
  return false;
}

/** The sensors of `mode` that see something now: arming would be refused. */
export function blocking(setup, mode, valueOf) {
  return setup.sensors.filter((s) => s.modes.includes(mode) && sensorActive(s.point, valueOf(s.point))).map((s) => s.name);
}

/** The state as a tone for the screen. */
export function toneOf(state) {
  if (state === 'triggered') return 'alert';
  if (state === 'pending') return 'warn';
  if (state === 'arming') return 'arming';
  if (state?.startsWith('armed_')) return 'armed';
  return 'off';
}

/** Seconds left in the current delay (`until` in Unix seconds, 0 = none). */
export function secondsLeft(until, nowMs) {
  if (!until) return null;
  return Math.max(0, Math.ceil(until - nowMs / 1000));
}

/** A delay in words: 0 → null, 45 → « 45 s », 480 → « 8 min ». */
export function delay(s) {
  if (!s) return null;
  if (s < 60) return { n: s, unit: 's' };
  return { n: Math.round(s / 60), unit: 'min' };
}

/** The alarm's past events from the history of `event` and `cause`:
 *  newest first, each with the cause recorded with it. */
export function journal(events, causes) {
  const c = [...(causes ?? [])].sort((a, b) => a[0] - b[0]);
  return [...(events ?? [])]
    .filter(([, v]) => typeof v === 'string' && v)
    .map(([ts, what]) => {
      // Its cause is written just before it (same instant, give or take).
      const near = c.filter(([t]) => t <= ts + 2000 && t >= ts - 5000).at(-1);
      const cause = ['arm_failure', 'pending', 'triggered'].includes(what) ? (near?.[1] ?? '') : '';
      return { ts, what, cause };
    })
    .sort((a, b) => b.ts - a.ts);
}
