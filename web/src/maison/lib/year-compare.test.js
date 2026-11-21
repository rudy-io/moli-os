import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spans, sum, yearOverYear, trendTone } from './year-compare.js';

const at = (y, m, d, h = 12) => new Date(y, m, d, h).getTime();
const day = (y, m, d) => new Date(y, m, d).getTime();

/** Daily buckets from `from` to `to` (exclusive), `kwh(date)` per meter. */
function series(from, to, values) {
  const buckets = [];
  for (let t = from; t < to; t = new Date(new Date(t).getFullYear(), new Date(t).getMonth(), new Date(t).getDate() + 1).getTime()) {
    const meters = {};
    for (const [id, f] of Object.entries(values)) {
      const v = f(new Date(t));
      if (v != null) meters[id] = { kwh: v };
    }
    buckets.push({ start: t, meters });
  }
  return {
    meters: [
      { id: 'linky', role: 'grid' },
      { id: 'pince', role: 'total' },
      { id: 'toit', role: 'solar' },
    ],
    buckets,
  };
}

test('whole days only: from the 1st to yesterday, both years', () => {
  const s = spans(at(2026, 10, 20));
  assert.deepEqual(s.now, [day(2026, 10, 1), day(2026, 10, 20)]);
  assert.deepEqual(s.before, [day(2025, 10, 1), day(2025, 10, 20)]);
  assert.equal(s.days, 19);
  assert.equal(spans(at(2026, 10, 1)), null, 'the 1st: nothing to compare yet');
  // A 29th of February finds the 28th a year before.
  assert.deepEqual(spans(at(2028, 1, 29)).before, [day(2027, 1, 1), day(2027, 1, 28)]);
});

test('what was bought is the finest meter; the panels are theirs', () => {
  const sr = series(day(2025, 10, 1), day(2026, 10, 21), { linky: () => 9, pince: () => 10, toit: () => 4 });
  const s = spans(at(2026, 10, 20));
  assert.equal(sum(sr, 'bought', s.now).kwh, 190);
  assert.equal(sum(sr, 'solar', s.now).kwh, 76);
  assert.equal(sum(sr, 'bought', s.now).complete, true);
});

test('this month against a year ago', () => {
  const sr = series(day(2025, 10, 1), day(2026, 10, 21), {
    pince: (d) => (d.getFullYear() === 2026 ? 8 : 10),
    toit: (d) => (d.getFullYear() === 2026 ? 6 : 4),
  });
  const bought = yearOverYear(sr, at(2026, 10, 20), 'bought');
  assert.equal(bought.pct, -20);
  assert.equal(bought.before, 190);
  assert.equal(bought.month.getFullYear(), 2025);
  assert.equal(yearOverYear(sr, at(2026, 10, 20), 'solar').pct, 50);
});

test('a year ago not fully known: nothing said', () => {
  // History starts on 2025-11-05: the 1st to the 4th are missing.
  const sr = series(day(2025, 10, 5), day(2026, 10, 21), { pince: () => 10 });
  assert.equal(yearOverYear(sr, at(2026, 10, 20), 'bought'), null);
  // No panels: no solar comparison.
  const none = series(day(2025, 10, 1), day(2026, 10, 21), { pince: () => 10 });
  none.meters = none.meters.filter((m) => m.role !== 'solar');
  assert.equal(yearOverYear(none, at(2026, 10, 20), 'solar'), null);
});

test('one colour scale for every comparison', () => {
  assert.equal(trendTone(-12), 'good');
  assert.equal(trendTone(3), '');
  assert.equal(trendTone(-4), '');
  assert.equal(trendTone(22), 'warm');
  assert.equal(trendTone(115), 'alert');
  assert.equal(trendTone(35), 'alert');
  assert.equal(trendTone(null), '');
  // Production: more is better.
  assert.equal(trendTone(24, 'better'), 'good');
  assert.equal(trendTone(-30, 'better'), 'alert');
});
