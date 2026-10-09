import { test } from 'node:test';
import assert from 'node:assert/strict';
import { Resampler, level, toWav } from './pcm.js';

test('resampling 48 kHz to 16 kHz keeps one sample in three', () => {
  const r = new Resampler(48000, 16000);
  const out = r.push(Float32Array.from({ length: 480 }, (_, i) => i / 480));
  assert.equal(out.length, 160);
  assert.ok(Math.abs(out[10] - 30 / 480) < 1e-6);
});

test('resampling is continuous across blocks', () => {
  const r = new Resampler(44100, 16000);
  let n = 0;
  for (let i = 0; i < 100; i++) n += r.push(new Float32Array(441)).length;
  assert.ok(Math.abs(n - 16000) <= 1);
});

test('level is the RMS', () => {
  assert.equal(level(new Float32Array([0.5, -0.5, 0.5, -0.5])), 0.5);
  assert.equal(level(new Float32Array(0)), 0);
});

test('a WAV header around 16-bit PCM', () => {
  const wav = new DataView(toWav([new Float32Array([0, 1, -1])], 16000));
  const tag = (o) => String.fromCharCode(...[0, 1, 2, 3].map((i) => wav.getUint8(o + i)));
  assert.equal(tag(0), 'RIFF');
  assert.equal(tag(8), 'WAVE');
  assert.equal(wav.getUint32(24, true), 16000);
  assert.equal(wav.getUint16(34, true), 16);
  assert.equal(wav.getUint32(40, true), 6);
  assert.equal(wav.getInt16(46, true), 32767);
  assert.equal(wav.getInt16(48, true), -32768);
});
