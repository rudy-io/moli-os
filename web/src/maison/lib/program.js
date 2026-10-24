// A pilot-wire heater's week program (Tuya's `week_program` point): for each
// day, Monday first, its runs from a quarter of an hour on, each an order
// (four or six orders) and a setpoint (switch wiring). Shared by the card's
// strip and the detail window.

/** Quarters of an hour in a day. */
export const SLOTS = 96;

/** The week's days (seven arrays of runs, `null` for one not read), or null. */
export function parseWeek(text) {
  try {
    return JSON.parse(text ?? 'null')?.days ?? null;
  } catch {
    return null;
  }
}

/** « 07:30 » for a quarter of an hour (96 is midnight at the end). */
export const hhmm = (slot) => `${String(Math.floor(slot / 4)).padStart(2, '0')}:${String((slot % 4) * 15).padStart(2, '0')}`;

/** The order a module runs: on four orders it shows comfort −1 and −2 as comfort. */
export const orderOn = (wiring, order) => (wiring === 'four' && order?.startsWith('comfort') ? 'comfort' : order);

/** A day's runs with their end; on switch wiring, a run's setpoint (a part
 *  kept on four or six orders has none: its order then). */
export function spans(runs, wiring) {
  return (runs ?? []).map((run, i) => {
    const order = orderOn(wiring, run.order);
    const celsius = wiring === 'switch' ? (run.celsius ?? null) : null;
    return { at: run.at, end: runs[i + 1]?.at ?? SLOTS, order, celsius };
  });
}

/** Setpoints coloured as the room gauge: blue when cool, red when hot. */
export const warmth = (c) => (c < 12 ? 'frost' : c < 18 ? 'cold' : c < 20 ? 'cool' : c <= 24 ? 'good' : c <= 27 ? 'warm' : 'hot');

/** Today's row (Monday first) and the quarter of an hour now. */
export const nowIn = (date) => ({ day: (date.getDay() + 6) % 7, slot: date.getHours() * 4 + Math.floor(date.getMinutes() / 15) });

/** A day's short or long name in the house's language (2024-01-01 was a Monday). */
export const dayName = (i, locale, weekday = 'short') => new Date(2024, 0, 1 + i).toLocaleDateString(locale, { weekday });

/** A day's 96 quarters of an hour from its runs (its raw orders and
 *  setpoints), or null when it does not cover the day. */
export function slotsOf(runs) {
  const out = [];
  (runs ?? []).forEach((run, i) => {
    const end = runs[i + 1]?.at ?? SLOTS;
    for (let s = run.at; s < end; s++) out.push({ order: run.order ?? null, celsius: run.celsius ?? null });
  });
  return out.length === SLOTS ? out : null;
}

/** A day's runs again from its quarters, a new run wherever the order or
 *  the setpoint changes (as the point gives them). */
export function runsOf(slots) {
  const runs = [];
  slots.forEach((s, at) => {
    const last = runs.at(-1);
    if (!last || last.order !== s.order || last.celsius !== s.celsius) runs.push({ at, order: s.order, celsius: s.celsius });
  });
  return runs;
}

/** The same, in the words a module is sent: only what its wiring sets (an
 *  order on four or six orders, a setpoint on switch), the rest kept. */
export function sentRuns(slots, wiring) {
  const key = wiring === 'switch' ? 'celsius' : 'order';
  const runs = [];
  slots.forEach((s, at) => {
    if (!runs.length || runs.at(-1)[key] !== s[key]) runs.push({ at, [key]: s[key] });
  });
  return runs;
}
