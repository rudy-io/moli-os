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

const executableCatalogue = () => ({ revision: 'revision', freshness: { current: true, collections: { scenarios: true } }, catalog: {}, execution: { read_only: false, scenarios: [{ id: 'demo', permitted: true, executable: true, point: 'fixture/run', value: 'revision-token' }] } });

test('permission errors accept the parent API error field and retire capabilities on a changed catalogue', async () => {
  for (const [status, code] of [[401, 'invalid_pin'], [409, 'catalog_changed'], [403, 'global_read_only']]) {
    let view;
    const observer = createCloudObserver(v => view = v, async (url, options) => {
      if (options.method === 'POST') return Response.json({ error: code }, { status });
      return Response.json(url.endsWith('/catalog') ? executableCatalogue() : { instances: [{ instance: 'demo', online: true, state }] });
    });
    await observer.open();
    const before = view.catalogText;
    assert.equal(await observer.permission(input()), code);
    assert.equal(view.auth, 'owner');
    assert.equal(view.catalogText, before, 'the last observation remains available for inspection');
    if (code !== 'invalid_pin') {
      assert.equal(view.catalog.execution, null);
      assert.equal(view.catalog.freshness.current, false);
    }
    observer.stop();
  }
});

test('starting a disconnect retires execution immediately even when deletion later fails', async () => {
  let view, finish;
  const observer = createCloudObserver(v => view = v, async (url, options) => {
    if (options.method === 'DELETE') return new Promise(resolve => { finish = resolve; });
    return Response.json(url.endsWith('/catalog') ? executableCatalogue() : { instances: [{ instance: 'demo', configured: true, online: true, state }] });
  });
  await observer.open();
  const before = view.catalogText;
  const pending = observer.disconnect();
  const retiredAtStart = view.catalog.execution === null && view.catalog.freshness.current === false;
  finish(Response.json({}, { status: 503 }));
  await pending;
  assert.equal(retiredAtStart, true);
  assert.equal(view.catalogText, before);
  assert.equal(view.catalog.execution, null);
  assert.equal(view.notice, '');
  assert.equal(view.error, 'unavailable');
  observer.stop();
});

test('a lost source, removed config or cleanup marker cannot retain executable catalogue data', async () => {
  for (const patch of [{ online: false }, { configured: false }, { cleanup_pending: true }]) {
    let view, updated = false;
    const observer = createCloudObserver(v => view = v, async url => Response.json(url.endsWith('/catalog') ? executableCatalogue()
      : { instances: [{ instance: 'demo', configured: true, online: true, state, ...(updated ? patch : {}) }] }));
    await observer.open();
    updated = true;
    await observer.load();
    if (patch.cleanup_pending) {
      assert.equal(view.catalog, null);
      assert.equal(view.catalogText, '');
    } else {
      assert.equal(view.catalog.execution, null);
      assert.equal(view.catalog.freshness.current, false);
    }
    observer.stop();
  }
});

test('a permission refusal confirming credential removal clears data and blocks another PIN request', async () => {
  let view, posts = 0;
  const observer = createCloudObserver(v => view = v, async (url, options) => {
    if (options.method === 'POST') { posts++; return Response.json({ error: 'cache_unavailable', credentials_removed: true }, { status: 503 }); }
    return Response.json(url.endsWith('/catalog') ? executableCatalogue() : { instances: [{ instance: 'demo', configured: true, online: true, state }] });
  });
  await observer.open();
  assert.equal(await observer.permission(input()), 'cache_unavailable');
  assert.equal(view.catalog, null);
  assert.equal(view.catalogText, '');
  assert.equal(view.instances[0].cleanup_pending, true);
  await observer.permission(input());
  assert.equal(posts, 1);
  observer.stop();
});

test('an unavailable scenario collection does not turn fresh calendar observations into stale data', async () => {
  let view;
  const data = executableCatalogue();
  data.freshness.collections = { scenarios: false, calendar_rules: true };
  const observer = createCloudObserver(v => view = v, async url => Response.json(url.endsWith('/catalog') ? data
    : { instances: [{ instance: 'demo', configured: true, online: true, state }] }));
  await observer.open();
  assert.equal(view.catalog.execution, null);
  assert.equal(view.catalog.freshness.current, true);
  assert.equal(view.catalog.freshness.collections.calendar_rules, true);
  observer.stop();
});
