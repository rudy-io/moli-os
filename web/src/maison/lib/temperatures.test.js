import { test } from 'node:test';
import assert from 'node:assert/strict';
import { probesOf, lineOf, statsOf, scaleOf, valueAt } from './temperatures.js';

const dev = (id, ...keys) => ({ id, points: keys.map((key) => ({ key })) });

test('a room is measured by its sensors, else by its heaters', () => {
  const sensor = dev('z2m:s', 'temperature', 'humidity');
  const radiator = dev('tuya:r', 'temp_current', 'target_temperature');
  const door = dev('z2m:d', 'contact');
  assert.deepEqual(probesOf({ sensors: [sensor, door], climates: [radiator] }), [{ id: 'z2m:s', point: 'z2m:s/temperature' }]);
  assert.deepEqual(probesOf({ sensors: [door], climates: [radiator] }), [{ id: 'tuya:r', point: 'tuya:r/temp_current' }]);
  assert.deepEqual(probesOf({ sensors: [], climates: [] }), []);
});

test('a series becomes a line, from the value before the window to its end', () => {
  const raw = { from: 0, to: 100, before: [-5, 19], raw: [[40, 20.5], [70, 'x']], buckets: [] };
  assert.deepEqual(lineOf(raw), [[0, 19], [40, 19], [40, 20.5], [100, 20.5]]);
  const bucketed = { from: 0, to: 100, raw: [], last: [80, 21.5], buckets: [{ ts: 0, avg: 19 }, { ts: 50, avg: null }, { ts: 60, avg: 21 }] };
  assert.deepEqual(lineOf(bucketed), [[0, 19], [60, 21], [100, 21.5]]);
  assert.deepEqual(statsOf(lineOf(raw)), { min: 19, max: 20.5, last: 20.5 });
});

test('the rooms share whole degrees, at least four tall', () => {
  assert.deepEqual(scaleOf([[[0, 19.4]], [[0, 20.6]]]), { min: 18, max: 22, ticks: [18, 19, 20, 21, 22] });
  const wide = scaleOf([[[0, 4]], [[0, 23]]]);
  assert.deepEqual(wide.ticks, [0, 5, 10, 15, 20, 25]);
});

test('the value at a moment is the nearest one, not too far', () => {
  const line = [[0, 18], [100, 19], [200, 20]];
  assert.equal(valueAt(line, 140, 60), 19);
  assert.equal(valueAt(line, 160, 60), 20);
  assert.equal(valueAt(line, 400, 60), null);
});

test('a reading of a few seconds that falls back is a glitch', () => {
  const s = { from: 0, to: 1_000_000, raw: [[0, 18], [100_000, 20], [104_000, 18], [500_000, 18.5]], buckets: [] };
  assert.deepEqual(lineOf(s), [[0, 18], [500_000, 18], [500_000, 18.5], [1_000_000, 18.5]]);
  // Held long enough, it stays.
  const held = { ...s, raw: [[0, 18], [100_000, 20], [300_000, 18]] };
  assert.equal(lineOf(held).length, 6);
});
