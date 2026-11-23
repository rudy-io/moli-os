import { test } from 'node:test';
import assert from 'node:assert/strict';
import { noLiveMeasure, lastDay, vsUsual, usualFrac, gaps } from './energy-late.js';

const DAY = 86_400_000;

test('a house without a live measure is told apart', () => {
  assert.equal(noLiveMeasure(null), false);
  assert.equal(noLiveMeasure({ meters: [{ role: 'grid', history: true }] }), true);
  assert.equal(noLiveMeasure({ live: { point: 'a/b' }, meters: [] }), false);
  assert.equal(noLiveMeasure({ meters: [{ role: 'total', power: 'z2m:x/power' }] }), false);
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
