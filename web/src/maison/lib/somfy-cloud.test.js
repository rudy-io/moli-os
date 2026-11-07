import { test } from 'node:test';
import assert from 'node:assert/strict';
import { safeDate, retryDate, requestFinished, createCloudObserver } from './somfy-cloud.js';

test('hostile timestamps and indefinite retry never invent dates or throw', () => {
  assert.equal(safeDate(1e30, 'en', 1000), null);
  assert.equal(safeDate(1001, 'en', 1000), null);
  assert.equal(safeDate(-1, 'en', 1000), null);
  assert.equal(safeDate('bad', 'en', 1000), null);
  assert.ok(safeDate(100, 'en', 1000));
  assert.equal(retryDate(100, 18446744073709551615, 'en'), null);
});
test('pending request is correlated and not completed by a persisted Ready without live summary', () => {
  const row = { online: false, state: { request_id: 'one', phase: 'ready' } };
  assert.equal(requestFinished(row, 'one'), false);
  assert.equal(requestFinished({ ...row, online: true }, 'other'), false);
  assert.equal(requestFinished({ ...row, online: true }, 'one'), true);
  assert.equal(requestFinished({ state: { request_id: 'one', phase: 'persistence_pending' } }, 'one'), false);
  assert.equal(requestFinished({ state: { request_id: 'one', phase: 'unavailable', failure: { kind: 'rate_limited' } } }, 'one'), true);
});
test('owner denial never dispatches credentials and exposes only policy hint', async () => {
  const requests = [];
  let view;
  const observer = createCloudObserver(v => { view = v; }, async (url, options) => {
    requests.push([url, options]);
    return new Response('{"auth_policy":"owner_account"}', { status: 403 });
  });
  await observer.load();
  assert.equal(view.auth, 'owner_account');
  const input = { kind: 'login', username: 'fixture@example.com', password: 'SYNTHETIC_PASSWORD' };
  await observer.send(input);
  await observer.disconnect();
  assert.equal(requests.length, 1);
  assert.equal(input.password, '');
  observer.stop();
});
test('catalogue download and raw definitions keep exact server text and large integers', async () => {
  let view;
  const text = '{"revision":"r","freshness":{"current":false},"catalog":{"collections":{"scenarios":{"data":{"definitions":[{"raw":{"n":9007199254740993}}]}}}}}';
  const raw = '{"n":9007199254740993,"label":"<img src=x onerror=evil()>"}';
  const observer = createCloudObserver(v => { view = v; }, async url => new Response(url.endsWith('/definitions/scenarios/0?revision=r') ? raw : url.endsWith('/catalog') ? text : '{"instances":[{"instance":"fixture","state":{}}]}'));
  await observer.load();
  await observer.catalog();
  await observer.definition('scenarios', 0);
  assert.equal(view.catalogText, text);
  assert.equal(view.details['scenarios:0'], raw);
  observer.stop();
});

