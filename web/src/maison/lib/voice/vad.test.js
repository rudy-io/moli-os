import { test } from 'node:test';
import assert from 'node:assert/strict';
import { Vad } from './vad.js';

const run = (vad, level, frames) => {
  const events = [];
  for (let i = 0; i < frames; i++) {
    const e = vad.push(level);
    if (e) events.push(e);
  }
  return events;
};

test('silence, speech, then the end of the sentence', () => {
  const vad = new Vad();
  assert.deepEqual(run(vad, 0.002, 50), []);
  assert.deepEqual(run(vad, 0.08, 20), ['start']);
  assert.deepEqual(run(vad, 0.002, 30), []);
  assert.deepEqual(run(vad, 0.002, 30), ['end']);
});

test('a click is not speech', () => {
  const vad = new Vad();
  run(vad, 0.002, 50);
  assert.deepEqual(run(vad, 0.3, 3), []);
  assert.deepEqual(run(vad, 0.002, 50), []);
});

test('a pause inside a sentence does not end it', () => {
  const vad = new Vad();
  run(vad, 0.002, 50);
  run(vad, 0.08, 20);
  assert.deepEqual(run(vad, 0.002, 25), []);
  assert.deepEqual(run(vad, 0.08, 10), []);
  assert.deepEqual(run(vad, 0.002, 60), ['end']);
});

test('strict mode (Moli speaking) needs louder and longer speech', () => {
  const vad = new Vad();
  run(vad, 0.002, 50);
  vad.strict = true;
  assert.deepEqual(run(vad, 0.02, 30), []);
  assert.deepEqual(run(vad, 0.2, 10), []);
  assert.deepEqual(run(vad, 0.2, 10), ['start']);
});

test('the noise floor follows a steady hum', () => {
  const vad = new Vad();
  assert.deepEqual(run(vad, 0.008, 400), []);
  assert.ok(vad.threshold > 0.02);
  assert.deepEqual(run(vad, 0.1, 20), ['start']);
});
