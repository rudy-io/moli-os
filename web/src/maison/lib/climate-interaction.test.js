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
          import ClimateCard from '/src/maison/ui/ClimateCard.svelte';
          import { hub } from '/src/lib/hub.svelte.js';
          import { flushSync } from 'svelte';
          import { effect_root } from 'svelte/internal/client';
          export { hub, flushSync };
          export function mount(id) {
            let card;
            const dispose = effect_root(() => { card = ClimateCard(null, { id, name: 'Fixture' }); });
            flushSync();
            return { card, dispose };
          }
        `;
      },
      transform(source, id) {
        if (!id.endsWith('/maison/ui/ClimateCard.svelte')) return;
        const { content } = parse(source, { modern: true }).instance;
        return `<script>${source.slice(content.start, content.end)}
          export { nudge, grab, drag, release, flush };
          export function intent() { return { wanted, dragging }; }
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

function fixture(t, response = async () => ({ ok: true, status: 200, json: async () => ({}) })) {
  t.mock.timers.enable({ apis: ['setTimeout', 'setInterval'] });
  const orders = [];
  t.mock.method(globalThis, 'fetch', async (url, options) => {
    if (url === '/api/energy') return { ok: false, status: 404 };
    assert.equal(url, '/api/command');
    orders.push(JSON.parse(options.body));
    return response();
  });
  client.hub.devices = {
    'fixture:climate': {
      id: 'fixture:climate', online: true, points: [{
        key: 'target_temperature', unit: '°C', access: { read: true, write: true },
        // Overlapping numeric ranges make the old 29°C value valid in °F too.
        kind: { type: 'numeric', min: 0, max: 100, step: 1 },
      }], state: { target_temperature: { value: 28 } },
    },
  };
  client.hub.pending = {};
  const { card, dispose } = client.mount('fixture:climate');
  t.after(dispose);
  const changeUnit = () => {
    client.hub.devices['fixture:climate'].points[0].unit = '°F';
    client.flushSync();
  };
  return { card, orders, changeUnit };
}

const pointer = {
  pointerId: 1, clientX: 160, clientY: 80,
  currentTarget: { setPointerCapture() {}, getBoundingClientRect: () => ({ left: 0, top: 0, width: 160, height: 160 }) },
};

test('a thermostat unit change cancels a debounced nudge even when numeric ranges overlap', (t) => {
  const { card, orders, changeUnit } = fixture(t);
  card.nudge(1);
  assert.equal(card.intent().wanted, 29);
  t.mock.timers.tick(899);
  changeUnit();
  t.mock.timers.tick(1);
  assert.deepEqual(orders, []);
  assert.equal(card.intent().wanted, null);
  card.nudge(1);
  t.mock.timers.tick(900);
  assert.deepEqual(orders, [{ point: 'fixture:climate/target_temperature', value: 29 }]);
});

test('a thermostat unit change ends an active drag and ignores its later pointer events', (t) => {
  const { card, orders, changeUnit } = fixture(t);
  card.grab(pointer);
  assert.equal(card.intent().dragging, true);
  changeUnit();
  card.drag({ ...pointer, clientX: 80, clientY: 0 });
  card.release();
  t.mock.timers.tick(250);
  assert.deepEqual(orders, []);
  assert.deepEqual(card.intent(), { wanted: null, dragging: false });
});

test('a thermostat unit change also cancels the delay after releasing the dial', (t) => {
  const { card, orders, changeUnit } = fixture(t);
  card.grab(pointer);
  card.release();
  t.mock.timers.tick(249);
  changeUnit();
  t.mock.timers.tick(1);
  assert.deepEqual(orders, []);
  assert.equal(card.intent().wanted, null);
});

test('a released dial still sends its value after 250 ms while the unit is unchanged', (t) => {
  const { card, orders } = fixture(t);
  card.grab(pointer);
  card.release();
  t.mock.timers.tick(249);
  assert.deepEqual(orders, []);
  t.mock.timers.tick(1);
  assert.deepEqual(orders, [{ point: 'fixture:climate/target_temperature', value: 83 }]);
});

test('a response from the old unit cannot clear a new-unit gesture', async (t) => {
  let finish;
  const response = new Promise((resolve) => { finish = resolve; });
  const { card, orders, changeUnit } = fixture(t, () => response);
  card.nudge(1);
  const pending = card.flush();
  assert.equal(orders.length, 1);
  changeUnit();
  card.nudge(2);
  finish({ ok: false, status: 500, statusText: 'Fixture failure', json: async () => ({}) });
  await pending;
  assert.equal(card.intent().wanted, 30);
});