test('login polling stops on correlated site selection without prematurely requesting an observation', async () => {
  let view, reads = 0, posts = 0, catalogs = 0;
  const observer = createCloudObserver(v => { view = v; }, async (url, options) => {
    if (options.method === 'POST') { posts++; return new Response('{"request_id":"new"}', { status: 202 }); }
    if (url.endsWith('/catalog')) { catalogs++; return new Response('{}', { status: 404 }); }
    reads++;
    return Response.json({ instances: [{ instance: 'fixture', online: false, state: reads > 1 ? { request_id: 'new', phase: 'selecting_site' } : { request_id: 'old', phase: 'ready' } }] });
  }, async () => {});
  await observer.load();
  const login = { kind: 'login', username: 'fixture@example.com', password: 'PASSWORD' };
  const pending = observer.send(login);
  assert.equal(login.password, '');
  await pending;
  assert.equal(posts, 1);
  assert.equal(catalogs, 0);
  assert.equal(view.error, '');
  assert.equal(view.pending, null);
  observer.stop();
});
test('conflict and 403 discard password without PIN continuation or automatic retry', async () => {
  for (const status of [409, 403]) {
    let view, calls = 0;
    const observer = createCloudObserver(v => { view = v; }, async (_, options) => {
      calls++;
      return options.method ? Response.json({ auth_policy: 'pin_fallback' }, { status }) : Response.json({ instances: [{ instance: 'fixture', state: {} }] });
    });
    await observer.load();
    const login = { kind: 'login', username: 'fixture@example.com', password: 'PASSWORD' };
    await observer.send(login);
    assert.equal(login.password, '');
    assert.equal(calls, 2);
    assert.equal(view.pending, null);
    observer.stop();
  }
});
test('polling is bounded and ignores a different completed request id', async () => {
  let view, reads = 0, waits = 0;
  const observer = createCloudObserver(v => { view = v; }, async (_, options) => {
    if (options.method) return Response.json({ request_id: 'new' }, { status: 202 });
    reads++;
    return Response.json({ instances: [{ instance: 'fixture', online: true, state: { request_id: 'old', phase: 'ready' } }] });
  }, async () => { waits++; });
  await observer.load();
  assert.equal(reads, 1, 'no idle polling');
  await observer.send({ kind: 'refresh', generation: 'a' });
  assert.equal(waits, 60);
  assert.equal(reads, 61);
  assert.equal(view.error, 'pending_timeout');
  observer.stop();
});
test('unmount aborts an in-flight status fetch and prevents later changes or polling', async () => {
  let updates = 0, signal, finish;
  const observer = createCloudObserver(() => { updates++; }, (_, options) => {
    signal = options.signal;
    return new Promise(resolve => { finish = resolve; });
  });
  const loading = observer.load();
  observer.stop();
  const stoppedUpdates = updates;
  assert.equal(signal.aborted, true);
  finish(Response.json({ instances: [{ instance: 'late' }] }));
  await loading;
  assert.equal(updates, stoppedUpdates);
});
test('a new authoritative identity clears old private details and catalogue text', async () => {
  let view, generation = 'first';
  const observer = createCloudObserver(v => { view = v; }, async url => url.endsWith('/catalog') ? Response.json({ catalog: { collections: {} }, revision: 'r', freshness: { current: false } }) : Response.json({ instances: [{ instance: 'fixture', state: { generation, scope: { site_oid: 's', gateway_id: 'g', region: 'emea' } } }] }));
  await observer.load(); await observer.catalog();
  assert.ok(view.catalogText);
  generation = 'second';
  await observer.load();
  assert.equal(view.catalog, null);
  assert.equal(view.catalogText, '');
  observer.stop();
});
test('a delayed definition cannot attach to a replacement catalogue revision', async () => {
  let view, finish, revision = 'first';
  const observer = createCloudObserver(v => { view = v; }, async url => {
    if (url.includes('/definitions/')) return new Promise(resolve => { finish = resolve; });
    return url.endsWith('/catalog') ? Response.json({ revision, catalog: {}, freshness: {} }) : Response.json({ instances: [{ instance: 'fixture', state: {} }] });
  });
  await observer.load(); await observer.catalog();
  const detail = observer.definition('scenarios', 0);
  revision = 'second'; await observer.catalog();
  finish(new Response('{"old":true}'));
  await detail;
  assert.deepEqual(view.details, {});
  observer.stop();
});
test('an older status response cannot restore the previous connection after a newer load', async () => {
  let view;
  const replies = [];
  const observer = createCloudObserver(v => { view = v; }, () => new Promise(resolve => replies.push(resolve)));
  const first = observer.load();
  const second = observer.load();
  replies[1](Response.json({ instances: [{ instance: 'fixture', state: { generation: 'new' } }] }));
  await second;
  replies[0](Response.json({ instances: [{ instance: 'fixture', state: { generation: 'old' } }] }));
  await first;
  assert.equal(view.instances[0].state.generation, 'new');
  observer.stop();
});
test('the pending deadline also aborts a status fetch that never responds', async (t) => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  let statusSignal, reads = 0;
  const observer = createCloudObserver(() => {}, async (_, options) => {
    if (options.method) return Response.json({ request_id: 'new' }, { status: 202 });
    if (reads++ === 0) return Response.json({ instances: [{ instance: 'fixture', state: {} }] });
    statusSignal = options.signal;
    return new Promise((_, reject) => options.signal.addEventListener('abort', () => reject(new DOMException('Aborted', 'AbortError')), { once: true }));
  });
  await observer.load();
  const sending = observer.send({ kind: 'refresh', generation: 'a' });
  await new Promise(setImmediate);
  t.mock.timers.tick(120000);
  const deadlineAborted = statusSignal.aborted;
  observer.stop();
  await sending;
  assert.equal(deadlineAborted, true);
});


