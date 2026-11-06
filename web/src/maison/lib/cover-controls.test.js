import { test } from 'node:test';
import assert from 'node:assert/strict';
import { coverCommands, coverPositionTarget, hasSlowCoverControl } from './cover-controls.js';

const motor = (values) => ({ key: 'control', kind: { type: 'enum', values }, access: { read: false, write: false } });
const position = { key: 'opening', semantic: 'position', kind: { type: 'numeric', min: 0, max: 100, step: 1 }, access: { read: true, write: true } };
const slow = { key: 'percent_control_slow', semantic: 'other', unit: '%', kind: position.kind, access: { read: false, write: false } };

test('advertised partial slow actions keep Stop and preserve enum spelling without normal fallback', () => {
  const device = { online: false, points: [motor(['OPEN', 'STOP', 'CLOSE', 'OPEN_SLOW'])] };
  assert.equal(hasSlowCoverControl(device), true, 'read-only/offline devices still advertise their capabilities');
  assert.deepEqual(coverCommands(device, 'slow').map(({ command, value }) => [command, value]), [['open', 'OPEN_SLOW'], ['stop', 'STOP'], ['close', null]]);
  assert.deepEqual(coverCommands(device).map(({ value }) => value), ['OPEN', 'STOP', 'CLOSE']);
  device.points[0].kind.values.push('open_slow');
  assert.equal(hasSlowCoverControl(device), false, 'ambiguous casing never picks an arbitrary command');
  assert.equal(coverCommands(device, 'slow')[0].value, null);
});

test('a slow target never becomes a measured position and survives read-only capabilities', () => {
  const device = { points: [position, slow] };
  assert.equal(coverPositionTarget(device).point.key, 'opening');
  assert.equal(coverPositionTarget(device, 'slow').point.key, 'percent_control_slow');
  assert.equal(coverPositionTarget(device, 'slow').point.access.write, false);
  assert.equal(hasSlowCoverControl(device), true);
  for (const invalid of [{ ...slow, access: { read: true } }, { ...slow, unit: '°' }, { ...slow, kind: { type: 'numeric', min: -10, max: 100 } }]) {
    assert.equal(coverPositionTarget({ points: [position, invalid] }, 'slow'), null);
  }
  assert.equal(coverPositionTarget({ points: [position] }, 'slow'), null);
  assert.equal(coverPositionTarget({ points: [position, slow, slow] }, 'slow'), null);
});

test('unrelated and ambiguous actuators never gain slow controls', () => {
  assert.equal(hasSlowCoverControl({ points: [motor(['cycle'])] }), false);
  const device = { points: [motor(['open', 'close', 'open_slow']), { ...motor(['open', 'close']), key: 'other' }] };
  assert.deepEqual(coverCommands(device, 'slow'), []);
});
