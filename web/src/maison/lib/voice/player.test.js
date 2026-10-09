import { test } from 'node:test';
import assert from 'node:assert/strict';
import { rateOf, samples } from './player.js';

test('the rate is read from the content type', () => {
  assert.equal(rateOf('audio/pcm;rate=24000'), 24000);
  assert.equal(rateOf('audio/pcm; rate=16000'), 16000);
  assert.equal(rateOf('audio/pcm'), 24000);
});

test('16-bit little-endian samples, an odd byte kept for the next chunk', () => {
  // 0x4000 = 0.5, 0xc000 = -0.5, then half of 0x7fff.
  const first = samples(new Uint8Array([0x00, 0x40, 0x00, 0xc0, 0xff]), null);
  assert.deepEqual([...first.out], [0.5, -0.5]);
  assert.deepEqual([...first.carry], [0xff]);
  const second = samples(new Uint8Array([0x7f]), first.carry);
  assert.equal(second.out.length, 1);
  assert.ok(Math.abs(second.out[0] - 32767 / 32768) < 1e-9);
  assert.equal(second.carry.length, 0);
});

test('a chunk of a single byte waits whole', () => {
  const one = samples(new Uint8Array([0x01]), null);
  assert.equal(one.out.length, 0);
  assert.deepEqual([...one.carry], [0x01]);
});
