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

/** The alarm's past events from the history of `event`, `cause` and
 *  `silent`: newest first, each with the sensors recorded with it (those
 *  in cause, or those that stopped answering). */
export function journal(events, causes, silents) {
  const sorted = (h) => [...(h ?? [])].sort((a, b) => a[0] - b[0]);
  const c = sorted(causes);
  const quiet = sorted(silents);
  // Written just before it (same instant, give or take).
  const near = (h, ts) => h.filter(([t]) => t <= ts + 2000 && t >= ts - 5000).at(-1)?.[1] ?? '';
  return [...(events ?? [])]
    .filter(([, v]) => typeof v === 'string' && v)
    .map(([ts, what]) => {
      const cause = ['arm_failure', 'pending', 'triggered'].includes(what) ? near(c, ts) : what === 'silent' ? near(quiet, ts) : '';
      return { ts, what, cause: typeof cause === 'string' ? cause : '' };
    })
    .sort((a, b) => b.ts - a.ts);
}

// ---- settings (point `setup`, written back disarmed) -------------------

/** Points a sensor can be: an opening, a presence, a count in a camera zone. */
const SENSOR_KEYS = ['contact', 'occupancy', 'motion', 'presence', 'person', 'car', 'vibration', 'tamper', 'water_leak', 'smoke'];

/** Every point of the house that can watch for the alarm: [{ point, name }],
 *  by name. `nameOf(device)` gives the device's name in Moli. */
export function candidates(devices, nameOf) {
  const out = [];
  for (const d of devices) {
    if (isAlarm(d)) continue;
    for (const p of d.points ?? []) {
      if (!SENSOR_KEYS.includes(p.key) && !['contact', 'occupancy'].includes(p.semantic)) continue;
      const zone = d.id.includes(':zone-');
      out.push({ point: `${d.id}/${p.key}`, name: zone || p.key !== 'contact' ? `${nameOf(d)} (${p.label ?? p.key})` : nameOf(d) });
    }
  }
  return out.sort((a, b) => a.name.localeCompare(b.name));
}

/** A draft from what the alarm shows: a copy the screen may change. */
export function draftOf(setup) {
  return { modes: structuredClone(setup.modes ?? {}), sensors: structuredClone(setup.sensors ?? []) };
}

/** Turning a mode off takes it from every sensor. */
export function withoutMode(draft, mode) {
  const modes = { ...draft.modes };
  delete modes[mode];
  return { modes, sensors: draft.sensors.map((s) => ({ ...s, modes: s.modes.filter((m) => m !== mode) })) };
}

/** What stops the draft from being kept, as keys of the screen's words. */
export function problems(draft) {
  const out = [];
  if (!Object.keys(draft.modes).length) out.push('alarme.reglages.aucun_mode');
  for (const d of Object.values(draft.modes)) {
    if ([d.exit, d.entry, d.trigger].some((v) => !Number.isInteger(v) || v < 0 || v > 3600)) {
      out.push('alarme.reglages.delai_invalide');
      break;
    }
  }
  if (draft.sensors.some((s) => !s.modes.length)) out.push('alarme.reglages.capteur_sans_mode');
  return out;
}

/** The settings as the alarm writes them back: `immediate` only when true,
 *  so a draft and what the alarm shows compare by meaning. */
function normal(settings) {
  return JSON.stringify({
    modes: Object.fromEntries(Object.entries(settings.modes ?? {}).sort(([a], [b]) => a.localeCompare(b))),
    sensors: (settings.sensors ?? []).map(({ immediate, ...s }) => (immediate ? { ...s, immediate: true } : s)),
  });
}

/** Whether the draft changes nothing: keeping it would only ask the code. */
export function unchanged(draft, setup) {
  return normal(draft) === normal(setup);
}
