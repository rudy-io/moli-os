import { test, before, beforeEach, after } from 'node:test';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { createServer } from 'vite';

let vite;
let render;
let hub;
let Chart;
const storage = Object.getOwnPropertyDescriptor(globalThis, 'localStorage');
before(async () => {
  delete globalThis.localStorage;
  globalThis.document = { querySelector: () => null, documentElement: {} };
  vite = await createServer({ root: fileURLToPath(new URL('../../../', import.meta.url)), server: { middlewareMode: true, watch: null }, logLevel: 'error' });
  ({ render } = await vite.ssrLoadModule('svelte/server'));
  ({ hub } = await vite.ssrLoadModule('/src/maison/lib/home.svelte.js'));
  Chart = (await vite.ssrLoadModule('/src/maison/ui/HistoryChart.svelte')).default;
});
beforeEach(() => {
  const point = (key, unit) => ({ key, label: key, unit, kind: { type: 'numeric' }, access: { read: true } });
  hub.devices = {
    meter: { id: 'meter', points: [point('power', 'W'), point('current', 'A')], state: { power: { value: 1234.5 }, current: { value: 2.25 } } },
    plug: { id: 'plug', points: [point('power', 'W')], state: { power: { value: 40 } } },
  };
});
after(async () => {
  await vite?.close();
  delete globalThis.document;
  if (storage) Object.defineProperty(globalThis, 'localStorage', storage);
});

const show = (points, minutes = 10) => render(Chart, { props: { title: 'Courbes', minutes, points } }).body;
const charts = (html) => (html.match(/<svg\b[^>]*role="img"/g) ?? []).length;

test('one chart per unit, values now in the language of the house', () => {
  const html = show(['meter/power', 'plug/power', 'meter/current']);
  assert.equal(charts(html), 2, 'W together, A apart');
  // French numbers: a comma, not the browser's own way.
  assert.match(html, /1\s?234,5 W/);
  assert.match(html, /2,25 A/);
  assert.doesNotMatch(html, /#[0-9a-f]{6}/i, 'theme colours, not fixed ones');
});

test('a badly written or unknown point is left aside, and said', () => {
  const html = show(['meter/power', 'nothing-here', 'ghost/power', 'meter/']);
  assert.equal(charts(html), 1);
  assert.match(html, /Écartés[^<]*nothing-here, ghost\/power, meter\//);
});

test('eight points at most', () => {
  // Nine points the house has: only 8 drawn.
  hub.devices.meter.points.push(...Array.from({ length: 9 }, (_, i) => ({ key: `p${i}`, label: `p${i}`, unit: 'W' })));
  const html = show(Array.from({ length: 9 }, (_, i) => `meter/p${i}`));
  assert.equal((html.match(/aria-pressed/g) ?? []).length, 8);
  assert.match(html, /Écartés[^<]*meter\/p8/);
});
