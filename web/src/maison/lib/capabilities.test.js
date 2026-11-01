import { test } from 'node:test';
import assert from 'node:assert/strict';
import { pointOf, canWrite, numericRange, quantize, writableChoices, powerPoint, knownDeviceKind } from './capabilities.js';

const point = (key, kind, write = true, semantic = 'other') => ({ key, kind, access: { read: true, write }, semantic });
const device = (...points) => ({ online: true, points });
const numeric = (min, max, step) => ({ type: 'numeric', min, max, step });

test('writability belongs to each point and disappears offline', () => {
  const d = device(point('target_temperature', numeric(5, 35, 0.5)), point('mode', { type: 'enum', values: ['chaud'] }, false));
  assert.equal(canWrite(d, 'target_temperature'), true);
  assert.equal(canWrite(d, 'mode'), false);
  assert.equal(canWrite(d, 'on'), false);
  assert.equal(canWrite({ ...d, online: false }, 'target_temperature'), false);
  assert.equal(canWrite(null, 'target_temperature'), false);
  assert.equal(pointOf(d, 'missing'), null);
});

test('numeric controls use advertised bounds and do not invent missing limits', () => {
  const d = device(point('target_temperature', numeric(10, 35, 0.1)));
  assert.deepEqual(numericRange(d, 'target_temperature'), { min: 10, max: 35, step: 0.1, stepBase: 10, first: 10, last: 35 });
  for (const kind of [numeric(null, 30, 1), numeric(10, Infinity, 1), numeric(10, 10, 1), numeric(30, 10, 1), numeric(10, 30, -1), numeric(-1e308, 1e308, 1)]) {
    assert.equal(numericRange(device(point('target_temperature', kind)), 'target_temperature'), null);
  }
  assert.deepEqual(numericRange(device(point('volume', numeric(0, 100, null))), 'volume', 1), { min: 0, max: 100, step: 1, stepBase: 0, first: 0, last: 100 });
});

test('a control can opt into legacy limits only where the device omits them', () => {
  const fallback = { min: 16, max: 30 };
  assert.deepEqual(numericRange(device(point('target_temperature', numeric(undefined, null, null))), 'target_temperature', 0.5, fallback), { min: 16, max: 30, step: 0.5, stepBase: 0, first: 16, last: 30 });
  assert.deepEqual(numericRange(device(point('target_temperature', numeric(5, undefined, 0.1))), 'target_temperature', 0.5, fallback), { min: 5, max: 30, step: 0.1, stepBase: 5, first: 5, last: 30 });
  assert.deepEqual(numericRange(device(point('target_temperature', numeric(undefined, 35, 1))), 'target_temperature', 0.5, fallback), { min: 16, max: 35, step: 1, stepBase: 0, first: 16, last: 35 });
  for (const kind of [numeric(NaN, 35, 1), numeric(10, Infinity, 1), numeric(30, 10, 1), numeric(5, 30, 0)]) {
    assert.equal(numericRange(device(point('target_temperature', kind)), 'target_temperature', 0.5, fallback), null);
  }
  assert.equal(numericRange(device(), 'target_temperature', 0.5, fallback), null);
  assert.equal(numericRange(device(point('target_temperature', { type: 'text' })), 'target_temperature', 0.5, fallback), null);
});

test('setpoints are finite, clamped and quantized relative to the minimum', () => {
  const range = { min: 10.2, max: 12.4, step: 0.5 };
  assert.equal(quantize(10.5, range), 10.7);
  assert.equal(quantize(-100, range), 10.2);
  assert.equal(quantize(100, range), 12.2);
  assert.equal(quantize(22.3, { min: 5, max: 35, step: 0.1 }), 22.3);
  for (const value of [NaN, Infinity, undefined, null, '22']) assert.equal(quantize(value, range), null);
  assert.equal(quantize(20, null), null);
});

test('partial numeric bounds keep their step origin separate from presentation fallbacks', () => {
  const rangeFor = (kind, fallback = { min: 16, max: 30 }) => numericRange(device(point('target_temperature', kind)), 'target_temperature', 0.5, fallback);
  for (const kind of [numeric(undefined, 30, 0.3), numeric(null, undefined, 0.3)]) {
    const range = rangeFor(kind);
    assert.deepEqual(range, { min: 16, max: 30, step: 0.3, stepBase: 0, first: 16.2, last: 30 });
    assert.equal(quantize(19.8, range), 19.8);
    assert.equal(quantize(19.8 + range.step, range), 20.1);
    assert.equal(quantize(0, range), 16.2);
    assert.equal(quantize(100, range), 30);
  }
  const declaredMin = rangeFor(numeric(16.1, undefined, 0.3));
  assert.deepEqual(declaredMin, { min: 16.1, max: 30, step: 0.3, stepBase: 16.1, first: 16.1, last: 29.9 });
  assert.equal(quantize(19.7, declaredMin), 19.7);
  assert.equal(quantize(19.7 + declaredMin.step, declaredMin), 20);
  assert.equal(quantize(100, declaredMin), 29.9);
  const negative = rangeFor(numeric(undefined, 1, 0.3), { min: -1, max: 1 });
  assert.equal(quantize(0, negative), 0);
  assert.equal(quantize(-10, negative), -0.9);
  assert.equal(quantize(10, negative), 0.9);
});

test('a presentation fallback with no selectable step cannot create a numeric control', () => {
  assert.equal(numericRange(device(point('target_temperature', numeric(null, 30, 100))), 'target_temperature', 0.5, { min: 16, max: 30 }), null);
});

test('only advertised writable enum choices become commands', () => {
  const d = device(point('mode', { type: 'enum', values: ['chaud', 'chaud', 'auto'] }), point('control', { type: 'enum', values: ['open'] }, false));
  assert.deepEqual(writableChoices(d, 'mode'), ['chaud', 'auto']);
  assert.deepEqual(writableChoices(d, 'control'), []);
  assert.deepEqual(writableChoices({ ...d, online: false }, 'mode'), []);
  assert.deepEqual(writableChoices(device(point('mode', { type: 'text' })), 'mode'), []);
});

test('read-only on/off state can be shown without offering a toggle', () => {
  const d = device(point('on', { type: 'binary' }, false, 'on_off'));
  assert.equal(powerPoint(d), null);
  assert.equal(powerPoint(d, false), 'on');
  assert.equal(powerPoint(device(point('state', { type: 'text' }, false))), null);
});

test('bridge display hints cannot depend on friendly names or write access', () => {
  assert.equal(knownDeviceKind({ model: 'Home Assistant light', native_name: 'A', points: [] }), 'light');
  assert.equal(knownDeviceKind({ model: 'Home Assistant switch', native_name: 'Lamp', points: [] }), 'plug');
  assert.equal(knownDeviceKind({ model: 'Home Assistant media_player', points: [] }), 'speaker');
  assert.equal(knownDeviceKind({ model: 'Home Assistant climate', points: [] }), 'climate');
  assert.equal(knownDeviceKind({ model: 'Home Assistant cover', points: [] }), 'cover');
  assert.equal(knownDeviceKind({ model: 'room', points: [] }), null);
});
