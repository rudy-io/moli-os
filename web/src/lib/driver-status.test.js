import { test } from 'node:test';
import assert from 'node:assert/strict';
import { applyDriverStatus } from './driver-status.js';

test('live warnings update only their integration and disappear with the next run', () => {
  const drivers = [
    { instance: 'ha', kind: 'homeassistant', integration: 'homeassistant', status: { state: 'running' } },
    { instance: 'other', kind: 'fake', status: { state: 'running' }, warning: 'Keep me' },
  ];
  applyDriverStatus(drivers, { instance: 'ha', status: { state: 'running' }, warning: 'HTTP sends the token unencrypted' });
  assert.equal(drivers[0].warning, 'HTTP sends the token unencrypted');
  assert.equal(drivers[0].status.state, 'running');
  assert.equal(drivers[0].integration, 'homeassistant');
  applyDriverStatus(drivers, { instance: 'ha', status: { state: 'starting' } });
  assert.equal(drivers[0].warning, null);
  assert.equal(drivers[1].warning, 'Keep me');
});

test('a warning received before the snapshot is visible immediately', () => {
  const drivers = [];
  applyDriverStatus(drivers, { instance: 'new', status: { state: 'running' }, warning: 'Connection warning' });
  assert.equal(drivers[0].warning, 'Connection warning');
  assert.equal(drivers[0].instance, 'new');
  applyDriverStatus(drivers, { instance: 'new', status: { state: 'running' }, warning: null });
  assert.equal(drivers[0].warning, null);
});
