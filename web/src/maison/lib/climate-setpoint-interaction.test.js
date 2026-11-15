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
  // Compile the real component's script and effects with the client runtime.
  // Expose its handlers only in this test bundle; SSR cannot exercise gestures.
  const result = await build({
    configFile: false,
    root: fileURLToPath(new URL('../../../', import.meta.url)),
    logLevel: 'silent',
    plugins: [{
      name: 'climate-interaction-fixture', enforce: 'pre',
      resolveId(id) { if (id === 'virtual:climate-fixture') return '\0' + id; },
      load(id) {
        if (id !== '\0virtual:climate-fixture') return;
        return `
          import ClimateSetpoint from '/src/maison/ui/ClimateSetpoint.svelte';
          import { hub } from '/src/lib/hub.svelte.js';
          import { flushSync } from 'svelte';
          import { effect_root } from 'svelte/internal/client';
          export { hub, flushSync };
          export function mount(id) {
            let card;
            const dispose = effect_root(() => { card = ClimateSetpoint(null, { id, pointKey: 'heat_setpoint', label: 'Heat', name: 'Fixture' }); });
            flushSync();
            return { card, dispose };
          }
        `;
      },
      transform(source, id) {
        if (!id.endsWith('/maison/ui/ClimateSetpoint.svelte')) return;
        const { content } = parse(source, { modern: true }).instance;
        return `<script>${source.slice(content.start, content.end)}
          export { nudge, edit, flush };
          export function intent() { return { wanted }; }
        </script>`;
      },
    }, svelte()],
    build: {
      write: false, minify: false,
      lib: { entry: 'virtual:climate-fixture', formats: ['es'] },
      rolldownOptions: { input: 'virtual:climate-fixture' },
    },
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

function fixture(t) {
  t.mock.timers.enable({ apis: ['setTimeout', 'setInterval'] });
  const orders = [];
  t.mock.method(globalThis, 'fetch', async (url, options) => {
    orders.push(JSON.parse(options.body));
    return { ok: true, status: 200, json: async () => ({}) };
  });
  client.hub.connected = true;
  client.hub.pending = {};
  client.hub.devices = { fixture: {
    id: 'fixture', online: true,
    points: ['heat_setpoint', 'cool_setpoint'].map(key => ({ key, unit: '°C', access: { write: true }, kind: { type: 'numeric', min: 10, max: 30, step: 0.5 } })),
    state: { heat_setpoint: { value: 20 }, cool_setpoint: { value: 25 }, mode: { value: 'heat' } },
  } };
  const { card, dispose } = client.mount('fixture');
  t.after(dispose);
  return { card, orders, d: client.hub.devices.fixture };
}
test('queued explicit heat commands keep their key across mode changes and respect bounds', t => {
  const { card, orders, d } = fixture(t);
  card.nudge(0.5);
  d.state.mode.value = 'cool';
  client.flushSync();
  t.mock.timers.tick(900);
  assert.deepEqual(orders, [{ point: 'fixture/heat_setpoint', value: 20.5 }]);
  card.edit(100);
  t.mock.timers.tick(900);
  assert.equal(orders[1].value, 30);
});
test('unknown values cannot be nudged and queued read-only/offline edits cannot send', t => {
  const { card, orders, d } = fixture(t);
  d.state.heat_setpoint.value = null;
  client.flushSync();
  card.nudge(0.5);
  t.mock.timers.tick(900);
  assert.deepEqual(orders, []);
  card.edit(22.2);
  assert.equal(card.intent().wanted, 22);
  d.points[0].access.write = false;
  client.flushSync();
  t.mock.timers.tick(900);
  assert.deepEqual(orders, []);
  d.points[0].access.write = true;
  card.edit(23);
  d.online = false;
  client.flushSync();
  t.mock.timers.tick(900);
  assert.deepEqual(orders, []);
});
test('an explicit setpoint unit change invalidates its queued edit', t => {
  const { card, orders, d } = fixture(t);
  card.nudge(0.5);
  d.points[0].unit = '°F';
  client.flushSync();
  t.mock.timers.tick(900);
  assert.deepEqual(orders, []);
  assert.equal(card.intent().wanted, null);
});
test('an old-unit response cannot clear a newer explicit edit', async t => {
  const { card, d } = fixture(t);
  let finish;
  t.mock.method(globalThis, 'fetch', () => new Promise(resolve => { finish = resolve; }));
  card.edit(21);
  const pending = card.flush();
  d.points[0].unit = '°F';
  client.flushSync();
  card.edit(23);
  finish({ ok: false, status: 500, statusText: 'Fixture failure', json: async () => ({}) });
  await pending;
  assert.equal(card.intent().wanted, 23);
});
