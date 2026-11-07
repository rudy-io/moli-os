import { test } from 'node:test';
import assert from 'node:assert/strict';
import { seriesPoints, seriesSteps, programmeSteps, extent, scaleOf, linePath, stepPath, valueTicks, timeTicks } from './valve-history.js';
import { scheduleSpec, DAYS } from './schedule.js';

const spec = scheduleSpec({
  online: true,
  points: [
    { key: 'occupied_heating_setpoint', kind: { type: 'numeric', min: 4, max: 35, step: 0.5 }, access: { read: true, write: true } },
    ...DAYS.map((d) => ({ key: `weekly_schedule_${d}`, kind: { type: 'text' }, access: { read: true, write: true } })),
  ],
});

test('a bucketed window is drawn from its averages, an unbucketed one from its changes', () => {
  assert.deepEqual(seriesPoints({ from: 0, to: 100, buckets: [{ ts: 10, avg: 19 }, { ts: 50, avg: 20 }], raw: [] }), [[10, 19], [50, 20]]);
  assert.deepEqual(seriesPoints({ from: 0, to: 100, buckets: [], raw: [[10, 19], [50, 20]] }), [[10, 19], [50, 20], [100, 20]]);
  assert.deepEqual(seriesPoints(null), []);
});

test('a temperature that never moved over the window still draws a line', () => {
  assert.deepEqual(seriesPoints({ from: 0, to: 100, before: [-5, 19], buckets: [], raw: [] }), [[0, 19], [100, 19]]);
  assert.deepEqual(seriesPoints({ from: 0, to: 100, buckets: [], raw: [] }), []);
});

test('a step series starts from the value held before the window, unless a hole hides it', () => {
  assert.deepEqual(seriesSteps({ from: 100, to: 200, before: [50, 18], raw: [[150, 20]] }), [[100, 18], [150, 20]]);
  assert.deepEqual(seriesSteps({ from: 100, to: 200, before: [50, 18], raw: [[150, 20]], truncated: true }), [[150, 20]]);
  assert.deepEqual(seriesSteps({ from: 100, to: 200, raw: [[150, 'heat'], [160, 21], [170, NaN]] }), [[160, 21]]);
});

const at = (y, m, d, h = 0, min = 0) => new Date(y, m, d, h, min).getTime();

test('a schedule is laid over the days of the window, the day before holding the start', () => {
  // 2026-11-02 is a Monday.
  const days = { sunday: '00:00/16', monday: '00:00/17 06:00/20 22:00/16', tuesday: '00:00/17 06:00/21' };
  const from = at(2026, 10, 2, 7);
  const to = at(2026, 10, 3, 12);
  assert.deepEqual(programmeSteps(spec, days, from, to), [
    [from, 20],
    [at(2026, 10, 2, 22), 16],
    [at(2026, 10, 3, 0), 17],
    [at(2026, 10, 3, 6), 21],
  ]);
});

test('a day with no text holds the previous value, no text at all draws nothing', () => {
  const from = at(2026, 10, 3, 1);
  const to = at(2026, 10, 4, 1);
  assert.deepEqual(programmeSteps(spec, { monday: '00:00/17 22:00/16' }, from, to), [[from, 16]]);
  assert.deepEqual(programmeSteps(spec, {}, from, to), []);
  assert.deepEqual(programmeSteps(null, { monday: '00:00/17' }, from, to), []);
  assert.deepEqual(programmeSteps(spec, { monday: '00:00/17' }, to, from), []);
});

test('the range holds every value with a margin, and nothing without values', () => {
  const r = extent([[[0, 18], [1, 22]], [[0, 20]]]);
  assert.ok(r.min < 18 && r.max > 22);
  assert.equal(extent([[], []]), null);
  assert.deepEqual(extent([[[0, 20]]]).max - extent([[[0, 20]]]).min > 0, true);
});

test('the scale maps time left to right and values bottom to top, or is null', () => {
  const s = scaleOf(0, 100, { min: 10, max: 20 }, { left: 40, top: 10, width: 200, height: 100 });
  assert.equal(s.x(0), 40);
  assert.equal(s.x(100), 240);
  assert.equal(s.y(20), 10);
  assert.equal(s.y(10), 110);
  assert.equal(scaleOf(0, 0, { min: 1, max: 2 }, { left: 0, top: 0, width: 1, height: 1 }), null);
  assert.equal(scaleOf(0, 1, null, { left: 0, top: 0, width: 1, height: 1 }), null);
});

test('lines and steps are drawn as paths, a step holding until the next change', () => {
  const s = scaleOf(0, 100, { min: 0, max: 10 }, { left: 0, top: 0, width: 100, height: 100 });
  assert.equal(linePath([[0, 0], [50, 10]], s), 'M0,100L50,0');
  assert.equal(stepPath([[0, 0], [50, 10]], 100, s), 'M0,100H50V0H100');
  assert.equal(stepPath([], 100, s), '');
});

test('the vertical ticks are round numbers inside the range', () => {
  assert.deepEqual(valueTicks({ min: 17.2, max: 21.4 }), [18, 20]);
  assert.deepEqual(valueTicks({ min: 4, max: 24 }, 4), [5, 10, 15, 20]);
  assert.deepEqual(valueTicks(null), []);
});

test('the time ticks fall on the local clock inside the window', () => {
  const from = at(2026, 10, 5, 9, 30);
  assert.deepEqual(timeTicks(from, at(2026, 10, 5, 12, 10), 6), [at(2026, 10, 5, 10), at(2026, 10, 5, 11), at(2026, 10, 5, 12)]);
  assert.deepEqual(timeTicks(from, at(2026, 10, 6, 9, 30), 24), [at(2026, 10, 5, 12), at(2026, 10, 5, 18), at(2026, 10, 6, 0), at(2026, 10, 6, 6)]);
  assert.deepEqual(timeTicks(at(2026, 10, 5, 9), at(2026, 10, 8, 9), 168), [at(2026, 10, 6, 0), at(2026, 10, 7, 0), at(2026, 10, 8, 0)]);
});
