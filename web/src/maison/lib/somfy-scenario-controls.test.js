import { test } from 'node:test';
import assert from 'node:assert/strict';
import { scenarioControl, launchScenario } from './somfy-scenario-controls.js';
const row = { id: 'demo', permitted: true, executable: true, point: 'somfy:cloud:scenario:opaque/run', value: 'bound-revision' };
const devices = { 'somfy:cloud:scenario:opaque': { online: true, points: [{ key: 'run', kind: { type: 'enum', values: ['bound-revision'] }, access: { write: true } }] } };
const observation = (entry = row) => ({ freshness: { current: true, collections: { scenarios: true } }, execution: { read_only: false, scenarios: [entry] } });

test('launch forwards only the server-issued point and bound value through the guarded command client', async () => {
  const sent = [];
  assert.equal(await launchScenario(observation(), 'demo', devices, true, async (...args) => { sent.push(args); return 'ok'; }, 'Night'), 'ok');
  assert.deepEqual(sent, [[row.point, row.value, 'Night']]);
});
test('read-only, stale, missing, ambiguous and non-permitted scenarios never dispatch', async () => {
  const variants = [null, {}, { ...observation(), freshness: { current: true, collections: { scenarios: false } } }, { ...observation(), freshness: { current: false } },
    { ...observation(), execution: { ...observation().execution, read_only: true } },
    { ...observation(), execution: { read_only: false, scenarios: [row, row] } },
    ...[{ permitted: false }, { executable: false }, { point: '' }, { value: null }].map(patch => observation({ ...row, ...patch })),
  ];
  for (const candidate of variants) {
    assert.equal(scenarioControl(candidate, 'demo').executable, false);
    assert.equal(await launchScenario(candidate, 'demo', devices, true, () => assert.fail('must not command')), 'unavailable');
  }
});
test('held and failed outcomes are retained without claiming execution or retrying', async () => {
  for (const result of ['held', 'error']) {
    let calls = 0;
    assert.equal(await launchScenario(observation(), 'demo', devices, true, async () => { calls++; return result; }), result);
    assert.equal(calls, 1);
  }
});


test('a missing read-only policy fails closed instead of offering an authorization', () => {
  const view = { freshness: { current: true, collections: { scenarios: true } }, execution: { scenarios: [{ ...row, permitted: false, reason: 'permission_required' }] } };
  assert.equal(scenarioControl(view, 'demo').reason, 'global_read_only');
  assert.equal(scenarioControl(view, 'demo').executable, false);
});


test('live capability withdrawal, offline state or a replaced revision defeats a previously executable card', async () => {
  for (const live of [{}, { 'somfy:cloud:scenario:opaque': { ...devices['somfy:cloud:scenario:opaque'], online: false } },
    { 'somfy:cloud:scenario:opaque': { online: true, points: [{ key: 'run', kind: { type: 'enum', values: ['new-revision'] }, access: { write: true } }] } },
    { 'somfy:cloud:scenario:opaque': { online: true, points: [{ key: 'run', kind: { type: 'enum', values: ['bound-revision'] }, access: { write: false } }] } },
  ]) {
    assert.equal(await launchScenario(observation(), 'demo', live, true, () => assert.fail('no dispatch')), 'unavailable');
  }
});


test('a disconnected event stream cannot reuse a previously writable snapshot', async () => {
  assert.equal(scenarioControl(observation(), 'demo', devices, false).executable, false);
  assert.equal(await launchScenario(observation(), 'demo', devices, false, () => assert.fail('no command')), 'unavailable');
});