test('opening a connected view reads the cached catalogue without a provider refresh or write', async () => {
  const calls = [];
  let view;
  const observer = createCloudObserver(v => view = v, async (url, options) => {
    calls.push({ url, method: options.method ?? 'GET' });
    return url.endsWith('/catalog') ? Response.json({ revision: 'known', catalog: { collections: {} } })
      : Response.json({ instances: [{ instance: 'demo', state: { scope: { site_oid: 'demo' } } }] });
  });
  await observer.open();
  assert.equal(view.catalog.revision, 'known');
  assert.deepEqual(calls.map(c => c.method), ['GET', 'GET']);
  assert.ok(calls[1].url.endsWith('/catalog'));
  observer.stop();
});

test('opening a locked or unbound view never requests the private catalogue', async () => {
  for (const locked of [true, false]) {
    let calls = 0;
    const observer = createCloudObserver(() => {}, async () => {
      calls++;
      return locked ? Response.json({ auth_policy: 'pin_fallback' }, { status: 403 })
        : Response.json({ instances: [{ instance: 'demo', state: { phase: 'disconnected' } }] });
    });
    await observer.open();
    assert.equal(calls, 1);
    observer.stop();
  }
});


test('opening after a failed status refresh does not use the previous selection to read the catalogue', async () => {
  let fail = false;
  const calls = [];
  const observer = createCloudObserver(() => {}, async url => {
    calls.push(url);
    if (fail) throw new Error('network');
    return Response.json({ instances: [{ instance: 'demo', state: { scope: { site_oid: 'demo' } } }] });
  });
  await observer.load();
  fail = true;
  await observer.open();
  assert.equal(calls.length, 2);
  assert.ok(calls.every(url => !url.endsWith('/catalog')));
  observer.stop();
});

test('disconnect purges only the selected orphan account and forgets private browser data after success', async () => {
  let view, removed = false;
  const calls = [];
  const observer = createCloudObserver(v => view = v, async (url, options) => {
    calls.push({ url, method: options.method ?? 'GET', body: options.body });
    if (options.method === 'DELETE') { removed = true; return Response.json({ disconnected: true }); }
    if (url.endsWith('/catalog')) return Response.json({ revision: 'r', catalog: {} });
    if (url.includes('/definitions/')) return new Response('{"private":true}');
    return Response.json({ instances: removed ? [] : [{ instance: 'former gateway', configured: false, state: {} }] });
  });
  await observer.load(); await observer.catalog(); await observer.definition('scenarios', 0);
  assert.ok(view.catalogText);
  await observer.disconnect();
  assert.deepEqual(calls.filter(c => c.method !== 'GET'), [{ url: '/api/integrations/overkiz-cloud/former%20gateway', method: 'DELETE', body: undefined }]);
  assert.equal(view.catalog, null);
  assert.equal(view.catalogText, '');
  assert.deepEqual(view.details, {});
  assert.deepEqual(view.instances, []);
  assert.equal(view.notice, 'disconnected');
  assert.equal(view.pending, null);
  observer.stop();
});

test('a failed durable disconnect preserves the account and never claims success or retries', async () => {
  let view, deletes = 0;
  const observer = createCloudObserver(v => view = v, async (url, options) => {
    if (options.method === 'DELETE') { deletes++; return Response.json({}, { status: 503 }); }
    return url.endsWith('/catalog') ? Response.json({ revision: 'retained', catalog: {} })
      : Response.json({ instances: [{ instance: 'fixture', configured: false, state: {} }] });
  });
  await observer.load(); await observer.catalog();
  const retained = view.catalogText;
  await observer.disconnect();
  assert.equal(deletes, 1);
  assert.equal(view.catalogText, retained);
  assert.equal(view.selected, 'fixture');
  assert.equal(view.error, 'unavailable');
  assert.notEqual(view.notice, 'disconnected');
  assert.equal(view.pending, null);
  observer.stop();
});

