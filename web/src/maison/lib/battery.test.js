import { test } from 'node:test';
import assert from 'node:assert/strict';
import { batteryNow, splitFlows } from './battery.js';

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

test('the battery charges from the sun first, then the grid', () => {
  // 202 W of sun, 42 W into the battery, 1.5 kW bought.
  let f = splitFlows({ bought: 1500, made: 202, sent: 0, batteryW: 42 });
  assert.deepEqual(f, { solarToBattery: 42, gridToBattery: 0, solarToGrid: 0, solarToHouse: 160, gridToHouse: 1500, batteryToGrid: 0, batteryToHouse: 0 });
  // Little sun, the battery takes 1 kW: the grid gives the rest.
  f = splitFlows({ bought: 1500, made: 300, sent: 0, batteryW: 1000 });
  assert.equal(f.solarToBattery, 300);
  assert.equal(f.gridToBattery, 700);
  assert.equal(f.gridToHouse, 800);
  // A sunny noon: 3 kW made, 1 kW stored, 500 W sent out.
  f = splitFlows({ bought: 0, made: 3000, sent: 500, batteryW: 1000 });
  assert.equal(f.solarToBattery, 1000);
  assert.equal(f.solarToGrid, 500);
  assert.equal(f.solarToHouse, 1500);
});

test('a discharging battery feeds the house, the grid only beyond the sun', () => {
  let f = splitFlows({ bought: 200, made: 0, sent: 0, batteryW: -900 });
  assert.equal(f.batteryToHouse, 900);
  assert.equal(f.gridToHouse, 200);
  f = splitFlows({ bought: 0, made: 100, sent: 300, batteryW: -500 });
  assert.equal(f.solarToGrid, 100);
  assert.equal(f.batteryToGrid, 200);
  assert.equal(f.batteryToHouse, 300);
  // Missing readings count as nothing.
  assert.equal(splitFlows({ bought: null, made: undefined, batteryW: null }).gridToHouse, 0);
});

test('no battery and the production unknown: what goes out came from the sun, as before', () => {
  // A solar meter without a power point (or a silent inverter), the clamp at −400 W.
  const f = splitFlows({ bought: 0, made: null, sent: 400, batteryW: null });
  assert.equal(f.solarToGrid, 400);
  assert.equal(f.solarToHouse, 0);
  assert.equal(f.gridToHouse, 0);
  // Known production keeps its own split.
  const g = splitFlows({ bought: 0, made: 1000, sent: 400 });
  assert.equal(g.solarToGrid, 400);
  assert.equal(g.solarToHouse, 600);
});
