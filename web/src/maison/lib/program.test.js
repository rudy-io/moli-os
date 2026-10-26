import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spans, sentRuns, slotsOf, unread } from './program.js';

// A real switch module's Monday: 17 °C all day, its hidden orders (kept
// from six-order wiring) changing at 6:00, 7:00 and 8:00.
const monday = [
  { at: 0, order: 'eco', celsius: 17 },
  { at: 24, order: 'comfort_1', celsius: 17 },
  { at: 28, order: 'comfort', celsius: 17 },
  { at: 32, order: 'eco', celsius: 17 },
];

test('on switch wiring a day is told by its setpoints alone', () => {
  assert.deepEqual(spans(monday, 'switch'), [{ at: 0, end: 96, order: null, celsius: 17 }]);
  const dip = [...monday.slice(0, 2), { at: 28, order: 'comfort', celsius: 20 }, monday[3]];
  assert.deepEqual(
    spans(dip, 'switch').map((s) => [s.at, s.end, s.celsius]),
    [
      [0, 28, 17],
      [28, 32, 20],
      [32, 96, 17],
    ],
  );
});

test('on switch wiring a part read without setpoints says no order', () => {
  // Read while the module was on six orders: its orders only.
  const stale = [
    { at: 0, order: 'eco', celsius: null },
    { at: 24, order: 'comfort', celsius: null },
  ];
  assert.deepEqual(spans(stale, 'switch'), [{ at: 0, end: 96, order: null, celsius: null }]);
  assert.equal(unread([stale], 'switch'), true);
  assert.equal(unread([monday, null], 'switch'), false);
  assert.equal(unread([stale], 'six'), false);
});

test('on six orders the same day is told by its orders', () => {
  assert.deepEqual(
    spans(monday, 'six').map((s) => [s.at, s.order, s.celsius]),
    [
      [0, 'eco', null],
      [24, 'comfort_1', null],
      [28, 'comfort', null],
      [32, 'eco', null],
    ],
  );
});

test('on four orders comfort −1 then comfort is one comfort run', () => {
  assert.deepEqual(
    spans(monday, 'four').map((s) => [s.at, s.end, s.order]),
    [
      [0, 24, 'eco'],
      [24, 32, 'comfort'],
      [32, 96, 'eco'],
    ],
  );
});

test('a switch module is sent its setpoints only', () => {
  assert.deepEqual(sentRuns(slotsOf(monday), 'switch'), [{ at: 0, celsius: 17 }]);
});
