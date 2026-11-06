// A thermostatic valve's weekly schedule, as Zigbee2MQTT states it: one text per
// day (`weekly_schedule_monday`…), steps "HH:mm/<deg>" separated by spaces, the
// first one at 00:00. Pure arithmetic, no Svelte, no DOM: the editor's logic is
// testable on its own. Every function takes the valve's `spec` (see `scheduleSpec`),
// because the limits are the valve's, not the editor's.
import { pointOf, canWrite, numericRange } from './capabilities.js';

/** Z2M's own day names, in the order a week is shown. */
export const DAYS = ['monday', 'tuesday', 'wednesday', 'thursday', 'friday', 'saturday', 'sunday'];

/** The editor places a new or moved step on this grid, in minutes. It is a choice
 *  of the editor: a step the valve already holds keeps its minute. */
export const GRID = 15;

const MINUTES_IN_DAY = 24 * 60;

/** What a valve accepts, from what it exposes; null when no day can be written.
 *  Setpoint bounds and step come from the setpoint's own contract. The number of
 *  steps a day holds is not a point: a SONOFF TRVZB takes 6, a TRV-ZBT (the one
 *  with schedule groups) 12, as Zigbee2MQTT's converters document them. */
export function scheduleSpec(device) {
  const days = DAYS.filter((day) => pointOf(device, `weekly_schedule_${day}`)?.kind?.type === 'text' && canWrite(device, `weekly_schedule_${day}`));
  const range = numericRange(device, 'occupied_heating_setpoint', 0.5);
  if (!days.length || !range) return null;
  return { days, min: range.first, max: range.last, step: range.step, stepBase: range.stepBase, maxSteps: pointOf(device, 'schedule_group_to_edit') ? 12 : 6 };
}

const snapDeg = (spec, deg) => {
  const n = Number(deg);
  if (!Number.isFinite(n)) return spec.min;
  const { min, max, step, stepBase } = spec;
  const index = Math.round((Math.min(max, Math.max(min, n)) - stepBase) / step);
  const snapped = Math.round((stepBase + index * step) * 1e10) / 1e10;
  return Math.min(max, Math.max(min, snapped));
};

const snapMinute = (min) => {
  const n = Number(min);
  if (!Number.isFinite(n)) return 0;
  return Math.min(MINUTES_IN_DAY - GRID, Math.max(0, Math.round(n / GRID) * GRID));
};

/** "HH:mm" of a minute of the day. */
export function hhmm(min) {
  const m = Math.min(MINUTES_IN_DAY - 1, Math.max(0, Math.round(Number(min) || 0)));
  return `${String(Math.floor(m / 60)).padStart(2, '0')}:${String(m % 60).padStart(2, '0')}`;
}

/** The minute of the day of an "HH:mm" (or "H:mm"); null when unreadable. */
export function minutesOf(time) {
  const m = /^(\d{1,2}):(\d{2})$/.exec(String(time ?? '').trim());
  if (!m || Number(m[1]) > 23 || Number(m[2]) > 59) return null;
  return Number(m[1]) * 60 + Number(m[2]);
}

/** Sorted by time, one step per minute (the first stands), at most `maxSteps`,
 *  the setpoints inside the valve's bounds, the first step at 00:00. Never empty. */
export function normalize(spec, steps) {
  const seen = new Set();
  const sorted = [...(steps ?? [])]
    .filter((s) => Number.isFinite(Number(s?.min)))
    .map((s) => ({ min: Math.min(MINUTES_IN_DAY - 1, Math.max(0, Math.round(Number(s.min)))), deg: snapDeg(spec, s.deg) }))
    .sort((a, b) => a.min - b.min)
    .filter((s) => !seen.has(s.min) && seen.add(s.min))
    .slice(0, spec.maxSteps);
  if (!sorted.length) return [{ min: 0, deg: spec.min }];
  if (sorted[0].min !== 0) sorted.unshift({ min: 0, deg: sorted[0].deg });
  return sorted.slice(0, spec.maxSteps);
}

