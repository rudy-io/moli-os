import { test } from 'node:test';
import assert from 'node:assert/strict';
import { tariffSplit } from './tariff-split.js';

const part = (id) => ({ id, name: id, kwh: (b) => b.meters[id]?.kwh ?? 0, cost: (b) => b.meters[id]?.cost ?? null });

test('each part is split as the contract split the hour, at its prices', () => {
  // 1 h 00 – 2 h 00 with off-peak from 1 h 30: half and half, 0.10 € and 0.20 €.
  const buckets = [
    { start: 0, meters: { a: { kwh: 1 }, b: { kwh: 3 } }, by_period: { HC: { kwh: 2, cost: 0.2 }, HP: { kwh: 2, cost: 0.4 } } },
    { start: 1, meters: { a: { kwh: 2, cost: 0.4 } }, by_period: { HP: { kwh: 2, cost: 0.4 } } },
  ];
  const rows = tariffSplit([part('a'), part('b')], buckets, () => 0);
  const [a, b] = ['a', 'b'].map((id) => rows.find((r) => r.id === id));
  assert.deepEqual([b.hc, b.hp], [1.5, 1.5]);
  assert.ok(Math.abs(b.hcCost - 0.15) < 1e-9 && Math.abs(b.hpCost - 0.3) < 1e-9);
  assert.deepEqual([a.hc, a.hp], [0.5, 2.5]);
  // The parts add up to the house.
  assert.equal(a.hc + b.hc, 2);
  assert.ok(Math.abs(a.hcCost + b.hcCost + a.hpCost + b.hpCost - 1.0) < 1e-9);
});

test('without the contract, the off-peak windows say the share', () => {
  const [a] = tariffSplit([part('a')], [{ start: 0, meters: { a: { kwh: 4, cost: 0.8 } } }], () => 0.25);
  assert.deepEqual([a.hc, a.hp, a.hcCost, a.hpCost], [1, 3, 0.2, 0.6000000000000001]);
});
