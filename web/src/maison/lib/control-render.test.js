import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { createServer } from 'vite';

// Render the actual Svelte controls with a synthetic hub. No API or device calls.
let vite;
let hub;
let home;
let render;
const components = {};
before(async () => {
  // The live hub only reads the current build's script at import time.
  globalThis.document = { querySelector: () => null, documentElement: {} };
  vite = await createServer({ root: fileURLToPath(new URL('../../../', import.meta.url)), server: { middlewareMode: true, watch: null }, logLevel: 'error' });
  ({ render } = await vite.ssrLoadModule('svelte/server'));
  ({ hub } = await vite.ssrLoadModule('/src/lib/hub.svelte.js'));
  home = await vite.ssrLoadModule('/src/maison/lib/home.svelte.js');
  for (const name of ['ClimateCard', 'CoverTile', 'SpeakerCard', 'LightTile']) {
    components[name] = (await vite.ssrLoadModule(`/src/maison/ui/${name}.svelte`)).default;
  }
});
after(async () => { await vite?.close(); delete globalThis.document; });

const point = (key, kind, write = true, semantic = 'other') => ({ key, kind, access: { read: true, write }, semantic });
const number = (min, max, step) => ({ type: 'numeric', min, max, step });
const enumeration = (...values) => ({ type: 'enum', values });
function show(component, points, values, extra = {}) {
  const d = { id: 'fixture:device', native_name: 'Fixture', model: null, label: {}, online: true, points, members: [], state: Object.fromEntries(Object.entries(values).map(([key, value]) => [key, { value }])), ...extra };
  hub.devices = { [d.id]: d };
  return render(components[component], { props: { id: d.id, name: 'Fixture' } }).body;
}
const inputs = (html) => [...html.matchAll(/<input\b[^>]*>/g)].map(([tag]) => tag);
const buttons = (html) => [...html.matchAll(/<button\b[^>]*>/g)].map(([tag]) => tag);

test('thermostat renders its own limits, setpoint-only access and supported modes', () => {
  const html = show('ClimateCard', [point('target_temperature', number(5, 35, 0.1)), point('mode', enumeration('chaud'), false)], { temperature: 20.4, target_temperature: 21.2, mode: 'chaud' });
  assert.match(html, /aria-valuemin="5"/);
  assert.match(html, /aria-valuemax="35"/);
  assert.match(html, /20,4/);
  assert.equal(buttons(html).length, 2, 'only setpoint buttons, no power or mode write');
});

test('master mode remains writable when its setpoint is read-only', () => {
  const html = show('ClimateCard', [point('target_temperature', number(5, 35, 0.5), false), point('mode', enumeration('chaud'))], { temperature: 20, target_temperature: 21, mode: 'chaud', on: true });
  assert.equal(buttons(html).length, 1);
  assert.doesNotMatch(html, /role="slider"/);
  assert.match(html, /aria-pressed="true"/);
});

test('Airzone system has translated mode and fan controls without a fake thermostat', () => {
  const html = show('ClimateCard', [point('mode', enumeration('off', 'heat')), point('fan_speed', enumeration('0', '1', '3'))], { mode: 'heat', fan_speed: '0' }, { model: 'Airzone system' });
  assert.equal(home.deviceKind(hub.devices['fixture:device']), 'climate');
  assert.equal(buttons(html).length, 2);
  assert.match(html, /value="3"/);
  assert.doesNotMatch(html, /class="dial/);
});

test('offline thermostat exposes no active commands', () => {
  const html = show('ClimateCard', [point('target_temperature', number(5, 35, 0.5)), point('on', { type: 'binary' }), point('mode', enumeration('chaud'))], { target_temperature: 21, on: true }, { online: false });
  assert.equal(buttons(html).length, 0);
  assert.doesNotMatch(html, /role="slider"/);
});

test('partial bridge climate and cover retain their family controls', () => {
  const climate = show('ClimateCard', [point('on', { type: 'binary' }), point('mode', enumeration('chaud'))], { on: true, mode: 'chaud' }, { model: 'Home Assistant climate' });
  assert.equal(home.deviceKind(hub.devices['fixture:device']), 'climate');
  assert.equal(buttons(climate).length, 2);
  const cover = show('CoverTile', [point('control', enumeration('close', 'stop'))], {}, { model: 'Home Assistant cover' });
  assert.equal(home.deviceKind(hub.devices['fixture:device']), 'cover');
  assert.equal(buttons(cover).length, 2);
});

test('read-only shutter shows position without command buttons', () => {
  const html = show('CoverTile', [point('percent_control', number(0, 100, 1), false), point('control', enumeration('open', 'close'), false)], { percent_control: 42 });
  assert.match(html, /42/);
  assert.equal(buttons(html).length, 0);
  assert.equal(inputs(html).length, 0);
});

test('shutter only exposes advertised actions and measured position setting', () => {
  const html = show('CoverTile', [point('percent_control', number(0, 100, 1)), point('control', enumeration('close', 'stop'))], { percent_control: 42 });
  assert.equal(buttons(html).length, 2);
  assert.match(inputs(html)[0], /min="0"/);
  assert.doesNotMatch(inputs(html)[0], /disabled/);
});

test('cycle-only opener has one distinct impulse, without invented direction', () => {
  const html = show('CoverTile', [point('control', enumeration('cycle'), true, 'control')], {}, { model: 'Overkiz Gate' });
  assert.equal(home.deviceKind(hub.devices['fixture:device']), 'cover');
  assert.equal(buttons(html).length, 1);
  assert.match(html, /Impulsion/);
  assert.equal(inputs(html).length, 0);
});

test('media partial write access does not enable playback or mute', () => {
  const html = show('SpeakerCard', [point('volume', number(0, 100, 1)), point('playing', { type: 'binary' }, false), point('mute', { type: 'binary' }, false)], { playing: true, mute: false, volume: 34 });
  assert.ok(buttons(html).every((tag) => /disabled/.test(tag)));
  assert.doesNotMatch(inputs(html)[0], /disabled/);
});

test('read-only light still displays its on state without a toggle', () => {
  const html = show('LightTile', [point('on', { type: 'binary' }, false, 'on_off')], { on: true }, { model: 'Home Assistant light' });
  assert.equal(home.deviceKind(hub.devices['fixture:device']), 'light');
  assert.match(html, /aria-pressed="true"/);
  assert.ok(buttons(html).every((tag) => /disabled/.test(tag)));
  assert.equal(home.toggle('fixture:device', 'Fixture', 'on'), undefined);
});
