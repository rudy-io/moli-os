import { test } from 'node:test';
import assert from 'node:assert/strict';
import { heatsOnly, valveOpen, heatState, nextStep } from './radiator.js';

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

test('a valve is open by what it says, else by its thermometer by the radiator', () => {
  assert.equal(valveOpen({ running: 'heat', temp: 30, target: 18 }), true);
  assert.equal(valveOpen({ running: 'idle', temp: 10, target: 18 }), false);
  assert.equal(valveOpen({ temp: 18.5, target: 18 }), true);
  assert.equal(valveOpen({ temp: 22.6, target: 18 }), false);
  assert.equal(valveOpen({ temp: null, target: 18 }), null);
});

test('a room heats while under its setpoint; a valve while it is open', () => {
  assert.equal(heatState({ reachable: false, mode: 'auto', temp: 18, target: 20 }), 'offline');
  assert.equal(heatState({ mode: 'off', temp: 18, target: 20, room: true }), 'off');
  assert.equal(heatState({ mode: 'auto', temp: 19.7, target: 20.5, room: true }), 'heating');
  assert.equal(heatState({ mode: 'auto', temp: 20.4, target: 20.5, room: true }), 'reached');
  assert.equal(heatState({ mode: 'auto', temp: null, target: 20.5, room: true }), 'idle');
  assert.equal(heatState({ mode: 'heat', temp: 22.6, target: 18 }), 'reached');
  assert.equal(heatState({ mode: 'heat', temp: 17, target: 18 }), 'heating');
});

test('the next step is the next change of setpoint, today or a later day', () => {
  const spec = { min: 5, max: 25, step: 0.5, stepBase: 5, maxSteps: 6 };
  const week = { monday: '00:00/19.5 07:30/18 18:00/19.5', tuesday: '00:00/19.5 07:30/18 18:00/19.5' };
  // Monday 2026-11-16, 10:00.
  assert.deepEqual(nextStep(spec, week, new Date(2026, 10, 16, 10, 0)), { min: 1080, deg: 19.5, days: 0 });
  // At 20:00, midnight's 19.5 changes nothing: tomorrow 07:30.
  assert.deepEqual(nextStep(spec, week, new Date(2026, 10, 16, 20, 0)), { min: 450, deg: 18, days: 1 });
  // A day not read: nothing said.
  assert.equal(nextStep(spec, { monday: week.monday }, new Date(2026, 10, 16, 20, 0)), null);
  assert.equal(nextStep(null, week, new Date()), null);
});
