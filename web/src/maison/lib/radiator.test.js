import { test } from 'node:test';
import assert from 'node:assert/strict';
import { radiatorState, nextChange, warm, heatsOnly, valveOpen } from './radiator.js';

test('a radiator heats under its setpoint, holds at it, rests over it', () => {
  const at = (temperature, target, extra = {}) => radiatorState({ mode: 'auto', temperature, target, ...extra }).phase;
  assert.equal(at(18.2, 19), 'heating');
  assert.equal(at(18.85, 19), 'holding', 'within the deadband');
  assert.equal(at(19.4, 19), 'holding');
  assert.equal(at(19.6, 19), 'idle');
  assert.equal(at(null, 19), 'unknown');
  assert.equal(warm('heating'), true);
  assert.equal(warm('holding'), false);
});

test('what the valve says of itself wins over the arithmetic', () => {
  assert.equal(radiatorState({ mode: 'heat', temperature: 21, target: 19, running: 'heat' }).phase, 'heating');
  assert.equal(radiatorState({ mode: 'heat', temperature: 18, target: 19, running: 'idle' }).phase, 'holding');
  assert.equal(radiatorState({ mode: 'heat', running: 'idle' }).phase, 'idle');
});

test('off, unreachable, airing and a valve turned by hand', () => {
  assert.equal(radiatorState({ reachable: false, mode: 'auto', temperature: 18, target: 19 }).phase, 'offline');
  assert.equal(radiatorState({ mode: 'off', temperature: 18, target: 19 }).phase, 'off');
  assert.deepEqual(radiatorState({ mode: 'auto', temperature: 18, target: 19, override: 'airing' }), { phase: 'airing' });
  assert.deepEqual(radiatorState({ mode: 'auto', temperature: 18, target: 21, override: 'manual' }), { phase: 'heating', hold: 'manual' });
  assert.equal(radiatorState({ mode: 'auto', temperature: 18, target: 19, override: 'none' }).hold, null);
});

test('the next change of the schedule: later today, tomorrow, or none', () => {
  const spec = { days: [], min: 5, max: 25, step: 0.5, stepBase: 5, maxSteps: 6 };
  const week = { monday: '00:00/17 06:30/19 22:00/17', tuesday: '00:00/17 07:00/19 22:00/17' };
  // A Monday (2026-11-23), 10:00.
  const monday = (h, m = 0) => new Date(2026, 10, 23, h, m);
  assert.deepEqual(nextChange(spec, week, monday(10)), { deg: 17, min: 22 * 60, days: 0 });
  assert.deepEqual(nextChange(spec, week, monday(23)), { deg: 19, min: 7 * 60, days: 1 }, 'midnight keeps 17: not a change');
  assert.deepEqual(nextChange(spec, week, monday(6, 30)), { deg: 17, min: 22 * 60, days: 0 });
  assert.equal(nextChange(spec, { monday: '00:00/19' }, monday(10)), null, 'one setpoint all week');
  assert.equal(nextChange(null, week, monday(10)), null);
});

const point = (key, kind) => ({ key, kind, access: { read: true, write: true } });
const enumeration = (...values) => ({ type: 'enum', values });
const numeric = { type: 'numeric', min: 5, max: 30 };

test('a thermostat that only heats is told from an air conditioner by what it exposes', () => {
  const valve = { points: [point('target_temperature', numeric), point('temperature', numeric), point('mode', enumeration('off', 'heat'))] };
  assert.equal(heatsOnly(valve), true);
  const zigbee = { points: [point('occupied_heating_setpoint', numeric), point('system_mode', enumeration('off', 'auto', 'heat'))] };
  assert.equal(heatsOnly(zigbee), true);
  assert.equal(heatsOnly({ points: [...valve.points.slice(0, 2), point('mode', enumeration('off', 'heat', 'cool'))] }), false);
  assert.equal(heatsOnly({ points: [...valve.points, point('on', { type: 'binary' })] }), false);
  assert.equal(heatsOnly({ points: [...valve.points, point('fan_speed', enumeration('1', '2'))] }), false);
  assert.equal(heatsOnly(null), false);
});

test('a valve on its own is open or closed: by what it says, else by its thermometer by the radiator', () => {
  assert.equal(valveOpen({ running: 'heat', temperature: 30, target: 18 }), true);
  assert.equal(valveOpen({ running: 'idle', temperature: 10, target: 18 }), false);
  assert.equal(valveOpen({ temperature: 18.5, target: 18 }), true);
  assert.equal(valveOpen({ temperature: 22.6, target: 18 }), false);
  assert.equal(valveOpen({ temperature: null, target: 18 }), null);
  const at = (temperature, target, extra = {}) => radiatorState({ mode: 'heat', temperature, target, valve: true, ...extra }).phase;
  assert.equal(at(17, 18), 'heating');
  assert.equal(at(22.6, 18), 'closed');
  assert.equal(at(null, 18), 'unknown');
  assert.equal(at(17, 18, { mode: 'off' }), 'off');
});
