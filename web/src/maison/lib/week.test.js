import { test } from 'node:test';
import assert from 'node:assert/strict';
import { DAYS, scheduleSpec, parseDay } from './schedule.js';
import { warmth, bands, minuteAt, dragBoundary, now, copyWeek, MINUTES_IN_DAY } from './week.js';

const point = (key, kind, write = true) => ({ key, kind, access: { read: true, write }, semantic: 'other' });
const thermostat = ({ min = 5, max = 25, days = DAYS } = {}) => ({
  online: true,
  points: [
    point('occupied_heating_setpoint', { type: 'numeric', min, max, step: 0.5 }),
    point('system_mode', { type: 'enum', values: ['off', 'heat', 'auto'] }),
    ...days.map((d) => point(`weekly_schedule_${d}`, { type: 'text' })),
  ],
});
const spec = scheduleSpec(thermostat());

test('a setpoint looks the same in every room: 15 °C cool, 23 °C warm', () => {
  assert.equal(warmth(15), 0);
  assert.equal(warmth(12), 0);
  assert.equal(warmth(19), 50);
  assert.equal(warmth(23), 100);
  assert.equal(warmth(28), 100);
  assert.equal(warmth('x'), 0);
});

test('a day is bands covering the whole day, in order', () => {
  assert.deepEqual(bands(spec, '00:00/17 06:30/19.5 22:30/17'), [
    { start: 0, end: 390, deg: 17, index: 0 },
    { start: 390, end: 1350, deg: 19.5, index: 1 },
    { start: 1350, end: MINUTES_IN_DAY, deg: 17, index: 2 },
  ]);
  // Nothing readable: one band at the lowest setpoint, never an empty day.
  assert.deepEqual(bands(spec, ''), [{ start: 0, end: MINUTES_IN_DAY, deg: 5, index: 0 }]);
});

test('a pointer lands on the 15-minute grid, never on midnight', () => {
  assert.equal(minuteAt(0, 960), 15);
  assert.equal(minuteAt(480, 960), 720);
  assert.equal(minuteAt(483, 960), 720);
  assert.equal(minuteAt(960, 960), MINUTES_IN_DAY - 15);
  assert.equal(minuteAt(100, 0), 0);
});

test('a boundary moves between its neighbours only', () => {
  const day = parseDay(spec, '00:00/17 06:30/19.5 22:30/17');
  assert.equal(dragBoundary(spec, day, 1, 420)[1].min, 420);
  assert.deepEqual(dragBoundary(spec, day, 1, 1350), day, 'onto the next step');
  assert.deepEqual(dragBoundary(spec, day, 1, 1400), day, 'past the next step');
  assert.deepEqual(dragBoundary(spec, day, 0, 60), day, 'midnight stays');
  assert.equal(dragBoundary(spec, day, 2, 1380)[2].min, 1380);
});

test('today is said Monday first', () => {
  assert.deepEqual(now(new Date(2026, 10, 16, 6, 45)), { day: 'monday', minute: 405 });
  assert.deepEqual(now(new Date(2026, 10, 22, 23, 59)), { day: 'sunday', minute: 1439 });
});

test('a week copied takes the target\'s bounds and days', () => {
  const texts = Object.fromEntries(DAYS.map((d) => [d, '00:00/16 07:00/24']));
  const narrow = scheduleSpec(thermostat({ min: 17, max: 22, days: DAYS.slice(0, 5) }));
  assert.deepEqual(copyWeek(spec, narrow, texts), Object.fromEntries(DAYS.slice(0, 5).map((d) => [d, '00:00/17 07:00/22'])));
  assert.deepEqual(copyWeek(spec, spec, texts).sunday, '00:00/16 07:00/24');
});
