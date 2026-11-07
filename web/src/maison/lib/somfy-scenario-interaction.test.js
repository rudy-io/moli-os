import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { parse } from 'svelte/compiler';
import { build } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

let client;
const globals = new Map(['document', 'localStorage'].map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)]));
before(async () => {
  delete globalThis.localStorage;
  globalThis.document = { querySelector: () => null, documentElement: {} };
  const result = await build({
    configFile: false, root: fileURLToPath(new URL('../../../', import.meta.url)), logLevel: 'silent',
    plugins: [{
      name: 'scenario-consent-fixture', enforce: 'pre',
      resolveId(id) { if (id === 'virtual:scenario-fixture') return '\0' + id; },
      load(id) {
        if (id !== '\0virtual:scenario-fixture') return;
        return `
          import Control from '/src/maison/ui/SomfyScenarioControl.svelte';
          import { hub } from '/src/maison/lib/home.svelte.js';
          import { flushSync } from 'svelte';
          import { effect_root, mutable_source, get, set } from 'svelte/internal/client';
          export { hub, flushSync };
          export function mount(observation, permission) {
            let card;
            const current = mutable_source(observation);
            const pending = mutable_source(false);
            const dispose = effect_root(() => { card = Control(null, {
              get observation() { return get(current); }, get pending() { return get(pending); },
              definition: { id: 'demo', label: 'Synthetic scenario' }, generation: 'generation', permission,
            }); });
            flushSync();
            return { card, dispose, setObservation(next) { set(current, next); flushSync(); }, setPending(next) { set(pending, next); flushSync(); } };
          }
        `;
      },
      transform(source, id) {
        if (!id.endsWith('/maison/ui/SomfyScenarioControl.svelte')) return;
        const { content } = parse(source, { modern: true }).instance;
        return `<script>${source.slice(content.start, content.end)}
          export { configure, save, launch };
          export function enterPin(value) { pin = value; consent = true; }
          export function snapshot() { return { opened, pin, consent, intent, outcome, error }; }
        </script>`;
      },
    }, svelte()],
    build: { write: false, minify: false, lib: { entry: 'virtual:scenario-fixture', formats: ['es'] }, rolldownOptions: { input: 'virtual:scenario-fixture' } },
  });
  const chunk = (Array.isArray(result) ? result[0] : result).output.find(entry => entry.type === 'chunk' && entry.isEntry);
  client = await import(`data:text/javascript;base64,${Buffer.from(chunk.code).toString('base64')}`);
});
after(() => {
  for (const [key, descriptor] of globals) {
    if (descriptor) Object.defineProperty(globalThis, key, descriptor);
    else delete globalThis[key];
  }
});
const observation = () => ({ revision: 'revision', freshness: { current: true, collections: { scenarios: true } },
  execution: { read_only: false, scenarios: [{ id: 'demo', permitted: true, executable: true, point: 'fixture:scenario/run', value: 'revision-token' }] } });
function fixture(t, permission = async () => '') {
  client.hub.connected = true;
  client.hub.devices = { 'fixture:scenario': { online: true, points: [{ key: 'run', access: { write: true }, kind: { type: 'enum', values: ['revision-token'] } }] } };
  const mounted = client.mount(observation(), permission);
  t.after(mounted.dispose);
  return mounted;
}

test('a stale observation, pending purge or lost connection closes a revoke form and discards its PIN', async t => {
  for (const change of ['stale', 'pending', 'offline', 'purged', 'read_only']) {
    const mounted = fixture(t);
    await mounted.card.configure();
    mounted.card.enterPin('123456');
    client.flushSync();
    assert.equal(mounted.card.snapshot().opened, true);
    if (change === 'pending') mounted.setPending(true);
    else if (change === 'offline') { client.hub.connected = false; client.flushSync(); }
    else {
      const next = observation();
      if (change === 'stale') next.freshness.current = false;
      if (change === 'purged') next.execution = null;
      if (change === 'read_only') next.execution.read_only = true;
      mounted.setObservation(next);
    }
    const state = mounted.card.snapshot();
    assert.equal(state.opened, false, change);
    assert.equal(state.pin, '', change);
    assert.equal(state.consent, false, change);
    assert.equal(state.intent, null, change);
    mounted.dispose();
  }
});

test('late permission feedback cannot revive a discarded consent context', async t => {
  let finish, calls = 0;
  const mounted = fixture(t, () => { calls++; return new Promise(resolve => { finish = resolve; }); });
  await mounted.card.configure();
  mounted.card.enterPin('123456');
  const pending = mounted.card.save({ preventDefault() {} });
  mounted.setObservation({ ...observation(), execution: null });
  finish('invalid_pin');
  await pending;
  assert.equal(calls, 1);
  assert.deepEqual(mounted.card.snapshot(), { opened: false, pin: '', consent: false, intent: null, outcome: '', error: '' });
});

test('leaving a scenario clears an unsubmitted PIN and ignores delayed permission feedback', async t => {
  const open = fixture(t);
  await open.card.configure();
  open.card.enterPin('123456');
  open.dispose();
  assert.equal(open.card.snapshot().pin, '');
  let finish;
  const pending = fixture(t, () => new Promise(resolve => { finish = resolve; }));
  await pending.card.configure();
  pending.card.enterPin('123456');
  const result = pending.card.save({ preventDefault() {} });
  pending.dispose();
  finish('invalid_pin');
  await result;
  assert.equal(pending.card.snapshot().error, '');
});

test('scenario launch still uses the guarded command and refuses purged, pending and read-only views', async t => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const calls = [];
  t.mock.method(globalThis, 'fetch', async (url, options) => {
    calls.push({ url, body: JSON.parse(options.body) });
    return Response.json({});
  });
  const mounted = fixture(t);
  await mounted.card.launch();
  assert.deepEqual(calls, [{ url: '/api/command', body: { point: 'fixture:scenario/run', value: 'revision-token' } }]);
  for (const change of ['pending', 'purged', 'read_only']) {
    mounted.setPending(change === 'pending');
    const next = observation();
    if (change === 'purged') next.execution = null;
    if (change === 'read_only') next.execution.read_only = true;
    mounted.setObservation(next);
    await mounted.card.launch();
  }
  assert.equal(calls.length, 1);
});
