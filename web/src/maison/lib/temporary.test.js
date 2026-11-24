import { test } from 'node:test';
import assert from 'node:assert/strict';
import { minutesLeft, hoursMinutes, boostChoices, temporaryOf } from './temporary.js';

const now = Date.parse('2026-01-05T10:00:00Z');

test('what is left of an end, rounded up to the minute', () => {
  assert.equal(minutesLeft('2026-01-05T11:00:00Z', now), 60);
  assert.equal(minutesLeft('2026-01-05T10:00:01Z', now), 1);
  assert.equal(minutesLeft('2026-01-05T10:00:00Z', now), null, 'over');
  assert.equal(minutesLeft(null, now), null);
  assert.equal(minutesLeft('', now), null);
  assert.deepEqual(hoursMinutes(42), { h: 0, m: '42' });
  assert.deepEqual(hoursMinutes(125), { h: 2, m: '05' });
});

test('a boost and a hold, each with its end', () => {
  const points = { boost: 2, boost_until: '2026-01-05T11:30:00Z', hold_until: null };
  const read = (key) => points[key];
  const { boost, hold } = temporaryOf(read, now);
  assert.deepEqual(boost, { by: 2, until: Date.parse('2026-01-05T11:30:00Z'), left: 90 });
  assert.equal(hold, null);

  points.boost = 0;
  points.hold_until = '2026-01-05T13:00:00Z';
  assert.equal(temporaryOf(read, now).boost, null);
  assert.equal(temporaryOf(read, now).hold.left, 180);
  assert.deepEqual(boostChoices(3), [1, 2]);
  assert.deepEqual(boostChoices(1.5), [1]);
  assert.deepEqual(boostChoices(undefined), []);
});
