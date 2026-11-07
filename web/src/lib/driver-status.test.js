import { test } from 'node:test';
import assert from 'node:assert/strict';
import { applyDriverStatus } from './driver-status.js';
import * as driverEvents from './driver-status.js';

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

test('a hot-added gateway retains Airzone metadata through later status events', () => {
  assert.equal(typeof driverEvents.applyDriverRegistration, 'function');
  const drivers = [];
  driverEvents.applyDriverRegistration(drivers, {instance:'new-airzone',kind:'airzone',integration:'airzone'});
  applyDriverStatus(drivers, {instance:'new-airzone',status:{state:'running'}});
  assert.equal(drivers[0].kind, 'airzone');
  assert.equal(drivers[0].status.state, 'running');
  assert.equal(drivers[0].integration, 'airzone');
});

test('registration metadata repairs a status-first arrival without losing its status or warning', () => {
  assert.equal(typeof driverEvents.applyDriverRegistration, 'function');
  const drivers = [{instance:'other',kind:'fake',status:{state:'running'}}];
  applyDriverStatus(drivers, {instance:'new-airzone',status:{state:'backoff',retry_in_ms:1000},warning:'Fixture warning'});
  const identity = drivers[1];
  const event = {instance:'new-airzone',kind:'airzone',integration:'airzone'};
  driverEvents.applyDriverRegistration(drivers,event);
  driverEvents.applyDriverRegistration(drivers,event);
  assert.equal(drivers.length,2);
  assert.equal(drivers[1],identity);
  assert.equal(drivers[1].kind,'airzone');
  assert.equal(drivers[1].status.state,'backoff');
  assert.equal(drivers[1].warning,'Fixture warning');
  assert.equal(drivers[0].kind,'fake');
});

test('withdrawing a runtime driver removes only its live registration and tolerates replay',()=>{
  assert.equal(typeof driverEvents.applyDriverUnregistration,'function');
  const drivers=[{instance:'paused',kind:'airzone'},{instance:'other',kind:'fake'}];
  driverEvents.applyDriverUnregistration(drivers,{instance:'paused'});
  driverEvents.applyDriverUnregistration(drivers,{instance:'paused'});
  assert.deepEqual(drivers,[{instance:'other',kind:'fake'}]);
});
