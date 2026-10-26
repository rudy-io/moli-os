import { test } from 'node:test';
import assert from 'node:assert/strict';
import { alarms } from './stove-alarms.js';

const at = (day, h, m) => Date.UTC(2026, 9, day, h, m);

// A real night: lit at 22:28, in alarm at 22:30, stored at 22:39, reset on
// the stove at 01:17; the code, read from 01:15 only, kept until the next
// lighting at 07:01.
const modes = [
  [at(25, 21, 6), 'stand_by'],
  [at(25, 22, 28), 'ignition'],
  [at(25, 22, 29), 'awaiting_flame'],
  [at(25, 22, 30), 'alarm'],
  [at(25, 22, 39), 'mem_alarm'],
  [at(26, 1, 17), 'off'],
  [at(26, 7, 1), 'awaiting_flame'],
  [at(26, 7, 14), 'on'],
];
const codes = [
  [at(24, 2, 20), 'OK'],
  [at(26, 1, 15), 'A05'],
  [at(26, 7, 1), 'OK'],
];

test('an alarm reset on the stove is told once, with its code and the lighting it stopped', () => {
  assert.deepEqual(alarms(modes, codes), [{ start: at(25, 22, 30), end: at(26, 1, 17), code: 'A05', lighting: at(25, 22, 28), stopped: true }]);
});

test('an alarm not reset yet has no end', () => {
  const now = modes.slice(0, 5);
  const [a] = alarms(now, codes.slice(0, 2));
  assert.equal(a.end, null);
  assert.equal(a.code, 'A05');
});

test('an alarm without a code read, and a code said without stopping', () => {
  const quiet = [
    [at(20, 9, 0), 'on'],
    [at(20, 10, 0), 'alarm'],
    [at(20, 11, 0), 'off'],
  ];
  const coded = [
    [at(19, 0, 0), 'OK'],
    [at(21, 8, 0), 'A09'],
    [at(21, 9, 0), 'OK'],
  ];
  assert.deepEqual(alarms(quiet, coded), [
    { start: at(21, 8, 0), end: at(21, 9, 0), code: 'A09', lighting: null, stopped: false },
    { start: at(20, 10, 0), end: at(20, 11, 0), code: null, lighting: null, stopped: true },
  ]);
});

test('no history, no alarms', () => {
  assert.deepEqual(alarms([], []), []);
  assert.deepEqual(alarms(null, undefined), []);
});
