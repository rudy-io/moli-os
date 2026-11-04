import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { parse } from 'svelte/compiler';
import { build } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

let client;
const globals = new Map(['document', 'localStorage'].map((key) => [key, Object.getOwnPropertyDescriptor(globalThis, key)]));
before(async () => {
  delete globalThis.localStorage;
  globalThis.document = { querySelector: () => null, documentElement: {} };
  // Run the real handlers and effects. Only the test bundle exposes them.
  const result = await build({
    configFile: false, root: fileURLToPath(new URL('../../../', import.meta.url)), logLevel: 'silent',
    plugins: [{
      name: 'cover-interaction-fixture', enforce: 'pre',
      resolveId(id) { if (id === 'virtual:cover-fixture') return '\0' + id; },
      load(id) {
        if (id !== '\0virtual:cover-fixture') return;
        return `
          import CoverTile from '/src/maison/ui/CoverTile.svelte';
          import { hub } from '/src/lib/hub.svelte.js';
          import { home } from '/src/maison/lib/home.svelte.js';
          import { flushSync } from 'svelte';
          import { effect_root } from 'svelte/internal/client';
          export { hub, home, flushSync };
          export function mount(id) {
            let card;
            const dispose = effect_root(() => { card = CoverTile(null, { id }); });
            flushSync();
            return { card, dispose };
          }
        `;
      },
      transform(source, id) {
        if (!id.endsWith('/maison/ui/CoverTile.svelte')) return;
        const { content } = parse(source, { modern: true }).instance;
        return `<script>${source.slice(content.start, content.end)}
          export { sendOrder, setPosition };
          export function result() { return typeof feedback === 'undefined' ? '' : feedback; }
        </script>`;
      },
    }, svelte()],
    build: { write: false, minify: false, lib: { entry: 'virtual:cover-fixture', formats: ['es'] }, rolldownOptions: { input: 'virtual:cover-fixture' } },
  });
  const chunk = (Array.isArray(result) ? result[0] : result).output.find((entry) => entry.type === 'chunk' && entry.isEntry);
  client = await import(`data:text/javascript;base64,${Buffer.from(chunk.code).toString('base64')}`);
});
after(() => {
  for (const [key, descriptor] of globals) {
    if (descriptor) Object.defineProperty(globalThis, key, descriptor);
    else delete globalThis[key];
  }
});

const reply = (status = 200, body = {}) => ({ status, ok: status < 400, statusText: 'Fixture response', json: async () => body });
function fixture(t, response = async () => reply()) {
  t.mock.timers.enable({ apis: ['setTimeout', 'setInterval'] });
  const orders = [];
  t.mock.method(globalThis, 'fetch', async (url, options) => {
    assert.equal(url, '/api/command');
    orders.push(JSON.parse(options.body));
    return response();
  });
  client.hub.devices = {
    'fixture:cover': {
      id: 'fixture:cover', native_name: 'Fixture', online: true, label: {},
      points: [
        { key: 'motor', access: { read: false, write: true }, kind: { type: 'enum', values: ['OPEN', 'STOP', 'CLOSE'] } },
        { key: 'opening', access: { read: true, write: true }, semantic: 'position', kind: { type: 'numeric', min: 0, max: 100, step: 10 } },
      ], state: { opening: { value: 40 } },
    },
  };
  client.hub.pending = {};
  client.home.held = null;
  const mounted = client.mount('fixture:cover');
  t.after(mounted.dispose);
  return { ...mounted, orders, device: client.hub.devices['fixture:cover'] };
}

test('a repeated pending order is suppressed while Stop remains available and owns the feedback', async (t) => {
  const finish = [];
  const { card, orders } = fixture(t, () => new Promise((resolve) => finish.push(resolve)));
  const first = card.sendOrder('open', 'commun.volet.ouvrir');
  card.sendOrder('open', 'commun.volet.ouvrir');
  assert.equal(orders.length, 1);
  assert.equal(card.result(), 'sending');
  const stop = card.sendOrder('stop', 'commun.volet.stop');
  assert.deepEqual(orders, [{ point: 'fixture:cover/motor', value: 'OPEN' }, { point: 'fixture:cover/motor', value: 'STOP' }]);
  finish[0](reply());
  await first;
  assert.equal(card.result(), 'sending', 'an older response cannot replace the Stop result');
  finish[1](reply());
  await stop;
  assert.equal(card.result(), 'sent', 'acceptance is not physical completion');
});

test('revoked and offline capabilities cannot send a cover command or position', async (t) => {
  const { card, orders, device } = fixture(t);
  for (const point of device.points) point.access.write = false;
  client.flushSync();
  await card.sendOrder('open', 'commun.volet.ouvrir');
  await card.setPosition({ currentTarget: { value: '80' } });
  for (const point of device.points) point.access.write = true;
  device.online = false;
  client.flushSync();
  await card.sendOrder('close', 'commun.volet.fermer');
  await card.setPosition({ currentTarget: { value: '80' } });
  assert.deepEqual(orders, []);
});

test('guarded and refused cover commands stay distinct without retry or automatic approval', async (t) => {
  let answer = reply(202, { pending: 42, reason: 'Fixture approval' });
  const { card, orders } = fixture(t, async () => answer);
  await card.sendOrder('open', 'commun.volet.ouvrir');
  assert.equal(card.result(), 'held');
  assert.equal(client.home.held.id, 42);
  answer = reply(403, { error: 'Fixture refusal' });
  await card.sendOrder('close', 'commun.volet.fermer');
  assert.equal(card.result(), 'error');
  t.mock.timers.tick(10000);
  assert.equal(orders.length, 2);
});

test('position feedback uses the selected capability and refuses missing feedback position', async (t) => {
  const { card, orders, device } = fixture(t);
  await card.setPosition({ currentTarget: { value: '67' } });
  assert.deepEqual(orders, [{ point: 'fixture:cover/opening', value: 70 }]);
  assert.equal(card.result(), 'sent');
  device.state.opening.value = null;
  client.flushSync();
  await card.setPosition({ currentTarget: { value: '80' } });
  assert.equal(orders.length, 1);
});

test('leaving a cover ignores its delayed command feedback', async (t) => {
  let finish;
  const { card, dispose } = fixture(t, () => new Promise((resolve) => { finish = resolve; }));
  const pending = card.sendOrder('open', 'commun.volet.ouvrir');
  dispose();
  finish(reply());
  await pending;
  assert.equal(card.result(), 'sending');
});
