import { test } from 'node:test';
import assert from 'node:assert/strict';
import { textPointCommand } from './point-edit.js';

const point = { key: 'name', kind: { type: 'text' }, access: { read: true, write: true } };
const device = { id: 'fixture:zone', online: true, state: { name: { value: 'Zone A' } } };

test('text edit preserves the exact value in a single addressed command', () => {
  assert.deepEqual(textPointCommand(device, point, 'Zone B', false), { point: 'fixture:zone/name', value: 'Zone B' });
  assert.deepEqual(textPointCommand(device, point, 'A < B & C', false), { point: 'fixture:zone/name', value: 'A < B & C' });
});

test('read-only, offline, unconfirmed, pending and unchanged text cannot be submitted', () => {
  assert.equal(textPointCommand(device, { ...point, access: { write: false } }, 'Zone B', false), null);
  assert.equal(textPointCommand({ ...device, online: false }, point, 'Zone B', false), null);
  assert.equal(textPointCommand({ ...device, online: null }, point, 'Zone B', false), null);
  assert.equal(textPointCommand(device, point, 'Zone B', true), null);
  assert.equal(textPointCommand(device, point, 'Zone A', false), null);
  assert.equal(textPointCommand(device, point, null, false), null);
  assert.equal(textPointCommand(device, { ...point, kind: { type: 'numeric' } }, 'Zone B', false), null);
});

test('text commands enforce the core 4000-character limit including Unicode', () => {
  assert.ok(textPointCommand(device, point, 'é'.repeat(4000), false));
  assert.ok(textPointCommand(device, point, '🙂'.repeat(4000), false));
  assert.equal(textPointCommand(device, point, 'é'.repeat(4001), false), null);
  assert.equal(textPointCommand(device, point, '🙂'.repeat(4001), false), null);
});
