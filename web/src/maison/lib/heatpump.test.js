import { test } from 'node:test';
import assert from 'node:assert/strict';
import { phaseOf, cop, dayCop, deltaT, stepped, followsRoom } from './heatpump.js';

test('what the heat pump does', () => {
  assert.equal(phaseOf({ power_state: false, frequency: 40 }), 'off');
  assert.equal(phaseOf({ power_state: true, defrost: 'Defrosting', frequency: 40 }), 'defrost');
  assert.equal(phaseOf({ power_state: true, defrost: 'Normal', frequency: 0, operating_mode: 'Heating' }), 'idle');
  assert.equal(phaseOf({ power_state: true, frequency: 52, operating_mode: 'Heating' }), 'heating');
  assert.equal(phaseOf({ power_state: true, frequency: 52, operating_mode: 'Hot Water' }), 'hot_water');
  assert.equal(phaseOf({ power_state: true, frequency: 52, operating_mode: 'Cooling' }), 'cooling');
  assert.equal(phaseOf({}), null);
});

test('the COP, only while running and measured', () => {
  assert.equal(cop(5000, 1600), 3.1);
  assert.equal(cop(5000, 1600, false), null);
  assert.equal(cop(0, 65), null);
  assert.equal(cop(5000, 30), null, 'a standby draw is no COP');
  assert.equal(dayCop(38.02, 12.46), 3.1);
  assert.equal(dayCop(10, 0), null);
});

test('water drop, set point steps, mode', () => {
  assert.equal(deltaT(34.5, 31.5), 3);
  assert.equal(deltaT(null, 31.5), null);
  const kind = { min: 10, max: 30, step: 0.5 };
  assert.equal(stepped(20, kind, 1), 20.5);
  assert.equal(stepped(10, kind, -1), 10);
  assert.equal(stepped(29.8, kind, 1), 30);
  assert.equal(followsRoom('Heating Room Temp'), true);
  assert.equal(followsRoom('Heating Flow Temp'), false);
});
