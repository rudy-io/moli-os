import { test } from 'node:test';
import assert from 'node:assert/strict';
import { batteryNow } from './battery.js';

const summary = (battery, today = {}) => ({ battery, today });

test('no battery, or nothing to show: nothing', () => {
  assert.equal(batteryNow({}), null);
  assert.equal(batteryNow(summary({ soc_point: 'bms:b/soc' })), null);
});

test('the charge, its gauge and what it holds', () => {
  const b = batteryNow(summary({ soc: 45, capacity_kwh: 10 }));
  assert.equal(b.soc, 45);
  assert.equal(b.tone, 'ok');
  assert.equal(b.stored, 4.5);
  assert.equal(batteryNow(summary({ soc: 30 })).tone, 'mid');
  assert.equal(batteryNow(summary({ soc: 12 })).tone, 'low');
  assert.equal(batteryNow(summary({ soc: 30 })).stored, null);
});

test('the live point wins over the summary, a bad charge is ignored', () => {
  const live = { 'bms:b/soc': 80, 'bms:b/power': 1500 };
  const b = batteryNow(summary({ soc_point: 'bms:b/soc', soc: 40, power_point: 'bms:b/power', power_w: 0 }), (p) => live[p] ?? null);
  assert.equal(b.soc, 80);
  assert.equal(b.power, 1500);
  assert.equal(b.state, 'charge');
  const bad = batteryNow(summary({ soc_point: 'bms:b/soc', power_w: -400 }), () => 140);
  assert.equal(bad.soc, null);
  assert.equal(bad.state, 'decharge');
});

test('charging, discharging, at rest, and a flipped point', () => {
  assert.equal(batteryNow(summary({ power_w: 10 })).state, 'repos');
  assert.equal(batteryNow(summary({ power_w: -800 })).state, 'decharge');
  const flipped = batteryNow(summary({ power_point: 'inv:i/battery', discharge_positive: true }), () => 600);
  assert.equal(flipped.power, -600);
  assert.equal(flipped.state, 'decharge');
});

test('what went in and out today', () => {
  const b = batteryNow(summary({ soc: 50 }, { battery_charge: { kwh: 3.2 }, battery_discharge: { kwh: 1.1 } }));
  assert.equal(b.charged, 3.2);
  assert.equal(b.discharged, 1.1);
});
