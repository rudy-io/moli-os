// A radiator's thermostat, drawn: whether it heats, from what the thermostat
// states. Pure arithmetic, no Svelte, no DOM, no hub: testable alone.
//
// Three kinds share it: a room's thermostat kept by Moli (it drives the room's
// valves), a Zigbee valve (ZCL points), and a valve that only heats on another
// protocol (Matter: `target_temperature` and a mode that is `off` or `heat`).
import { pointOf, isValve } from './capabilities.js';
import { DAYS, parseDay } from './schedule.js';

/** The room is warm enough within this much under its setpoint. */
export const REACHED_MARGIN = 0.2;
/** A valve's thermometer, by its radiator, over its setpoint by more than
 *  this: the valve has shut (the hub's `SHUT_MARGIN`, `moli-heating`). */
export const SHUT_MARGIN = 0.5;

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
export function valveOpen({ running, temp, target }) {
  if (running === 'heat' || running === 'heating') return true;
  if (running === 'idle') return false;
  if (!finite(temp) || !finite(target)) return null;
  return temp <= target + SHUT_MARGIN;
}

/** Where a thermostat is: `offline`, `off`, `heating`, `reached`, `idle` (no
 *  reading to tell). A room (one that drives valves) is read by its own
 *  thermometer, away from the radiator; a valve by whether it is open. */
export function heatState({ reachable = true, mode, temp, target, running = null, room = false }) {
  if (!reachable) return 'offline';
  if (mode === 'off') return 'off';
  if (room) {
    if (!finite(temp) || !finite(target)) return 'idle';
    return temp < target - REACHED_MARGIN ? 'heating' : 'reached';
  }
  const open = valveOpen({ running, temp, target });
  return open == null ? 'idle' : open ? 'heating' : 'reached';
}

/** The next change of setpoint the schedule makes after `now` (a Date): `{min,
 *  deg, days}` (`days` 0 today, 1 tomorrow), within the next week; null when the
 *  days are not read or nothing changes. `texts` maps a Z2M day name to its text. */
export function nextStep(spec, texts, now) {
  if (!spec) return null;
  const today = (now.getDay() + 6) % 7;
  const minute = now.getHours() * 60 + now.getMinutes();
  const dayOf = (n) => {
    const text = texts[DAYS[(today + n) % 7]];
    return typeof text === 'string' && text.trim() ? parseDay(spec, text) : null;
  };
  const first = dayOf(0);
  if (!first) return null;
  let held = first.filter((s) => s.min <= minute).at(-1)?.deg ?? first[0].deg;
  for (let n = 0; n < 7; n++) {
    const steps = dayOf(n);
    if (!steps) return null;
    for (const s of steps) {
      if (n === 0 && s.min <= minute) continue;
      if (s.deg !== held) return { min: s.min, deg: s.deg, days: n };
      held = s.deg;
    }
  }
  return null;
}
