import { test } from 'node:test';
import assert from 'node:assert/strict';
import { daily, byDrink, average, cupCounters } from './coffee-usage.js';

// Local times: the days are the house's.
const at = (day, hour, minute = 0) => new Date(2026, 9, day, hour, minute).getTime();
const now = at(26, 15);

test('the counters that count coffee', () => {
  const points = ['status', 'brews.coffee', 'brews.hot_water', 'brews.milk_foam', 'brews.espresso', 'brews_total'].map((key) => ({ key }));
  assert.deepEqual(cupCounters(points), ['brews.coffee', 'brews.espresso']);
});

test('each rise of a counter is cups, on the day it was seen', () => {
  const { days, since } = daily(
    {
      coffee: { before: [at(20, 8), 388], raw: [[at(25, 8), 389], [at(26, 7), 390], [at(26, 7, 5), 392]] },
      espresso: { before: null, raw: [[at(26, 10), 152], [at(26, 13), 153]] },
    },
    3,
    now,
  );
  assert.deepEqual(
    days.map((d) => d.total),
    [0, 1, 4],
  );
  assert.deepEqual(days[2].by, { coffee: 3, espresso: 1 });
  assert.equal(days[2].start, at(26, 0));
  assert.equal(since, at(20, 8));
});

test('a counter first seen, or reset, is no cup', () => {
  const { days, since } = daily({ coffee: { before: null, raw: [[at(26, 9), 500], [at(26, 10), 501], [at(26, 11), 0], [at(26, 12), 1]] } }, 1, now);
  assert.equal(days[0].total, 2);
  assert.equal(since, at(26, 9));
});

test('older than the days shown: not counted', () => {
  const { days } = daily({ coffee: { before: [at(1, 8), 10], raw: [[at(10, 8), 12]] } }, 7, now);
  assert.equal(
    days.reduce((s, d) => s + d.total, 0),
    0,
  );
});

test('a day across the clock change starts at midnight', () => {
  // 25 Oct 2026 is 25 hours long in Paris; every start is a local midnight.
  const { days } = daily({}, 30, now);
  assert.ok(days.every((d) => new Date(d.start).getHours() === 0));
  assert.equal(new Set(days.map((d) => new Date(d.start).getDate())).size, 30);
});

test('by drink, and the average since counting began', () => {
  const { days, since } = daily({ coffee: { before: null, raw: [[at(25, 8), 1], [at(25, 9), 3], [at(26, 9), 4]] }, espresso: { before: [at(25, 7), 5], raw: [[at(26, 9), 8]] } }, 7, now);
  assert.deepEqual(byDrink(days), [
    { drink: 'coffee', n: 3 },
    { drink: 'espresso', n: 3 },
  ]);
  // Two days counted (25 and 26), six cups.
  assert.equal(average(days, since), 3);
  assert.equal(average(days, null), null);
});
