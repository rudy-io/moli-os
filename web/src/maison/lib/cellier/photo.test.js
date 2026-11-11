import { test } from 'node:test';
import assert from 'node:assert/strict';
import { fitTicket } from './photo.js';

test('a long ticket is cut to 4000 on its long side', () => {
  assert.deepEqual(fitTicket(1000, 5000), { width: 800, height: 4000 });
});

test('a large photo is cut to 1200 on its short side', () => {
  assert.deepEqual(fitTicket(3000, 4000), { width: 1200, height: 1600 });
});

test('a small photo is never enlarged', () => {
  assert.deepEqual(fitTicket(600, 800), { width: 600, height: 800 });
});

test('landscape works the same way', () => {
  assert.deepEqual(fitTicket(4000, 3000), { width: 1600, height: 1200 });
});

test('the tightest limit wins', () => {
  // short side 1500 -> 1200 would give a long side of 6000: the long side rules.
  assert.deepEqual(fitTicket(1500, 7500), { width: 800, height: 4000 });
});
