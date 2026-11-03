import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { parse } from 'svelte/compiler';
import { build } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import { saveDeviceLabels, setDeviceHidden } from './device-management.js';

const response = (data, status = 200) => ({ ok: status >= 200 && status < 300, status, json: async () => data });

test('label metadata uses the encoded stable identity and preserves explicit clearing', async () => {
  const calls = [];
  const fetcher = async (...args) => { calls.push(args); return response({ name: 'Living room' }); };
  assert.deepEqual(await saveDeviceLabels('fixture:zone/1 #é', { name: 'Living room', room: '' }, fetcher), {
    ok: true, data: { name: 'Living room' },
  });
  assert.deepEqual(calls, [[ '/api/labels/fixture%3Azone%2F1%20%23%C3%A9', {
    method: 'PUT', headers: { 'content-type': 'application/json', 'x-moli-origin': 'ui' },
    body: JSON.stringify({ name: 'Living room', room: '' }),
  } ]]);
});

test('visibility is a boolean metadata PUT for hiding and restoring', async () => {
  const calls = [];
  for (const hidden of [true, false]) {
    const result = await setDeviceHidden('fixture:zone/2', hidden, async (...args) => {
      calls.push(args); return response({ hidden });
    });
    assert.deepEqual(result, { ok: true, data: { hidden } });
  }
  assert.deepEqual(calls, [true, false].map((hidden) => ['/api/devices/fixture%3Azone%2F2/visibility', {
    method: 'PUT', headers: { 'content-type': 'application/json', 'x-moli-origin': 'ui' },
    body: JSON.stringify({ hidden }),
  }]));
});

test('invalid metadata never reaches the network and label limits count Unicode characters', async () => {
  const fetcher = () => assert.fail('Invalid metadata must not be sent');
  for (const label of [{ name: 'x'.repeat(61) }, { room: 'x'.repeat(61) }, { name: 1 }, null, {}, { command: true }]) {
    assert.deepEqual(await saveDeviceLabels('fixture:zone', label, fetcher), { ok: false, error: 'invalid' });
  }
  assert.deepEqual(await saveDeviceLabels('', { name: 'Zone' }, fetcher), { ok: false, error: 'invalid' });
  assert.deepEqual(await setDeviceHidden('fixture:zone', 'true', fetcher), { ok: false, error: 'invalid' });
  assert.deepEqual(await setDeviceHidden('', true, fetcher), { ok: false, error: 'invalid' });
  assert.equal((await saveDeviceLabels('fixture:zone', { name: '🌡'.repeat(60) }, async () => response({}))).ok, true);
});

test('403 stays distinguishable even when its error response is not JSON, with no retry', async () => {
  let count = 0;
  const result = await setDeviceHidden('fixture:zone', true, async () => {
    count++;
    return { ok: false, status: 403, json: async () => { throw new SyntaxError('HTML'); } };
  });
  assert.deepEqual(result, { ok: false, error: 'auth', status: 403 });
  assert.equal(count, 1);
});

test('network failures and server refusals do not become successful metadata updates', async () => {
  assert.deepEqual(await saveDeviceLabels('fixture:zone', { name: '' }, async () => { throw new Error('network'); }), {
    ok: false, error: 'network',
  });
  for (const status of [404, 429, 500]) {
    assert.deepEqual(await setDeviceHidden('fixture:zone', false, async () => response({ error: 'Fixture refusal' }, status)), {
      ok: false, error: 'server', status,
    });
  }
});

test('malformed successes are rejected, including a visibility response contradicting the request', async () => {
  for (const data of [null, [], { name: 42 }, { room: false }, { error: 'Not saved' }, { pending: 1 }]) {
    assert.deepEqual(await saveDeviceLabels('fixture:zone', { name: '' }, async () => response(data)), {
      ok: false, error: 'response', status: 200,
    });
  }
  for (const data of [{}, { hidden: 'true' }, { hidden: false }]) {
    assert.deepEqual(await setDeviceHidden('fixture:zone', true, async () => response(data)), {
      ok: false, error: 'response', status: 200,
    });
  }
  assert.deepEqual(await saveDeviceLabels('fixture:zone', { name: '' }, async () => ({
    ok: true, status: 200, json: async () => { throw new SyntaxError('HTML'); },
  })), { ok: false, error: 'response', status: 200 });
  assert.deepEqual(await saveDeviceLabels('fixture:zone', { name: '' }, async () => response({}, 202)), {
    ok: false, error: 'response', status: 202,
  });
});

