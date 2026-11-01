import { test, before, beforeEach, after } from 'node:test';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { readFileSync } from 'node:fs';
import { createServer } from 'vite';

// Render the actual Svelte controls with a synthetic hub. No API or device calls.
let vite;
let hub;
let home;
let energy;
let render;
const localStorageDescriptor = Object.getOwnPropertyDescriptor(globalThis, 'localStorage');
const components = {};
before(async () => {
  // Keep SSR independent from Node's optional browser-storage implementation.
  delete globalThis.localStorage;
  // The live hub only reads the current build's script at import time.
  globalThis.document = { querySelector: () => null, documentElement: {} };
  vite = await createServer({ root: fileURLToPath(new URL('../../../', import.meta.url)), server: { middlewareMode: true, watch: null }, logLevel: 'error' });
  ({ render } = await vite.ssrLoadModule('svelte/server'));
  ({ hub } = await vite.ssrLoadModule('/src/lib/hub.svelte.js'));
  home = await vite.ssrLoadModule('/src/maison/lib/home.svelte.js');
  ({ energy } = await vite.ssrLoadModule('/src/maison/lib/energy-live.svelte.js'));
  for (const name of ['ClimateCard', 'CoverTile', 'SpeakerCard', 'LightTile', 'RoomCard']) {
    components[name] = (await vite.ssrLoadModule(`/src/maison/ui/${name}.svelte`)).default;
  }
});
beforeEach(() => { home.home.config = null; energy.summary = null; });
after(async () => {
  await vite?.close();
  delete globalThis.document;
  if (localStorageDescriptor) Object.defineProperty(globalThis, 'localStorage', localStorageDescriptor);
});

const point = (key, kind, write = true, semantic = 'other') => ({ key, kind, access: { read: true, write }, semantic });
const number = (min, max, step) => ({ type: 'numeric', min, max, step });
const enumeration = (...values) => ({ type: 'enum', values });
function show(component, points, values, extra = {}) {
  const d = { id: 'fixture:device', native_name: 'Fixture', model: null, label: {}, online: true, points, members: [], state: Object.fromEntries(Object.entries(values).map(([key, value]) => [key, { value }])), ...extra };
  hub.devices = { [d.id]: d };
  return render(components[component], { props: { id: d.id, name: 'Fixture' } }).body;
}
function showRoom(entries) {
  hub.devices = Object.fromEntries(entries.map(({ id, points, values, ...extra }) => [id, {
    id, native_name: id, model: null, label: { room: 'Workshop' }, online: true, points, members: [],
    state: Object.fromEntries(Object.entries(values).map(([key, value]) => [key, { value }])), ...extra,
  }]));
  return render(components.RoomCard, { props: { room: home.roomList()[0] } }).body;
}
const inputs = (html) => [...html.matchAll(/<input\b[^>]*>/g)].map(([tag]) => tag);
const buttons = (html) => [...html.matchAll(/<button\b[^>]*>/g)].map(([tag]) => tag);

test('villa Sonos and Daikin fixtures retain their working numeric controls', () => {
  const fixture = (name) => JSON.parse(readFileSync(new URL(`../../../../demo/villa/devices/${name}.json`, import.meta.url), 'utf8')).devices;
  const speakers = fixture('sonos');
  const climates = fixture('daikin');
  assert.ok(speakers.length > 0 && climates.length > 0);
  for (const d of speakers) {
    const html = show('SpeakerCard', d.points, d.state, { model: d.model });
    const slider = inputs(html).find((tag) => /aria-label="Volume"/.test(tag));
    assert.ok(slider);
    assert.match(slider, /min="0"/);
    assert.match(slider, /max="100"/);
  }
  for (const d of climates) {
    const html = show('ClimateCard', d.points, d.state, { model: d.model });
    assert.match(html, /role="slider"/);
    assert.match(html, /aria-valuemin="16"/);
    assert.match(html, /aria-valuemax="30"/);
  }
});

test('thermostat renders its own limits, setpoint-only access and supported modes', () => {
  const html = show('ClimateCard', [point('target_temperature', number(5, 35, 0.1)), point('mode', enumeration('chaud'), false)], { temperature: 20.4, target_temperature: 21.2, mode: 'chaud' });
  assert.match(html, /aria-valuemin="5"/);
  assert.match(html, /aria-valuemax="35"/);
  assert.match(html, /20,4/);
  assert.equal(buttons(html).length, 2, 'only setpoint buttons, no power or mode write');
});

test('legacy thermostat without declared bounds retains its writable dial', () => {
  const html = show('ClimateCard', [point('target_temperature', { type: 'numeric' })], { target_temperature: 21 });
  assert.match(html, /role="slider"/);
  assert.match(html, /aria-valuemin="16"/);
  assert.match(html, /aria-valuemax="30"/);
  assert.equal(buttons(html).length, 2);
});

