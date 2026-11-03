import { test, before, beforeEach, after } from 'node:test';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { createServer } from 'vite';

let vite;
let hub;
let render;
let Point;
const storage = Object.getOwnPropertyDescriptor(globalThis, 'localStorage');
before(async () => {
  delete globalThis.localStorage;
  globalThis.document = { querySelector: () => null, documentElement: {} };
  vite = await createServer({ root: fileURLToPath(new URL('../../', import.meta.url)), server: { middlewareMode: true, watch: null }, logLevel: 'error' });
  ({ render } = await vite.ssrLoadModule('svelte/server'));
  ({ hub } = await vite.ssrLoadModule('/src/lib/hub.svelte.js'));
  Point = (await vite.ssrLoadModule('/src/components/Point.svelte')).default;
});
beforeEach(() => { hub.pending = {}; });
after(async () => {
  await vite?.close();
  delete globalThis.document;
  if (storage) Object.defineProperty(globalThis, 'localStorage', storage);
});

function show(kind, write = true, online = true, value = 'Zone A') {
  const point = { key: 'parameter', label: 'Parameter', semantic: 'other', kind, access: { read: true, write } };
  const device = { id: 'fixture:zone', online, state: { parameter: { value } } };
  return render(Point, { props: { device, point } }).body;
}

test('writable text has an explicit save form with an escaped current value', () => {
  const html = show({ type: 'text' }, true, true, '<script>synthetic</script>');
  assert.match(html, /<form\b/);
  assert.match(html, /<input\b[^>]*type="text"/);
  assert.match(html, /type="submit"/);
  assert.match(html, /Enregistrer/);
  assert.doesNotMatch(html, /<script>synthetic/);
});

test('read-only and offline points render values without command controls', () => {
  for (const kind of [{ type: 'text' }, { type: 'binary' }, { type: 'numeric' }, { type: 'enum', values: ['0', '1'] }]) {
    for (const [write, online] of [[false, true], [true, false], [true, null]]) {
      const html = show(kind, write, online);
      assert.doesNotMatch(html, /<input\b|<select\b|<form\b|role="switch"/);
      assert.match(html, /class="value/);
    }
  }
});

test('pending text disables both editing and saving', () => {
  hub.pending = { 'fixture:zone/parameter': 'Zone B' };
  const html = show({ type: 'text' });
  assert.match(html, /<input\b[^>]*disabled/);
  assert.match(html, /<button\b[^>]*type="submit"[^>]*disabled/);
});
