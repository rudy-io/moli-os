import { test, before, beforeEach, after } from 'node:test';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { createServer } from 'vite';

let vite;
let render;
let hub;
let energy;
let Flow;
const storage = Object.getOwnPropertyDescriptor(globalThis, 'localStorage');
before(async () => {
  delete globalThis.localStorage;
  globalThis.document = { querySelector: () => null, documentElement: {} };
  vite = await createServer({ root: fileURLToPath(new URL('../../../', import.meta.url)), server: { middlewareMode: true, watch: null }, logLevel: 'error' });
  ({ render } = await vite.ssrLoadModule('svelte/server'));
  ({ hub } = await vite.ssrLoadModule('/src/maison/lib/home.svelte.js'));
  ({ energy } = await vite.ssrLoadModule('/src/maison/lib/energy-live.svelte.js'));
  Flow = (await vite.ssrLoadModule('/src/maison/ui/EnergyFlow.svelte')).default;
});
beforeEach(() => {
  hub.devices = {};
  energy.summary = null;
});
after(async () => {
  await vite?.close();
  delete globalThis.document;
  if (storage) Object.defineProperty(globalThis, 'localStorage', storage);
});

const at = (values) => {
  for (const [point, value] of Object.entries(values)) {
    const [id, key] = point.split('/');
    hub.devices[id] ??= { id, state: {} };
    hub.devices[id].state[key] = { value };
  }
};
const show = (props = {}) => render(Flow, { props }).body;
// The figure's summary: what the house uses, from the grid, from the panels.
const summary = (html) => html.match(/aria-label="([^"]*consomme[^"]*)"/)?.[1] ?? null;

test('the house is what was bought plus what the panels made, less what went out', () => {
  energy.summary = {
    meters: [
      // A P1's day and night indexes share one power point: counted once.
      { id: 'day', role: 'grid', power: 'p1/import' },
      { id: 'night', role: 'grid', power: 'p1/import' },
      { id: 'out', role: 'export', power: 'p1/export' },
      { id: 'pv', role: 'solar', power: 'inv/power' },
    ],
    today: { meters: {} },
  };
  at({ 'p1/import': 300, 'p1/export': 200, 'inv/power': 1500 });
  const s = summary(show());
  assert.ok(s, 'the card is drawn');
  assert.match(s, /consomme 1,6 kW/);
  assert.match(s, /300 W du réseau/);
});

test('a single clamp below zero is power sent out', () => {
  energy.summary = {
    meters: [
      { id: 'clamp', role: 'grid', power: 'em/power' },
      { id: 'pv', role: 'solar', power: 'inv/power' },
    ],
    today: { meters: {} },
  };
  at({ 'em/power': -400, 'inv/power': 1000 });
  const html = show();
  // Nothing bought, 400 W out: the house uses 600 W.
  assert.match(summary(html), /consomme 600 W : 0 W du réseau/);
  assert.match(html, /← 400 W/);
});

test('on the home page, a house with only the grid shows no flow', () => {
  energy.summary = { meters: [{ id: 'day', role: 'grid', power: 'p1/import' }], today: { meters: {} } };
  at({ 'p1/import': 500 });
  assert.equal(summary(show({ home: true })), null, 'home page, no panels: no card');
  assert.ok(summary(show()), 'the energy page still has it');
});
