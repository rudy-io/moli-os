// A radiator behind a thermostat: what it is doing, read from the temperature,
// the setpoint and the mode. Pure arithmetic, no Svelte, no DOM, no hub:
// testable alone.
//
// Three kinds share it: a room's thermostat kept by Moli (it reads the room's
// sensor and drives the room's valves), a Zigbee valve (ZCL points), and a
// valve that only heats on another protocol (Matter: `target_temperature` and
// a mode that is `off` or `heat`). A valve on its own reads its thermometer by
// its radiator: it tells whether it is open, not whether the room is warm.
import { pointOf, isValve } from './capabilities.js';
import { DAYS, parseDay } from './schedule.js';

/** Under the setpoint by more than this (°C), the radiator heats: a sensor's
 *  noise and half a step of the setpoint move nothing (as Moli's own regulation). */
export const DEADBAND = 0.2;
/** Over the setpoint by more than this (°C), the valve has shut (the hub's
 *  `SHUT_MARGIN`, `moli-heating`). */
export const OVER = 0.5;

const finite = (v) => typeof v === 'number' && Number.isFinite(v);

/** A thermostat that only heats, on any protocol: a setpoint, a mode that is
 *  only `off` and `heat`, nothing of an air conditioner's (no power switch, no
 *  fan, no cooling setpoint). */
export function heatsOnly(d) {
  if (isValve(d)) return true;
  const modes = pointOf(d, 'mode')?.kind?.values;
  return pointOf(d, 'target_temperature')?.kind?.type === 'numeric'
    && Array.isArray(modes) && modes.includes('heat') && modes.every((m) => m === 'off' || m === 'heat')
    && !['on', 'fan_speed', 'cool_setpoint', 'heat_setpoint'].some((k) => pointOf(d, k));
}

/** Whether a valve lets the hot water in: what it says (`running_state`), else
 *  its thermometer against its setpoint; null when nothing tells. */
export function valveOpen({ running = null, temperature, target }) {
  if (running === 'heat' || running === 'heating') return true;
  if (running === 'idle') return false;
  if (!finite(temperature) || !finite(target)) return null;
  return temperature <= target + OVER;
}

/** What the radiator is doing: `{phase, hold?}`.
 *  `phase`: `offline`, `off`, `airing` (a valve turned down, window open),
 *  `heating`, `holding` (at its setpoint), `idle` (warmer than asked),
 *  `closed` (a valve that has shut), `unknown` (no temperature to tell).
 *  `hold`: `manual` when a valve was turned by hand. `running`: what the
 *  valve says itself (`heat`, `idle`), when it says it. `valve`: a valve read
 *  on its own, open or closed, rather than a room's thermostat. */
export function radiatorState({ reachable = true, mode = null, temperature = null, target = null, override = null, running = null, valve = false }) {
  if (!reachable) return { phase: 'offline' };
  const hold = override === 'manual' ? 'manual' : null;
  if (override === 'airing') return { phase: 'airing' };
  if (mode === 'off') return { phase: 'off', hold };
  if (valve) {
    const open = valveOpen({ running, temperature, target });
    return { phase: open == null ? 'unknown' : open ? 'heating' : 'closed', hold };
  }
  if (running === 'heat') return { phase: 'heating', hold };
  if (!finite(temperature) || !finite(target)) return { phase: running === 'idle' ? 'idle' : 'unknown', hold };
  const gap = target - temperature;
  if (gap > DEADBAND) return { phase: running === 'idle' ? 'holding' : 'heating', hold };
  if (gap < -OVER) return { phase: 'idle', hold };
  return { phase: 'holding', hold };
}

/** Whether the drawing shows heat rising. */
export const warm = (phase) => phase === 'heating';

/** The schedule's next change of setpoint after `date`, within a week:
 *  `{deg, min, days}` (`days`: 0 today, 1 tomorrow…), or null. `texts`: the
 *  day texts (`weekly_schedule_<day>`), `spec`: what the valve accepts. */
export function nextChange(spec, texts, date = new Date()) {
  if (!spec) return null;
  const today = (date.getDay() + 6) % 7;
  const minute = date.getHours() * 60 + date.getMinutes();
  // A day the valve does not state says nothing (not a day at its lowest).
  const day = (offset) => {
    const text = texts?.[DAYS[(today + offset) % 7]];
    return typeof text === 'string' && text.trim() ? parseDay(spec, text) : [];
  };
  const steps = day(0);
  if (!steps.length) return null;
  let current = steps[0].deg;
  for (const s of steps) if (s.min <= minute) current = s.deg;
  for (let offset = 0; offset < 8; offset++) {
    for (const s of day(offset)) {
      if (offset === 0 && s.min <= minute) continue;
      if (s.deg !== current) return { deg: s.deg, min: s.min, days: offset };
    }
  }
  return null;
}