let client;
const globals = new Map(['document', 'localStorage'].map((key) => [key, Object.getOwnPropertyDescriptor(globalThis, key)]));
before(async () => {
  delete globalThis.localStorage;
  globalThis.document = { querySelector: () => null, documentElement: {} };
  // Exercise the actual component handlers and effects with the client runtime.
  // Only this test bundle exposes handlers; no test API ships in the component.
  const result = await build({
    configFile: false,
    root: fileURLToPath(new URL('../../../', import.meta.url)),
    logLevel: 'silent',
    plugins: [{
      name: 'device-management-interaction-fixture', enforce: 'pre',
      resolveId(id) { if (id === 'virtual:management-fixture') return '\0' + id; },
      load(id) {
        if (id !== '\0virtual:management-fixture') return;
        return `
          import Management from '/src/maison/ui/AirzoneManagement.svelte';
          import { home } from '/src/maison/lib/home.svelte.js';
          import { flushSync } from 'svelte';
          import { effect_root, proxy } from 'svelte/internal/client';
          export { home, flushSync };
          export function mount(input) {
            const props = proxy(input);
            let component;
            const dispose = effect_root(() => { component = Management(null, props); });
            flushSync();
            return { component, dispose, props };
          }
        `;
      },
      transform(source, id) {
        if (!id.endsWith('/maison/ui/AirzoneManagement.svelte')) return;
        const { content } = parse(source, { modern: true }).instance;
        return `<script>${source.slice(content.start, content.end)}
          export { edit, cancelEdit, requestHide, cancelHide, saveLabels, changeVisibility };
          export function typeLabel(name, room) { draft = { name, room }; }
          export function search(text, visibility = 'all') { query = text; filter = visibility; }
          export function intent() {
            return { editing, draft: { ...draft }, confirmId, pending, awaitingPin, error, success, warning,
              visible: visibleDevices.map((device) => device.id) };
          }
        </script>`;
      },
    }, svelte()],
    build: {
      write: false, minify: false,
      lib: { entry: 'virtual:management-fixture', formats: ['es'] },
      rolldownOptions: { input: 'virtual:management-fixture' },
    },
  });
  const chunk = (Array.isArray(result) ? result[0] : result).output.find((entry) => entry.type === 'chunk' && entry.isEntry);
  const source = `${chunk.code}\n//# sourceURL=device-management-interaction-fixture.js`;
  client = await import(`data:text/javascript;base64,${Buffer.from(source).toString('base64')}`);
});
after(() => {
  for (const [key, descriptor] of globals) {
    if (descriptor) Object.defineProperty(globalThis, key, descriptor);
    else delete globalThis[key];
  }
});

const devices = [
  { id: 'fixture:zone/1', native_name: 'Native living room', label: { name: 'My zone', room: 'Living room' }, points: [], state: {} },
  { id: 'fixture:gateway', native_name: 'Gateway', label: {}, points: [], state: {} },
];

function fixture(t, handler = () => response({}), extraProps = {}) {
  const calls = [], changes = [];
  t.mock.method(globalThis, 'fetch', async (url, options) => {
    assert.match(url, /^\/api\/(labels\/|devices\/.*\/visibility$)/);
    calls.push({ url, ...options });
    return handler(url, options);
  });
  client.home.held = null;
  const mounted = client.mount({ devices: structuredClone(devices), hidden: ['fixture:gateway'],
    connected: true, onchanged: async (change) => { changes.push(change); }, ...extraProps });
  t.after(() => { mounted.dispose(); client.home.held = null; });
  return { ...mounted, calls, changes };
}

