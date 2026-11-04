import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createCloudObserver } from './somfy-cloud.js';
const state = { generation: 'generation', scope: { site_oid: 'demo' }, request_id: 'before', phase: 'ready' };
const catalogue = { revision: 'revision', catalog: {}, execution: { read_only: false, scenarios: [] } };
const input = () => ({ generation: 'generation', revision: 'revision', scenario_id: 'demo', enabled: true, pin: '123456' });
test('fresh PIN uses the dedicated endpoint once and is discarded before polling', async () => {
  let view, posted = false;
  const calls = [];
  const observer = createCloudObserver(v => view = v, async (url, options) => {
    calls.push({ url, method: options.method ?? 'GET', body: options.body });
    if (options.method === 'POST') { posted = true; return Response.json({ request_id: 'permission' }, { status: 202 }); }
    if (url.endsWith('/catalog')) return Response.json(catalogue);
    return Response.json({ instances: [{ instance: 'demo', online: true, state: { ...state, request_id: posted ? 'permission' : 'before' } }] });
  }, async () => {});
  await observer.open();
  const request = input();
  const pending = observer.permission(request);
  assert.equal(request.pin, '');
  assert.equal(await pending, '');
  const posts = calls.filter(c => c.method === 'POST');
  assert.equal(posts.length, 1);
  assert.ok(posts[0].url.endsWith('/scenario-permission'));
  assert.equal(JSON.parse(posts[0].body).pin, '123456');
  assert.equal(view.pending, null);
  assert.equal(view.catalog.revision, 'revision');
  assert.ok(!JSON.stringify(view).includes('123456'));
  observer.stop();
});

test('PIN, lockout, stale revision and read-only refusals preserve the owner view without retry', async () => {
  for (const [status, code] of [[403, 'pin_disabled'], [401, 'invalid_pin'], [429, 'pin_rate_limited'], [409, 'catalog_changed'], [403, 'global_read_only']]) {
    let view, posts = 0;
    const observer = createCloudObserver(v => view = v, async (url, options) => {
      if (options.method === 'POST') { posts++; return Response.json({ code }, { status }); }
      return Response.json(url.endsWith('/catalog') ? catalogue : { instances: [{ instance: 'demo', online: true, state }] });
    });
    await observer.open();
    const request = input();
    assert.equal(await observer.permission(request), code);
    assert.equal(request.pin, '');
    assert.equal(posts, 1);
    assert.equal(view.auth, 'owner');
    assert.equal(view.catalog.revision, 'revision');
    observer.stop();
  }
});

test('owner denial clears private data and a locked view never dispatches another PIN', async () => {
  let view, posts = 0;
  const observer = createCloudObserver(v => view = v, async (url, options) => {
    if (options.method === 'POST') { posts++; return Response.json({ code: 'owner_required', auth_policy: 'owner_account' }, { status: 403 }); }
    return Response.json(url.endsWith('/catalog') ? catalogue : { instances: [{ instance: 'demo', online: true, state }] });
  });
  await observer.open();
  await observer.permission(input());
  assert.equal(view.auth, 'owner_account');
  assert.equal(view.catalog, null);
  assert.equal(view.selected, '');
  const request = input();
  await observer.permission(request);
  assert.equal(request.pin, '');
  assert.equal(posts, 1);
  observer.stop();
});


test('a permission accepted then refused by the actor stays visible after the card is removed', async () => {
  let view, posted = false;
  const observer = createCloudObserver(v => view = v, async (url, options) => {
    if (options.method === 'POST') { posted = true; return Response.json({ request_id: 'permission' }, { status: 202 }); }
    if (url.endsWith('/catalog')) return Response.json(catalogue);
    return Response.json({ instances: [{ instance: 'demo', online: true, state: { ...state, request_id: posted ? 'permission' : 'before', failure: posted ? { kind: 'scope_mismatch' } : null } }] });
  });
  await observer.open();
  assert.equal(await observer.permission(input()), 'permission_failed');
  assert.equal(view.error, 'permission_failed');
  assert.equal(view.catalog, null);
  observer.stop();
});

test('timeout after permission acceptance retains a parent error with no retry', async () => {
  let view, posts = 0;
  const observer = createCloudObserver(v => view = v, async (url, options) => {
    if (options.method === 'POST') { posts++; return Response.json({ request_id: 'never-completes' }, { status: 202 }); }
    return Response.json(url.endsWith('/catalog') ? catalogue : { instances: [{ instance: 'demo', online: true, state }] });
  }, async () => {});
  await observer.open();
  assert.equal(await observer.permission(input()), 'pending_timeout');
  assert.equal(view.error, 'pending_timeout');
  assert.equal(posts, 1);
  observer.stop();
});
