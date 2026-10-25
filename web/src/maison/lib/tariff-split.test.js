import { test } from 'node:test';
import assert from 'node:assert/strict';
import { tariffSplit, meterSplit, addSplits } from './tariff-split.js';

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

test('a meter split by the contract keeps its own off-peak and peak', () => {
  // The house half and half, but the bedroom drew all its kWh off-peak.
  const b = {
    start: 0,
    meters: { a: { kwh: 1 }, b: { kwh: 3 } },
    by_period: { HC: { kwh: 2, cost: 0.2 }, HP: { kwh: 2, cost: 0.4 } },
    meter_periods: { a: { HP: { kwh: 1, cost: 0.2 } }, b: { HC: { kwh: 2, cost: 0.2 }, HP: { kwh: 1, cost: 0.2 } } },
  };
  assert.deepEqual(meterSplit(b, 'a'), { hc: 0, hp: 1, hcCost: 0, hpCost: 0.2 });
  const own = (id) => ({ ...part(id), split: (x) => meterSplit(x, id) });
  const [rb, ra] = tariffSplit([own('a'), own('b')], [b], () => 0);
  assert.deepEqual([ra.id, ra.hc, ra.hp], ['a', 0, 1]);
  assert.deepEqual([rb.id, rb.hc, rb.hp], ['b', 2, 1]);
});

test('a meter with kWh but no periods (before the first contract) is not split', () => {
  assert.equal(meterSplit({ meters: { a: { kwh: 1 } } }, 'a'), null);
  assert.deepEqual(meterSplit({ meters: {} }, 'a'), { hc: 0, hp: 0, hcCost: 0, hpCost: 0 });
  assert.equal(addSplits([{ hc: 1, hp: 1, hcCost: 0, hpCost: 0 }, null]), null);
});

test('a circuit net of what is measured inside it, never below zero', () => {
  const circuit = { hc: 2, hp: 1, hcCost: 0.2, hpCost: 0.2 };
  const tv = { hc: 0.5, hp: 1.5, hcCost: 0.05, hpCost: 0.3 };
  assert.deepEqual(addSplits([circuit, tv], -1), { hc: 1.5, hp: 0, hcCost: 0.15000000000000002, hpCost: 0 });
});