test('opening and filtering metadata keeps hidden devices inspectable and never writes', (t) => {
  const { component, calls } = fixture(t);
  assert.deepEqual(component.intent().visible, ['fixture:zone/1', 'fixture:gateway']);
  component.search('', 'hidden');
  assert.deepEqual(component.intent().visible, ['fixture:gateway']);
  component.search('living');
  assert.deepEqual(component.intent().visible, ['fixture:zone/1']);
  component.search('fixture:gateway');
  assert.deepEqual(component.intent().visible, ['fixture:gateway']);
  component.edit(devices[1]);
  assert.deepEqual(component.intent().draft, { name: '', room: '' });
  assert.deepEqual(calls, []);
});

test('a selected device opens its label editor without granting permission or writing', (t) => {
  const { component, calls } = fixture(t, undefined, { selectedId: 'fixture:zone/1' });
  assert.equal(component.intent().editing, 'fixture:zone/1');
  assert.deepEqual(component.intent().draft, { name: 'My zone', room: 'Living room' });
  assert.deepEqual(calls, []);
  assert.equal(client.home.held, null);
});

test('pending label writes reject duplicate submission and keep the draft on failure', async (t) => {
  let finish;
  const { component, calls, changes } = fixture(t, () => new Promise((resolve) => { finish = resolve; }));
  component.edit(devices[0]);
  component.typeLabel('New label', 'New room');
  const pending = component.saveLabels();
  assert.equal(component.intent().pending, true);
  await component.saveLabels();
  component.cancelEdit();
  assert.equal(component.intent().editing, devices[0].id);
  assert.equal(calls.length, 1);
  finish(response({ error: 'Fixture failure' }, 500));
  await pending;
  assert.equal(component.intent().pending, false);
  assert.equal(component.intent().error, 'server');
  assert.equal(component.intent().success, null);
  assert.deepEqual(component.intent().draft, { name: 'New label', room: 'New room' });
  assert.deepEqual(changes, []);
});

test('403 defers the same captured metadata to PinSheet and does not retry without human action', async (t) => {
  let count = 0;
  const { component, calls, changes } = fixture(t, () => ++count === 1 ? response({}, 403) : response({ name: 'New label' }));
  component.edit(devices[0]);
  component.typeLabel('New label', '');
  await component.saveLabels();
  assert.equal(calls.length, 1);
  assert.equal(component.intent().awaitingPin, true);
  assert.equal(client.home.held.reason, 'configuration');
  assert.equal(typeof client.home.held.custom, 'function');
  component.edit(devices[1]);
  assert.equal(component.intent().editing, devices[0].id);
  await component.saveLabels();
  assert.equal(calls.length, 1);
  const retry = client.home.held.custom;
  client.home.held = null; // confirmHeld clears the sheet after its own successful login.
  await retry();
  assert.equal(calls.length, 2);
  assert.deepEqual(calls[1], calls[0]);
  assert.equal(component.intent().editing, null);
  assert.equal(component.intent().success, 'labels_saved');
  assert.deepEqual(changes, [{ id: devices[0].id, kind: 'labels', data: { name: 'New label' } }]);
  await retry();
  assert.equal(calls.length, 2);
});

test('cancelling PinSheet preserves the form and a second 403 never reopens an authentication loop', async (t) => {
  const { component, calls } = fixture(t, () => response({}, 403));
  component.edit(devices[0]);
  component.typeLabel('Kept draft', 'Kept room');
  await component.saveLabels();
  const cancelledRetry = client.home.held.custom;
  client.home.held.onCancel();
  client.home.held = null;
  await cancelledRetry();
  assert.equal(calls.length, 1);
  assert.equal(component.intent().awaitingPin, false);
  assert.deepEqual(component.intent().draft, { name: 'Kept draft', room: 'Kept room' });
  await component.saveLabels();
  const retry = client.home.held.custom;
  client.home.held = null;
  await retry();
  assert.equal(calls.length, 3);
  assert.equal(component.intent().error, 'auth');
  assert.equal(client.home.held, null);
});

test('hiding needs an explicit inline confirmation while restoring uses a single action', async (t) => {
  const { component, calls, changes } = fixture(t, (url, options) => response(JSON.parse(options.body)));
  await component.changeVisibility(devices[0].id, true);
  assert.deepEqual(calls, []);
  component.requestHide(devices[0].id);
  assert.equal(component.intent().confirmId, devices[0].id);
  assert.deepEqual(calls, []);
  await component.changeVisibility(devices[0].id, true);
  assert.equal(component.intent().confirmId, null);
  assert.equal(component.intent().success, 'hidden_saved');
  await component.changeVisibility(devices[1].id, false);
  assert.equal(component.intent().success, 'restored');
  assert.deepEqual(changes.map(({ kind, data }) => ({ kind, data })), [
    { kind: 'visibility', data: { hidden: true } }, { kind: 'visibility', data: { hidden: false } },
  ]);
});