/** Reads a day's text. Whatever it cannot read is left out, never fatal. */
export function parseDay(spec, text) {
  const steps = [];
  for (const part of String(text ?? '').split(/\s+/)) {
    const m = /^(\d{1,2}:\d{2})\/(\d+(?:\.\d+)?)$/.exec(part);
    const min = m ? minutesOf(m[1]) : null;
    if (min != null) steps.push({ min, deg: Number(m[2]) });
  }
  return normalize(spec, steps);
}

/** A day as the valve is given it. */
export function formatDay(spec, steps) {
  return normalize(spec, steps)
    .map((s) => `${hhmm(s.min)}/${s.deg}`)
    .join(' ');
}

/** The step in force at `minutes`. */
export function stepAt(spec, steps, minutes) {
  let found = normalize(spec, steps)[0];
  for (const s of normalize(spec, steps)) {
    if (s.min <= minutes) found = s;
  }
  return found;
}

/** A new step at `min` (on the grid), taking the setpoint of the step it lands in.
 *  Unchanged when 00:00, an occupied minute or a day already full. */
export function addStep(spec, steps, min) {
  const day = normalize(spec, steps);
  const at = snapMinute(min);
  if (at === 0 || day.some((s) => s.min === at) || day.length >= spec.maxSteps) return day;
  return normalize(spec, [...day, { min: at, deg: stepAt(spec, day, at).deg }]);
}

/** Where a new step is least in the way: the middle of the day's longest stretch,
 *  on the grid. Null when the day is full or no free grid minute is left. */
export function suggestMinute(spec, steps) {
  const day = normalize(spec, steps);
  if (day.length >= spec.maxSteps) return null;
  let best = null;
  day.forEach((s, i) => {
    const end = day[i + 1]?.min ?? MINUTES_IN_DAY;
    const mid = snapMinute(s.min + (end - s.min) / 2);
    if (mid > s.min && mid < end && (!best || end - s.min > best.len)) best = { len: end - s.min, mid };
  });
  return best?.mid ?? null;
}

/** The step at `index` moved to `min` (on the grid). The 00:00 one never moves,
 *  nothing moves onto 00:00 or onto another step. */
export function moveStep(spec, steps, index, min) {
  const day = normalize(spec, steps);
  const at = snapMinute(min);
  if (index <= 0 || index >= day.length || at === 0 || day.some((s, i) => i !== index && s.min === at)) return day;
  return normalize(spec, day.map((s, i) => (i === index ? { ...s, min: at } : s)));
}

/** The setpoint of the step at `index`. */
export function setStepDeg(spec, steps, index, deg) {
  const day = normalize(spec, steps);
  // An emptied field is no setpoint: it never falls back to the valve's minimum.
  if (deg == null || deg === '' || !Number.isFinite(Number(deg))) return day;
  return index < 0 || index >= day.length ? day : day.map((s, i) => (i === index ? { ...s, deg: snapDeg(spec, deg) } : s));
}

/** Writes the days one after the other and stops at the first one that is not done:
 *  the guard keeps only the last held order, so going on would leave the earlier
 *  days silently unwritten. `write(day)` resolves `'ok'` when the order went through.
 *  Returns the days written and the outcome that stopped it (null when none did). */
export async function writeDays(days, write) {
  const written = [];
  for (const day of days) {
    const outcome = await write(day);
    if (outcome !== 'ok') return { written, stopped: outcome ?? 'error' };
    written.push(day);
  }
  return { written, stopped: null };
}

/** The step at `index` removed. The 00:00 one stays. */
export function removeStep(spec, steps, index) {
  const day = normalize(spec, steps);
  return index <= 0 || index >= day.length ? day : day.filter((_, i) => i !== index);
}

/** True when two days make the same text. */
export const sameDay = (spec, a, b) => formatDay(spec, a) === formatDay(spec, b);
