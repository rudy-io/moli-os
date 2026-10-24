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
