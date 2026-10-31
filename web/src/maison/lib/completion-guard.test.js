import { test } from 'node:test';
import assert from 'node:assert/strict';
import { completionGuard } from './completion-guard.js';

test('a failed older request cannot discard a newer setpoint gesture', async () => {
  const guard = completionGuard();
  let wanted = 20;
  let finish;
  const reply = new Promise((resolve) => { finish = resolve; });
  const owns = guard.capture();
  const first = reply.then(() => { if (owns()) wanted = null; });
  guard.change();
  wanted = 23;
  finish('failed');
  await first;
  assert.equal(wanted, 23);
});

test('a reset scheduled for a former card cannot clear the same value on another', () => {
  const guard = completionGuard();
  let wanted = 21;
  const owns = guard.capture();
  const delayedReset = () => { if (owns()) wanted = null; };
  guard.change(); // card identity changes, even if the desired number is identical
  delayedReset();
  assert.equal(wanted, 21);
  const current = guard.capture();
  assert.equal(current(), true);
  guard.change(); // cancelled pointer gesture
  assert.equal(current(), false);
});