test('disconnect blocks duplicate requests and prevents late catalogue or list reads from restoring data', async () => {
  let view, removed = false, deferList = false, finishList, finishCatalog, finishDelete, deletes = 0;
  const observer = createCloudObserver(v => view = v, async (url, options) => {
    if (options.method === 'DELETE') { deletes++; return new Promise(resolve => { finishDelete = resolve; }); }
    if (url.endsWith('/catalog')) return new Promise(resolve => { finishCatalog = resolve; });
    if (deferList) { deferList = false; return new Promise(resolve => { finishList = resolve; }); }
    return Response.json({ instances: removed ? [] : [{ instance: 'fixture', configured: false, state: {} }] });
  });
  await observer.load();
  const catalog = observer.catalog();
  deferList = true;
  const oldList = observer.load();
  const deletion = observer.disconnect();
  await observer.disconnect();
  assert.equal(deletes, 1);
  assert.notEqual(view.notice, 'disconnected');
  removed = true;
  finishDelete(Response.json({ disconnected: true }));
  await deletion;
  finishCatalog(Response.json({ revision: 'late', catalog: {} }));
  finishList(Response.json({ instances: [{ instance: 'fixture', state: { phase: 'ready' } }] }));
  await Promise.all([catalog, oldList]);
  assert.deepEqual(view.instances, []);
  assert.equal(view.catalog, null);
  assert.equal(view.catalogText, '');
  assert.equal(view.reading, false);
  observer.stop();
});

test('an orphan account cannot restart cloud login or refresh but remains removable', async () => {
  const methods = [];
  const observer = createCloudObserver(() => {}, async (_, options) => {
    methods.push(options.method ?? 'GET');
    return Response.json({ instances: [{ instance: 'fixture', configured: false, state: {} }] });
  });
  await observer.load();
  const credentials = { kind: 'login', username: 'fixture@example.com', password: 'SYNTHETIC' };
  await observer.send(credentials);
  await observer.send({ kind: 'refresh', generation: 'old' });
  assert.equal(credentials.password, '');
  assert.deepEqual(methods, ['GET']);
  observer.stop();
});

test('opening an orphan account does not request a catalogue from its retired source', async () => {
  const calls = [];
  const observer = createCloudObserver(() => {}, async url => {
    calls.push(url);
    return Response.json({ instances: [{ instance: 'fixture', configured: false, state: { scope: { site_oid: 'former' } } }] });
  });
  await observer.open();
  assert.deepEqual(calls, ['/api/integrations/overkiz-cloud']);
  observer.stop();
});

test('disconnect waits at most thirty seconds and keeps cached data when its outcome is unknown', async (t) => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  let view, deletionSignal;
  const observer = createCloudObserver(v => view = v, async (url, options) => {
    if (options.method === 'DELETE') {
      deletionSignal = options.signal;
      return new Promise((_, reject) => options.signal.addEventListener('abort', () => reject(new DOMException('Aborted', 'AbortError')), { once: true }));
    }
    return url.endsWith('/catalog') ? Response.json({ revision: 'retained', catalog: {} })
      : Response.json({ instances: [{ instance: 'fixture', state: {} }] });
  });
  await observer.load(); await observer.catalog();
  const retained = view.catalogText;
  const pending = observer.disconnect();
  t.mock.timers.tick(30000);
  const timedOut = deletionSignal.aborted;
  if (!timedOut) observer.stop();
  await pending;
  assert.equal(timedOut, true);
  assert.equal(view.catalogText, retained);
  assert.equal(view.error, 'pending_timeout');
  assert.equal(view.pending, null);
  observer.stop();
});

test('a confirmed credential removal clears private browser data but reports unfinished storage cleanup', async () => {
  let view, writes = 0;
  const observer = createCloudObserver(v => view = v, async (url, options) => {
    if (options.method === 'DELETE') { writes++; return Response.json({ error: 'cache_unavailable', credentials_removed: true }, { status: 503 }); }
    if (options.method) { writes++; return Response.json({}, { status: 409 }); }
    return url.endsWith('/catalog') ? Response.json({ revision: 'r', catalog: {} })
      : Response.json({ instances: [{ instance: 'fixture', configured: true, online: true, state: { phase: 'ready', scope: {} } }] });
  });
  await observer.open();
  assert.ok(view.catalogText);
  await observer.disconnect();
  assert.equal(view.error, 'cache_unavailable');
  assert.equal(view.notice, '');
  assert.equal(view.catalog, null);
  assert.equal(view.catalogText, '');
  assert.equal(view.instances[0].cleanup_pending, true);
  assert.equal(view.instances[0].online, false);
  assert.equal(view.instances[0].state.phase, 'disconnected');
  await observer.send({ kind: 'refresh', generation: 'old' });
  assert.equal(writes, 1, 'an unfinished cleanup cannot restart the source');
  observer.stop();
});