test('lost connection prevents metadata writes, including a previously approved retry', async (t) => {
  const { component, calls, props } = fixture(t, () => response({}, 403));
  component.edit(devices[0]);
  await component.saveLabels();
  const retry = client.home.held.custom;
  client.home.held = null;
  props.connected = false;
  client.flushSync();
  await retry();
  assert.equal(component.intent().error, 'connection');
  assert.equal(component.intent().awaitingPin, false);
  assert.equal(calls.length, 1);
  await component.saveLabels();
  assert.equal(calls.length, 1);
});

test('leaving the component invalidates its pending approval without clearing another sheet', async (t) => {
  const { component, calls, dispose } = fixture(t, () => response({}, 403));
  component.edit(devices[0]);
  await component.saveLabels();
  const retry = client.home.held.custom;
  const other = { custom() {}, label: 'Another action' };
  client.home.held = other;
  client.flushSync();
  assert.equal(client.home.held.label, 'Another action');
  assert.notEqual(client.home.held.custom, retry);
  dispose();
  assert.equal(client.home.held?.label, 'Another action', 'unmount preserves the other approval');
  await retry();
  assert.equal(calls.length, 1);
  assert.equal(client.home.held.label, 'Another action');
});

test('a failed refresh after a successful write is reported separately from saving', async (t) => {
  const { component, calls } = fixture(t, () => response({}), { onchanged: async () => { throw new Error('Refresh failed'); } });
  component.edit(devices[0]);
  await component.saveLabels();
  assert.equal(calls.length, 1);
  assert.equal(component.intent().success, 'labels_saved');
  assert.equal(component.intent().warning, 'refresh');
  assert.equal(component.intent().error, null);
  assert.equal(component.intent().editing, null);
  assert.equal(component.intent().pending, false);
});

test('a removed device cannot be written through a previously opened editor', async (t) => {
  const { component, calls, props } = fixture(t);
  component.edit(devices[0]);
  component.typeLabel('Draft preserved', '');
  props.devices = [];
  client.flushSync();
  await component.saveLabels();
  assert.equal(component.intent().error, 'missing');
  assert.equal(component.intent().editing, devices[0].id);
  assert.equal(component.intent().draft.name, 'Draft preserved');
  assert.deepEqual(calls, []);
});

test('failed hiding preserves its confirmation and cannot displace an existing PIN action', async (t) => {
  const { component, calls, changes } = fixture(t, () => response({}, 403));
  component.requestHide(devices[0].id);
  const other = { custom() {}, label: 'Existing approval' };
  client.home.held = other;
  await component.changeVisibility(devices[0].id, true);
  assert.equal(calls.length, 1);
  assert.equal(component.intent().confirmId, devices[0].id);
  assert.equal(component.intent().awaitingPin, false);
  assert.equal(component.intent().error, 'auth_busy');
  assert.equal(client.home.held.label, 'Existing approval');
  assert.deepEqual(changes, []);
});

test('a device removed while a 403 is in flight does not open a stale approval', async (t) => {
  let finish;
  const { component, props } = fixture(t, () => new Promise((resolve) => { finish = resolve; }));
  component.edit(devices[0]);
  const pending = component.saveLabels();
  props.devices = [];
  client.flushSync();
  finish(response({}, 403));
  await pending;
  assert.equal(component.intent().error, 'missing');
  assert.equal(component.intent().pending, false);
  assert.equal(client.home.held, null);
});

test('direct selection clears filters so the requested editor is actually visible', (t) => {
  const { component, props, calls } = fixture(t);
  component.search('living', 'visible');
  props.selectedId = 'fixture:gateway';
  client.flushSync();
  assert.equal(component.intent().editing, 'fixture:gateway');
  assert.equal(component.intent().visible[0], 'fixture:gateway');
  assert.deepEqual(calls, []);
});
