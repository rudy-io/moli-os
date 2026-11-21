import { test } from 'node:test';
import assert from 'node:assert/strict';
import { hourOf, dayHours, totals, peak } from './day-energy.js';

const H = 3_600_000;
const day0 = new Date(2026, 10, 20).getTime();

const meters = [
  { id: 'pince', role: 'total' },
  { id: 'linky', role: 'grid' },
  { id: 'toit', role: 'solar' },
  { id: 'injection', role: 'export' },
  { id: 'bat-in', role: 'charge' },
  { id: 'bat-out', role: 'discharge' },
];

test('an hour: bought, the sun, the battery make what the house used', () => {
  const s = { meters };
  // 0.3 bought, 1.0 produced, 0.1 sent out, 0.2 stored: 1.0 used, 0.7 from the sun.
  const h = hourOf(s, { meters: { pince: { kwh: 0.3 }, linky: { kwh: 0.31 }, toit: { kwh: 1 }, injection: { kwh: 0.1 }, 'bat-in': { kwh: 0.2 } } });
  assert.ok(Math.abs(h.used - 1.0) < 1e-9);
  assert.equal(h.bought, 0.3);
  assert.ok(Math.abs(h.sun - 0.7) < 1e-9);
  // The battery gives back at night: used without being bought, not from the sun either.
  const night = hourOf(s, { meters: { pince: { kwh: 0.2 }, 'bat-out': { kwh: 0.3 } } });
  assert.ok(Math.abs(night.used - 0.5) < 1e-9);
  assert.ok(Math.abs(night.sun - 0.3) < 1e-9, 'given back by the battery: counted with the sun it stored');
  // No reading of what was bought: no hour.
  assert.equal(hourOf(s, { meters: { toit: { kwh: 1 } } }), null);
  // Without panels: what was bought.
  const plain = hourOf({ meters: [{ id: 'linky', role: 'grid' }] }, { meters: { linky: { kwh: 0.8 } } });
  assert.deepEqual(plain, { used: 0.8, bought: 0.8, solar: 0, sun: 0 });
});

test('today against the usual day of the week before', () => {
  const buckets = [];
  for (let d = 7; d >= 1; d--) {
    for (let h = 0; h < 24; h++) buckets.push({ start: day0 - d * 24 * H + h * H, meters: { pince: { kwh: h === 19 ? 2 : 0.5 } } });
  }
  for (let h = 0; h <= 10; h++) buckets.push({ start: day0 + h * H, meters: { pince: { kwh: 0.4 }, toit: { kwh: h >= 9 ? 0.6 : 0 } } });
  const now = day0 + 10.5 * H;
  const { today, usual } = dayHours({ meters, buckets }, now);
  assert.equal(usual[19], 2);
  assert.equal(usual[3], 0.5);
  assert.equal(today[11], null);
  assert.ok(Math.abs(today[9].used - 1.0) < 1e-9);
  const t = totals(today, usual, now);
  // 0.4 × 11 bought + 0.6 × 2 from the sun.
  assert.ok(Math.abs(t.used - 5.6) < 1e-9);
  assert.ok(Math.abs(t.sun - 1.2) < 1e-9);
  assert.equal(t.share, 21);
  // Usual by 10:30: 10 hours and a half at 0.5.
  assert.ok(Math.abs(t.usual - 5.25) < 1e-9);
  assert.equal(t.gap, 7);
  assert.equal(peak(today), 9);
  assert.equal(totals(null, usual, now), null);
});
