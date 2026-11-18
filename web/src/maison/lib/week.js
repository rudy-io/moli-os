// A thermostat's week at a glance: seven days of bands across 24 h, one colour per
// setpoint. Pure arithmetic over `schedule.js`, no Svelte, no DOM.
import { DAYS, parseDay, formatDay, moveStep, GRID } from './schedule.js';

export const MINUTES_IN_DAY = 24 * 60;

/** The colour scale is the same for every room: 15 °C is the coolest, 23 °C the warmest. */
export const COLD = 15;
export const WARM = 23;

/** How warm a setpoint looks, from 0 (cool) to 100 (warm), in whole percents. */
export function warmth(deg) {
  const n = Number(deg);
  if (!Number.isFinite(n)) return 0;
  return Math.round((Math.min(WARM, Math.max(COLD, n)) - COLD) / (WARM - COLD) * 100);
}

/** A day's bands: `{ start, end, deg }`, minutes from midnight, covering 0 → 1440. */
export function bands(spec, text) {
  const steps = parseDay(spec, text);
  return steps.map((s, i) => ({ start: s.min, end: steps[i + 1]?.min ?? MINUTES_IN_DAY, deg: s.deg, index: i }));
}

/** The minute of the day under a pointer at `x` in a row `width` wide, on the grid. */
export function minuteAt(x, width) {
  if (!(width > 0)) return 0;
  const raw = (Math.min(width, Math.max(0, x)) / width) * MINUTES_IN_DAY;
  return Math.min(MINUTES_IN_DAY - GRID, Math.max(GRID, Math.round(raw / GRID) * GRID));
}

/** The day with the boundary of step `index` dragged to `minute`; unchanged when it
 *  cannot go there (on another step, on 00:00, past its neighbours). */
export function dragBoundary(spec, steps, index, minute) {
  const prev = steps[index - 1]?.min ?? 0;
  const next = steps[index + 1]?.min ?? MINUTES_IN_DAY;
  if (index <= 0 || minute <= prev || minute >= next) return steps;
  return moveStep(spec, steps, index, minute);
}

/** Today's day name and minute, in the browser's clock (the house's, at home). */
export function now(date = new Date()) {
  return { day: DAYS[(date.getDay() + 6) % 7], minute: date.getHours() * 60 + date.getMinutes() };
}

/** A week copied to another thermostat: each day as `target` accepts it (its bounds
 *  and steps). `texts` maps a day to its text; days `target` does not take are left out. */
export function copyWeek(source, target, texts) {
  const out = {};
  for (const day of DAYS) {
    if (!source.days.includes(day) || !target.days.includes(day)) continue;
    out[day] = formatDay(target, parseDay(source, texts[day]));
  }
  return out;
}
