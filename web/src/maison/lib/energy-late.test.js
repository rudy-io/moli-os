import { test } from 'node:test';
import assert from 'node:assert/strict';
import { noLiveMeasure, lateHouse, lastDay, vsUsual, usualFrac, gaps } from './energy-late.js';

const DAY = 86_400_000;

test('a house without a live measure is told apart', () => {
  assert.equal(noLiveMeasure(null), false);
  assert.equal(noLiveMeasure({ meters: [{ role: 'grid', history: true }] }), true);
  assert.equal(noLiveMeasure({ live: { point: 'a/b' }, meters: [] }), false);
  assert.equal(noLiveMeasure({ meters: [{ role: 'total', power: 'z2m:x/power' }] }), false);
});

test('a house is shown by its last day only when the server says its electricity is late', () => {
  const elec = { energy: 'electricity', day: 1, amount: { kwh: 1 }, usual_days: 0 };
  const gas = { energy: 'gas', day: 1, amount: { kwh: 1 }, usual_days: 0 };
  assert.equal(lateHouse({ meters: [], last_days: [elec] }), true);
  // New install, or a cumulative meter without a power point: no last day yet.
  assert.equal(lateHouse({ meters: [{ role: 'total' }] }), false);
  // Only the gas is late: the electricity keeps « now » and « today ».
  assert.equal(lateHouse({ meters: [], last_days: [gas] }), false);
  assert.equal(lateHouse({ live: { point: 'a/b' }, meters: [], last_days: [elec] }), false);
});

test('the last day known, against its usual day', () => {
  const s = {
    last_days: [
      { energy: 'electricity', day: 1, amount: { kwh: 12 }, usual: { kwh: 10 }, usual_days: 7 },
      { energy: 'gas', day: 0, amount: { kwh: 50 }, usual_days: 0 },
    ],
  };
  const elec = lastDay(s, 'electricity');
  assert.equal(vsUsual(elec), 20);
  assert.equal(vsUsual(lastDay(s, 'gas')), null);
  assert.equal(lastDay({}, 'gas'), null);
  assert.ok(Math.abs(usualFrac(elec) - 0.8) < 1e-9);
  assert.equal(usualFrac({ amount: { kwh: 40 }, usual: { kwh: 10 } }), 1);
  assert.equal(usualFrac(lastDay(s, 'gas')), 2 / 3);
  assert.equal(usualFrac(null), 0);
});

test('the days a late meter never told, not the days not yet published', () => {
  const day = (i, ids) => ({ start: i * DAY, meters: Object.fromEntries(ids.map((id) => [id, { kwh: 1 }])) });
  const report = {
    step: 'day',
    meters: [
      { id: 'linky', name: 'Linky', role: 'grid', history: true },
      { id: 'gaz', name: 'Gaz', role: 'gas', history: true },
      { id: 'pince', name: 'Pince', role: 'circuit' },
    ],
    buckets: [
      day(0, ['linky', 'gaz']),
      day(1, ['gaz']),
      day(2, ['gaz']),
      day(3, ['linky', 'gaz']),
      day(4, ['gaz']),
      day(5, ['linky']),
      day(6, []),
      day(7, []),
    ],
  };
  const g = gaps(report);
  assert.deepEqual(g.meters, [
    {
      id: 'linky',
      name: 'Linky',
      runs: [
        { from: DAY, to: 2 * DAY, days: 2 },
        { from: 4 * DAY, to: 4 * DAY, days: 1 },
      ],
    },
  ]);
  assert.deepEqual([...g.missing].sort(), [1, 2, 4]);
  assert.equal(gaps({ ...report, step: 'hour' }).meters.length, 0);
});

test('the gas holes only when the chart draws the gas', () => {
  const day = (i, ids) => ({ start: i * DAY, meters: Object.fromEntries(ids.map((id) => [id, { kwh: 1 }])) });
  const report = {
    step: 'day',
    meters: [
      { id: 'linky', name: 'Linky', role: 'grid', history: true },
      { id: 'gaz', name: 'Gaz', role: 'gas', history: true },
    ],
    buckets: [day(0, ['linky', 'gaz']), day(1, ['linky']), day(2, ['linky', 'gaz'])],
  };
  assert.deepEqual(gaps(report).meters.map((m) => m.id), ['gaz']);
  assert.deepEqual([...gaps(report).missing], [1]);
  const drawn = gaps(report, { gas: false });
  assert.deepEqual(drawn.meters, []);
  assert.equal(drawn.missing.size, 0);
});