test('partial numeric controls render their first and last selectable steps', () => {
  const climate = show('ClimateCard', [point('target_temperature', number(undefined, 30, 0.3))], { target_temperature: 19.8 });
  assert.match(climate, /aria-valuemin="16.2"/);
  assert.match(climate, /aria-valuemax="30"/);
  assert.match(climate, /aria-valuenow="19.8"/);
  const speaker = show('SpeakerCard', [point('volume', number(undefined, 30.1, 0.3))], { volume: 19.8 });
  const [slider] = inputs(speaker);
  assert.match(slider, /min="0"/);
  assert.match(slider, /max="30"/);
  assert.match(slider, /step="0.3"/);
});

test('fallback thermostat limits do not grant read-only or offline access', () => {
  const readonly = show('ClimateCard', [point('target_temperature', { type: 'numeric' }, false)], { target_temperature: 21 });
  assert.doesNotMatch(readonly, /role="slider"/);
  assert.equal(buttons(readonly).length, 0);
  const offline = show('ClimateCard', [point('target_temperature', { type: 'numeric' })], { target_temperature: 21 }, { online: false });
  assert.doesNotMatch(offline, /role="slider"/);
  assert.equal(buttons(offline).length, 0);
});

test('the legacy Celsius range is not applied to an explicitly different unit', () => {
  const target = { ...point('target_temperature', { type: 'numeric' }), unit: '°F' };
  const unbounded = show('ClimateCard', [target], { target_temperature: 70 });
  assert.doesNotMatch(unbounded, /role="slider"/);
  const bounded = show('ClimateCard', [{ ...target, kind: number(60, 86, 1) }], { target_temperature: 70 });
  assert.match(bounded, /aria-valuemin="60"/);
  assert.match(bounded, /aria-valuemax="86"/);
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

test('partial RTS shutter retains close and stop without inventing position or open', () => {
  const points = [point('control', enumeration('close', 'stop'))];
  const html = show('CoverTile', points, {}, { model: 'Overkiz RollerShutter' });
  assert.equal(home.deviceKind(hub.devices['fixture:device']), 'cover');
  assert.equal(buttons(html).length, 2);
  assert.equal(inputs(html).length, 0);
  const offline = show('CoverTile', points, {}, { model: 'Overkiz RollerShutter', online: false });
  assert.equal(buttons(offline).length, 0);
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
  assert.equal(buttons(html).length, 0);
  assert.doesNotMatch(inputs(html)[0], /disabled/);
});

test('speaker volume and announcement sliders respect their distinct advertised ranges', () => {
  const html = show('SpeakerCard', [point('volume', number(5, 80, 2)), point('announce_volume', number(10, 70, 10))], { volume: 25, announce_volume: 30 });
  const sliders = inputs(html);
  assert.equal(sliders.length, 2);
  assert.match(sliders[0], /min="5"/);
  assert.match(sliders[0], /max="79"/); // Last selectable step inside the declared max of 80.
  assert.match(sliders[0], /step="2"/);
  assert.match(sliders[1], /min="10"/);
  assert.match(sliders[1], /max="70"/);
  assert.match(sliders[1], /step="10"/);
});

test('legacy and announce-only speakers keep their ranges without inventing playback', () => {
  const legacy = show('SpeakerCard', [point('volume', { type: 'numeric' })], { volume: 30 });
  assert.match(inputs(legacy)[0], /min="0"/);
  assert.match(inputs(legacy)[0], /max="100"/);
  const announce = show('SpeakerCard', [point('announce_volume', { type: 'numeric' })], { announce_volume: 35 });
  assert.equal(inputs(announce).length, 1);
  assert.match(inputs(announce)[0], /step="5"/);
  assert.equal(buttons(announce).length, 0);
  assert.doesNotMatch(announce, /Arrêt|En pause|Rien/);
});

test('offline speaker offers no playback, mute or level commands', () => {
  const html = show('SpeakerCard', [point('playing', { type: 'binary' }), point('mute', { type: 'binary' }), point('volume', number(0, 100, 1)), point('announce_volume', number(0, 100, 5))], { playing: true, mute: false, volume: 25, announce_volume: 30 }, { online: false });
  assert.equal(buttons(html).length, 0);
  assert.equal(inputs(html).length, 0);
});

test('read-only light still displays its on state without a toggle', () => {
  const html = show('LightTile', [point('on', { type: 'binary' }, false, 'on_off')], { on: true }, { model: 'Home Assistant light' });
  assert.equal(home.deviceKind(hub.devices['fixture:device']), 'light');
  assert.match(html, /aria-pressed="true"/);
  assert.ok(buttons(html).every((tag) => /disabled/.test(tag)));
  assert.equal(home.toggle('fixture:device', 'Fixture', 'on'), undefined);
});

test('room read-only light and switch retain readings with disabled individual and bulk controls', () => {
  const html = showRoom([
    { id: 'fixture:light', model: 'Home Assistant light', points: [point('on', { type: 'binary' }, false, 'on_off')], values: { on: true } },
    { id: 'fixture:switch', model: 'Home Assistant switch', points: [point('on', { type: 'binary' }, false, 'on_off')], values: { on: false } },
  ]);
  const commands = buttons(html);
  assert.equal(commands.length, 3, 'room switch and two individual controls');
  assert.ok(commands.every((tag) => /disabled/.test(tag)), 'read-only controls must not appear actionable');
  assert.match(html, /Allumée/);
  assert.match(html, /Éteinte/);
});

test('room bulk light state follows writable lights, not an observed light', () => {
  const html = showRoom([
    { id: 'fixture:observed', model: 'Home Assistant light', points: [point('on', { type: 'binary' }, false, 'on_off')], values: { on: true } },
    { id: 'fixture:writable', model: 'Home Assistant light', points: [point('on', { type: 'binary' }, true, 'on_off')], values: { on: false } },
  ]);
  const commands = buttons(html);
  assert.doesNotMatch(commands[0], /disabled/);
  assert.match(commands[0], /aria-pressed="false"/);
  assert.match(commands[1], /disabled/);
  assert.doesNotMatch(commands[2], /disabled/);
});

test('room offline light and explicit television power cannot offer commands', () => {
  const html = showRoom([
    { id: 'fixture:light', model: 'Home Assistant light', online: false, points: [point('on', { type: 'binary' }, true, 'on_off')], values: { on: true } },
    { id: 'fixture:tv', points: [point('key', { type: 'text' }), point('power', { type: 'binary' }, false)], values: { power: true } },
  ]);
  assert.equal(buttons(html).length, 3);
  assert.ok(buttons(html).every((tag) => /disabled/.test(tag)));
});

test('room shared Airzone control shows its translated mode without fabricated temperatures or off state', () => {
  const html = showRoom([{ id: 'fixture:system', model: 'Airzone system', points: [point('mode', enumeration('off', 'heat')), point('fan_speed', enumeration('0', '1'))], values: { mode: 'heat', fan_speed: '1' } }]);
  assert.match(html, />Chaud<\/span>/);
  assert.doesNotMatch(html, /°|Éteinte|→/);
});

test('room climate with an unknown mode does not infer off', () => {
  const html = showRoom([{ id: 'fixture:system', model: 'Airzone system', points: [point('mode', enumeration('off', 'heat'))], values: { mode: null } }]);
  assert.doesNotMatch(html, /°|Éteinte|→/);
});

test('shared climate detail omits an outdoor reading when the capability is absent', () => {
  const html = show('ClimateCard', [point('mode', enumeration('off', 'heat')), point('fan_speed', enumeration('0', '1'))], { mode: 'heat', fan_speed: '1' }, { model: 'Airzone system' });
  assert.doesNotMatch(html, /Dehors|—°|— °/);
});

test('mixed power strip summary counts only writable outlets', () => {
  home.home.config = { outlets: { 'fixture:strip': { switch_1: 'Controlled', switch_2: 'Observed' } } };
  const html = showRoom([{
    id: 'fixture:strip', model: 'Home Assistant switch',
    points: [point('switch_1', { type: 'binary' }, true, 'on_off'), point('switch_2', { type: 'binary' }, false, 'on_off')],
    values: { switch_1: true, switch_2: false },
  }]);
  const group = buttons(html)[0];
  assert.doesNotMatch(group, /disabled/);
  assert.match(group, /aria-pressed="true"/);
  assert.match(group, /1 sur 1/);
  assert.doesNotMatch(group, /sur 2/);
});

test('climate watts have no leading separator without an outdoor temperature', () => {
  energy.summary = { meters: [{ id: 'meter', name: 'Fixture meter', feeds: ['fixture:device'], power: 'fixture:meter/power', power_w: 120 }] };
  const html = show('ClimateCard', [point('mode', enumeration('heat'))], { mode: 'heat' }, { model: 'Airzone system' });
  const draw = html.match(/<span class="draw num[^\"]*"[^>]*>([\s\S]*?)<\/span>/)?.[1];
  assert.ok(draw);
  assert.equal(draw.replace(/<!--.*?-->/g, '').trim(), '120 W');
});
