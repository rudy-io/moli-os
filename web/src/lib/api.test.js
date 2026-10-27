import { test } from 'node:test';
import assert from 'node:assert/strict';
import { request, Refused } from './api.js';

/** A fetch that answers `status` with `body`, and keeps what it was asked. */
function answering(status, body) {
  const asked = [];
  globalThis.fetch = async (url, init) => {
    asked.push({ url, ...init });
    return { ok: status < 400, status, statusText: 'Forbidden', json: async () => (body === undefined ? Promise.reject(new Error('empty')) : body) };
  };
  return asked;
}

test('JSON goes out as the dashboard, the answer comes back', async () => {
  const asked = answering(200, { ok: true });
  assert.deepEqual(await request('POST', '/api/command', { point: 'a/on', value: true }), { ok: true });
  assert.equal(asked[0].headers['x-moli-origin'], 'ui');
  assert.equal(asked[0].headers['content-type'], 'application/json');
  assert.equal(asked[0].body, '{"point":"a/on","value":true}');
});

test('a file goes as it is, with its type', async () => {
  const asked = answering(200, {});
  const file = new Uint8Array([1, 2]);
  await request('POST', '/api/plan/images', file, { type: 'image/png' });
  assert.equal(asked[0].body, file);
  assert.equal(asked[0].headers['content-type'], 'image/png');
});

test('an order held by the guard is an answer, with why', async () => {
  answering(202, { pending: 7, hold: 'protected' });
  assert.equal((await request('POST', '/api/command', {})).hold, 'protected');
});

test("a refusal says the house's words, its status and its answer", async () => {
  answering(403, { error: 'le code', hold: 'named' });
  await assert.rejects(request('PUT', '/api/labels/x', {}), (err) => err instanceof Refused && err.status === 403 && err.message === 'le code' && err.body.hold === 'named');
  answering(500, undefined);
  await assert.rejects(request('GET', '/api/x'), (err) => err.message === 'Forbidden' && err.status === 500);
});
