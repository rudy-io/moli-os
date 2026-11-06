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
  for (const name of ['ClimateCard', 'CoverTile', 'SpeakerCard', 'LightTile', 'RoomCard', 'VacuumCard', 'ValveCard', 'ScheduleEditor', 'ValveChart']) {
    components[name] = (await vite.ssrLoadModule(`/src/maison/ui/${name}.svelte`)).default;
  }
  components.Pieces = (await vite.ssrLoadModule('/src/maison/pages/Pieces.svelte')).default;
});
beforeEach(() => { hub.connected = true; home.home.config = null; energy.summary = null; });
after(async () => {
  await vite?.close();
  delete globalThis.document;
  if (localStorageDescriptor) Object.defineProperty(globalThis, 'localStorage', localStorageDescriptor);
});

const point = (key, kind, write = true, semantic = 'other') => ({ key, kind, access: { read: true, write }, semantic });
const number = (min, max, step) => ({ type: 'numeric', min, max, step });
const enumeration = (...values) => ({ type: 'enum', values });
function show(component, points, values, extra = {}, props = {}) {
  const d = { id: 'fixture:device', native_name: 'Fixture', model: null, label: {}, online: true, points, members: [], state: Object.fromEntries(Object.entries(values).map(([key, value]) => [key, { value }])), ...extra };
  hub.devices = { [d.id]: d };
  return render(components[component], { props: { id: d.id, name: 'Fixture', ...props } }).body;
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

test('read-only climate ventilation remains visible without a selector', () => {
  const html = show('ClimateCard', [point('mode', enumeration('off', 'heat'), false), point('fan_speed', enumeration('0', '1', '3'), false)], { mode: 'heat', fan_speed: '3' }, { model: 'Airzone system' });
  assert.match(html, /Ventilation/);
  assert.match(html, />3</);
  assert.doesNotMatch(html, /<select\b/);
});

test('climate readings never invent a unit and preserve explicitly reported Fahrenheit', () => {
  const temperature = point('temperature', { type: 'numeric' }, false);
  const target = point('target_temperature', number(10, 100, 0.5), false);
  for (const unit of [undefined, null]) {
    const html = show('ClimateCard', [{ ...temperature, unit }, { ...target, unit }], { temperature: 20.4, target_temperature: 21.5 });
    assert.match(html, /20,4/);
    assert.match(html, /21,5/);
    assert.doesNotMatch(html, /°/);
  }
  for (const unit of ['°C', '°F']) {
    const html = show('ClimateCard', [{ ...temperature, unit }, { ...target, unit }], { temperature: 70, target_temperature: 72 });
    assert.ok(html.includes(unit));
    assert.match(html, /70/);
    assert.match(html, /72/);
  }
});

test('room climate summaries use only the declared temperature units', () => {
  for (const unit of [null, '°F']) {
    const html = showRoom([{
      id: 'fixture:climate', model: 'Home Assistant climate',
      points: [{ ...point('temperature', { type: 'numeric' }, false), unit }, { ...point('target_temperature', { type: 'numeric' }, false), unit }],
      values: { temperature: 70, target_temperature: 72 },
    }]);
    assert.match(html, /70/);
    assert.match(html, /72/);
    if (unit) assert.ok(html.includes(unit));
    else assert.doesNotMatch(html, /°/);
  }
});

test('room averages never combine temperature scales or grade Fahrenheit as Celsius', () => {
  const entries = (units, values) => units.map((unit, index) => ({
    id: `fixture:climate${index}`, model: 'Home Assistant climate',
    points: [{ ...point('temperature', { type: 'numeric' }, false), unit }], values: { temperature: values[index] },
  }));
  const mixed = showRoom(entries(['°C', '°F'], [20, 68]));
  assert.doesNotMatch(mixed, /<span class="temp /);
  const unknown = showRoom(entries(['°C', null], [20, 68]));
  assert.doesNotMatch(unknown, /<span class="temp /);
  const celsius = showRoom(entries(['°C', '°C'], [20, 22]));
  assert.match(celsius, /class="temp good/);
  assert.match(celsius, /21,0°C/);
  const fahrenheit = showRoom(entries(['°F', '°F'], [68, 72]));
  assert.match(fahrenheit, /70,0°F/);
  assert.doesNotMatch(fahrenheit, /class="temp hot/);
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

test('read-only shutter keeps advertised command buttons visible and disabled', () => {
  const html = show('CoverTile', [point('percent_control', number(0, 100, 1), false), point('control', enumeration('open', 'close'), false)], { percent_control: 42 });
  assert.match(html, /42/);
  assert.equal(buttons(html).length, 2);
  assert.ok(buttons(html).every((button) => /disabled/.test(button)));
  assert.equal(inputs(html).length, 0);
});

test('observed bridge covers explain read-only access beside labelled icon actions', () => {
  const html = show('CoverTile', [
    point('state', { type: 'text' }, false),
    { ...point('control', enumeration('open', 'close', 'stop'), false, 'control'), access: { read: false, write: false } },
    { ...point('percent_control', number(0, 100, 1), false, 'control'), unit: '%' },
  ], { percent_control: 100 }, { model: 'Home Assistant cover' });
  assert.match(html, /Lecture seule/);
  assert.match(html, /Ouvrir.*Stop.*Fermer/s);
  assert.match(html, /Ouvert/);
  assert.doesNotMatch(html, /Ouvert à 100 %/);
  assert.match(html, /href="#\/systeme"/);
  assert.doesNotMatch(html, /code Moli|<input/);
  assert.equal(buttons(html).length, 3);
  assert.ok(buttons(html).every((button) => /disabled/.test(button)));
  assert.equal((html.match(/data-cover-command=/g) ?? []).length, 3);
});

test('unavailable covers mark retained positions as historical and never claim movement', () => {
  const html = show('CoverTile', [point('opening', number(0, 100, 1), true, 'position'), point('state', enumeration('OPEN', 'STOP', 'CLOSE'))],
    { opening: 42, moving: 'UP' }, { online: false });
  assert.match(html, /Injoignable/);
  assert.match(html, /Dernière position.*42/s);
  assert.match(html, /Ouvrir.*Stop.*Fermer/s);
  assert.doesNotMatch(html, /en ouverture|Ouvert à|<input/);
  assert.equal(buttons(html).length, 3);
  assert.ok(buttons(html).every((button) => /disabled/.test(button)));
});

test('static cover capabilities stay partial and an unknown position stays explicit', () => {
  const html = show('CoverTile', [point('control', enumeration('close', 'stop'), false)], {}, { model: 'Overkiz RollerShutter' });
  assert.match(html, /Position non communiquée/);
  assert.match(html, /Stop.*Fermer/s);
  assert.doesNotMatch(html, /Ouvrir|<input|0 %/);
  assert.equal(buttons(html).length, 2);
  assert.ok(buttons(html).every((button) => /disabled/.test(button)));
});

test('slow mode remains a selectable preview with disabled physical actions in read-only', () => {
  const html = show('CoverTile', [point('control', enumeration('open', 'stop', 'close', 'open_slow'), false),
    point('opening', number(0, 100, 1), false, 'position')], { opening: 0 });
  assert.match(html, /Fermé/);
  assert.doesNotMatch(html, /Ouvert à 0|en mouvement/);
  assert.match(html, /Vitesse de la prochaine commande/);
  const physical = buttons(html).filter((button) => /data-cover-command/.test(button));
  const speed = buttons(html).filter((button) => /aria-pressed/.test(button));
  assert.equal(physical.length, 3);
  assert.ok(physical.every((button) => /disabled/.test(button)));
  assert.equal(speed.length, 2);
  assert.ok(speed.every((button) => !/disabled/.test(button)));
});

test('a disconnected hub keeps cached positions historical and disables every physical action', () => {
  hub.connected = false;
  const html = show('CoverTile', [point('control', enumeration('open', 'stop', 'close')),
    point('opening', number(0, 100, 1), true, 'position')], { opening: 40, moving: 'UP' });
  assert.match(html, /Connexion à Moli/);
  assert.match(html, /Dernière position.*40/s);
  assert.doesNotMatch(html, /en ouverture|Ouvert à|<input/);
  assert.equal(buttons(html).length, 3);
  assert.ok(buttons(html).every((button) => /disabled/.test(button)));
});

test('icons-only covers put names and readings in a tooltip and label every icon button', () => {
  const html = show('CoverTile', [point('control', enumeration('open', 'stop', 'close', 'open_slow', 'close_slow')),
    point('opening', number(0, 100, 1), true, 'position')], { opening: 42 }, {}, { icons: true });
  assert.match(html, /title="Fixture · Ouvert à 42 %"/);
  assert.equal(buttons(html).length, 5, 'three actions and two speed choices remain available');
  for (const name of ['Ouvrir', 'Stop', 'Fermer', 'Normal', 'Lent']) assert.match(html, new RegExp(`aria-label="${name}"`));
  const contents = [...html.matchAll(/<button\b[^>]*>(.*?)<\/button>/gs)].map(([, body]) => body);
  assert.ok(contents.every((body) => /<svg\b/.test(body)));
  assert.doesNotMatch(html, />Fixture<|>Ouvert à 42 %<|>Ouvrir<|>Stop<|>Fermer<|>Normal<|>Lent<|>Position<|>Prochaine commande</);
  assert.equal(inputs(html).length, 1);
});

test('icons-only read-only and offline covers retain their safeguards and last-known tooltip', () => {
  const points = [point('control', enumeration('open', 'stop', 'close', 'open_slow'), false),
    point('opening', number(0, 100, 1), false, 'position')];
  const readonly = show('CoverTile', points, { opening: 42 }, {}, { icons: true });
  assert.match(readonly, /aria-label="Lecture seule"/);
  assert.equal(buttons(readonly).filter((button) => /disabled/.test(button)).length, 3);
  const offline = show('CoverTile', points, { opening: 42, moving: 'UP' }, { online: false }, { icons: true });
  assert.match(offline, /title="Fixture · Dernière position : 42 %"/);
  assert.match(offline, /aria-label="Injoignable"/);
  assert.doesNotMatch(offline, /en ouverture/);
  assert.equal(buttons(offline).filter((button) => /disabled/.test(button)).length, 3);
});

test('the rooms icons preference reaches each cover without changing the dedicated shutters view', () => {
  show('CoverTile', [point('control', enumeration('open', 'stop', 'close'), false),
    point('opening', number(0, 100, 1), false, 'position')], { opening: 42 }, { label: { room: 'Workshop' } });
  const room = render(components.RoomCard, { props: { room: home.roomList()[0], icons: true } }).body;
  assert.match(room, /title="Fixture · Ouvert à 42 %"/);
  const focused = render(components.Pieces, { props: { route: 'pieces/volets' } }).body;
  assert.match(focused, />Fixture</);
  assert.match(focused, />Ouvrir</);
});

test('the shutter route bypasses a stored lights filter without replacing it or deduplicating identities', () => {
  const saved = '["lumieres"]';
  const writes = [];
  globalThis.localStorage = { getItem: () => saved, setItem: (...args) => writes.push(args) };
  try {
    const cover = (id) => ({
      id, model: 'Home Assistant cover', label: { name: 'Twin shade', room: 'Studio' }, online: true,
      points: [point('control', enumeration('open', 'close', 'stop'), false)], state: {},
    });
    const entries = [cover('fixture:one'), cover('fixture:two'), { ...cover('fixture:hidden'), label: { name: 'Hidden shade', room: 'Studio' } },
      { id: 'fixture:lamp', model: 'Home Assistant light', label: { name: 'Fixture lamp', room: 'Studio' }, online: true, points: [point('on', { type: 'binary' }, false, 'on_off')], state: { on: { value: true } } }];
    hub.devices = Object.fromEntries(entries.map((d) => [d.id, d]));
    home.home.config = { hidden: ['fixture:hidden'] };
    const focused = render(components.Pieces, { props: { route: 'pieces/volets' } }).body;
    assert.equal((focused.match(/>Twin shade</g) ?? []).length, 2);
    assert.doesNotMatch(focused, /Fixture lamp|Hidden shade/);
    assert.match(focused, /href="#\/pieces"/);
    const normal = render(components.Pieces, { props: { route: 'pieces' } }).body;
    assert.match(normal, /Fixture lamp/);
    assert.doesNotMatch(normal, /Twin shade/);
    assert.match(normal, /href="#\/pieces\/volets"/);
    assert.deepEqual(writes, []);
  } finally { delete globalThis.localStorage; }
});

test('an empty shutter route explains the empty view and offers the ordinary rooms', () => {
  hub.devices = {};
  const html = render(components.Pieces, { props: { route: 'pieces/volets' } }).body;
  assert.match(html, /Aucun volet ni store à afficher/);
  assert.match(html, /href="#\/pieces"/);
});

test('the shutters view groups many individual cards by room without lighting summaries', () => {
  const rooms = [['Studio', 38], ['Workshop', 2]];
  const entries = rooms.flatMap(([room, count]) => Array.from({ length: count }, (_, index) => ({
    id: `fixture:${room}-${index}`, model: 'Home Assistant cover',
    label: { name: `${room} shade ${index + 1}`, room }, online: index !== 0,
    points: [point('control', enumeration('open', 'close', 'stop'), false)], state: {},
  })));
  hub.devices = Object.fromEntries(entries.map((d) => [d.id, d]));
  const html = render(components.Pieces, { props: { route: 'pieces/volets' } }).body;
  for (const [room, count] of rooms) {
    assert.match(html, new RegExp(`<h2[^>]*>.*?${room}.*?</h2>.*?${count} ouvrants`, 's'));
    const list = html.match(new RegExp(`<ul[^>]*aria-label="${room}"[^>]*>(.*?)</ul>`, 's'))?.[1];
    assert.ok(list, `${room} has its own accessible list`);
    assert.equal((list.match(/<li\b/g) ?? []).length, count);
    assert.match(list, /Injoignable/);
  }
  assert.doesNotMatch(html, /Tout est éteint|<input/);
  assert.equal(buttons(html).length, 120);
  assert.ok(buttons(html).every((button) => /disabled/.test(button)));
  const normal = render(components.Pieces, { props: { route: 'pieces' } }).body;
  assert.match(normal, /Tout est éteint/);
  assert.doesNotMatch(normal, /<ul[^>]*aria-label="Studio"/);
});

test('partial RTS shutter retains close and stop without inventing position or open', () => {
  const points = [point('control', enumeration('close', 'stop'))];
  const html = show('CoverTile', points, {}, { model: 'Overkiz RollerShutter' });
  assert.equal(home.deviceKind(hub.devices['fixture:device']), 'cover');
  assert.equal(buttons(html).length, 2);
  assert.equal(inputs(html).length, 0);
  const offline = show('CoverTile', points, {}, { model: 'Overkiz RollerShutter', online: false });
  assert.equal(buttons(offline).length, 2);
  assert.ok(buttons(offline).every((button) => /disabled/.test(button)));
});

test('legacy Tuya position stays informative until its normalized position semantic is declared', () => {
  const html = show('CoverTile', [point('percent_control', number(0, 100, 1)), point('control', enumeration('close', 'stop'))], { percent_control: 42 });
  assert.equal(buttons(html).length, 2);
  assert.match(html, /42/);
  assert.equal(inputs(html).length, 0);
  const partial = show('CoverTile', [point('control', enumeration('open'))], {});
  assert.equal(home.deviceKind(hub.devices['fixture:device']), 'cover');
  assert.equal(buttons(partial).length, 1);
  assert.equal(inputs(partial).length, 0);
});

test('a normalized position semantic identifies a cover and supplies its actual range', () => {
  const html = show('CoverTile', [point('opening', number(20, 80, 10), true, 'position')], { opening: 40 });
  assert.equal(home.deviceKind(hub.devices['fixture:device']), 'cover');
  assert.match(html, /40/);
  assert.equal(inputs(html).length, 1);
  assert.match(inputs(html)[0], /min="20"/);
  assert.match(inputs(html)[0], /max="80"/);
  assert.match(inputs(html)[0], /step="10"/);
  assert.doesNotMatch(inputs(html)[0], /disabled/);
});

test('the normalized position contract supplies 0–100 only when bounds are missing', () => {
  const html = show('CoverTile', [point('opening', { type: 'numeric' }, true, 'position')], { opening: 100 });
  assert.equal(inputs(html).length, 1);
  assert.match(inputs(html)[0], /min="0"/);
  assert.match(inputs(html)[0], /max="100"/);
  assert.match(inputs(html)[0], /value="100"/);
  for (const kind of [number(NaN, 100, 1), number(0, Infinity, 1), number(0, 100, 0), number(-10, 100, 1), number(0, 200, 1), { type: 'text' }]) {
    const invalid = show('CoverTile', [point('opening', kind, true, 'position')], { opening: 40 });
    assert.equal(inputs(invalid).length, 0);
  }
});

test('read-only semantic position cannot be replaced by a writable legacy percentage', () => {
  const html = show('CoverTile', [point('opening', number(0, 100, 1), false, 'position'), point('percent_control', number(0, 100, 1))], { opening: 37, percent_control: 99 });
  assert.match(html, /37/);
  assert.doesNotMatch(html, /99/);
  assert.equal(inputs(html).length, 0);
});

test('offline and unmeasured semantic covers cannot expose an active slider', () => {
  const points = [point('opening', number(0, 100, 1), true, 'position')];
  const offline = show('CoverTile', points, { opening: 40 }, { online: false });
  assert.equal(inputs(offline).length, 0);
  for (const opening of [null, '40', NaN, Infinity, -1, 101]) {
    const unmeasured = show('CoverTile', points, { opening });
    assert.equal(inputs(unmeasured).length, 1);
    assert.match(inputs(unmeasured)[0], /disabled/);
  }
});

test('case-preserving motor orders identify a cover without inventing a power switch or position', () => {
  const points = [point('state', enumeration('OPEN', 'STOP', 'CLOSE'))];
  const html = show('CoverTile', points, { moving: 'UP' });
  assert.equal(home.deviceKind(hub.devices['fixture:device']), 'cover');
  assert.equal(home.powerKey(hub.devices['fixture:device']), null);
  assert.equal(buttons(html).length, 3);
  assert.equal(inputs(html).length, 0);
  assert.match(html, /en ouverture/);
  const readonly = show('CoverTile', [point('state', enumeration('OPEN', 'STOP', 'CLOSE'), false)], { moving: 'DOWN' });
  assert.equal(home.deviceKind(hub.devices['fixture:device']), 'cover');
  assert.equal(home.powerKey(hub.devices['fixture:device'], false), null);
  assert.equal(buttons(readonly).length, 3);
  assert.ok(buttons(readonly).every((button) => /disabled/.test(button)));
  assert.match(readonly, /en fermeture/);
  const offline = show('CoverTile', points, { moving: 'forward' }, { online: false });
  assert.equal(buttons(offline).length, 3);
  assert.ok(buttons(offline).every((button) => /disabled/.test(button)));
  assert.match(offline, /Injoignable/);
  assert.doesNotMatch(offline, /mouvement/);
});

test('multiple position or motor-order candidates do not choose an arbitrary actuator', () => {
  const html = show('CoverTile', [
    point('left', number(0, 100, 1), true, 'position'),
    point('right', number(0, 100, 1), true, 'position'),
    point('percent_control', number(0, 100, 1)),
    point('left_order', enumeration('OPEN', 'CLOSE')),
    point('right_order', enumeration('OPEN', 'CLOSE')),
  ], { left: 30, right: 70, percent_control: 50 });
  assert.equal(inputs(html).length, 0);
  assert.equal(buttons(html).length, 0);
});

test('the named legacy control cannot bypass an ambiguous motor-order selection', () => {
  for (const legacy of [enumeration('OPEN', 'CLOSE'), enumeration('close', 'stop'), enumeration('cycle')]) {
    const html = show('CoverTile', [point('control', legacy), point('state', enumeration('OPEN', 'CLOSE'))], {}, { model: 'Overkiz RollerShutter' });
    assert.equal(buttons(html).length, 0);
    assert.equal(inputs(html).length, 0);
  }
});

test('a partial legacy control does not identify a fan as a cover', () => {
  show('CoverTile', [point('control', enumeration('stop')), point('on', { type: 'binary' })], { on: false }, { native_name: 'Fan' });
  assert.equal(home.deviceKind(hub.devices['fixture:device']), 'plug');
  assert.equal(home.powerKey(hub.devices['fixture:device']), 'on');
});

test('ambiguous enum casing hides only the ambiguous cover command', () => {
  const html = show('CoverTile', [point('control', enumeration('OPEN', 'open', 'STOP', 'CLOSE'))], {});
  assert.equal(buttons(html).length, 2);
  assert.doesNotMatch(html, />Ouvrir</);
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

const robot = (write = true) => [
  point('clean', { type: 'binary' }, write, 'control'),
  point('pause', { type: 'binary' }, write, 'control'),
  point('dock', { type: 'binary' }, write, 'control'),
  point('locate', { type: 'binary' }, write),
  point('state', { type: 'text' }, false),
  point('battery', number(0, 100, 1), false, 'battery'),
  point('fan_speed', enumeration('quiet', 'strong'), write, 'config'),
];
const orders = (html) => (html.match(/class="orders[\s\S]*?(?=<div class="speeds|<\/section)/)?.[0].match(/<button/g) ?? []).length;

test('a robot at its base can start and ring, not stop or go home', () => {
  const html = show('VacuumCard', robot(), { state: 'fullcharge', clean: false, pause: false, battery: 100, fan_speed: 'strong' });
  assert.equal(home.deviceKind(hub.devices['fixture:device']), 'vacuum');
  assert.equal(orders(html), 2);
  assert.match(html, /aria-pressed="true"[^>]*>[^<]*(Forte|Strong)/);
});

test('a cleaning robot offers pause, stop and home; a paused one resumes', () => {
  const cleaning = show('VacuumCard', robot(), { state: 'totaling', clean: true, pause: false, battery: 80 });
  assert.match(cleaning, /Pause/);
  assert.equal(orders(cleaning), 4);
  const paused = show('VacuumCard', robot(), { state: 'pause', clean: true, pause: true, battery: 80 });
  assert.match(paused, /Reprendre|Resume/);
  assert.equal(orders(paused), 4);
});

test('a cleaning time that is not a number is not shown', () => {
  const html = show('VacuumCard', robot(), { state: 'totaling', clean: true, pause: false, clean_time: 'soon' });
  assert.doesNotMatch(html, /NaN/);
});

test('a read-only or offline robot shows its state and offers no order', () => {
  const values = { state: 'idle', clean: false, pause: false, battery: 50, fan_speed: 'quiet' };
  assert.equal(buttons(show('VacuumCard', robot(false), values)).length, 0);
  assert.equal(buttons(show('VacuumCard', robot(), values, { online: false })).length, 0);
});

test('a fault is told in words and an unknown state keeps its own', () => {
  const html = show('VacuumCard', robot(), { state: 'remotectl', clean: false, battery: 50, fault: true, detail: 'code 27' });
  assert.match(html, /remotectl/);
  assert.match(html, /code 27/);
});

const valvePoints = (setpointWrite = true, modeWrite = true) => [
  point('occupied_heating_setpoint', { ...number(4, 35, 0.5) }, setpointWrite, 'temperature'),
  point('local_temperature', { type: 'numeric' }, false, 'temperature'),
  point('system_mode', enumeration('off', 'auto', 'heat'), modeWrite),
  point('running_state', enumeration('idle', 'heat'), false),
];

test('a Zigbee valve is a climate shown with its measured temperature, setpoint and mode', () => {
  const html = show('ValveCard', valvePoints(), { occupied_heating_setpoint: 18.5, local_temperature: 19.2, system_mode: 'auto', running_state: 'idle' });
  assert.equal(home.deviceKind(hub.devices['fixture:device']), 'climate');
  assert.match(html, /19[.,]2/);
  assert.match(html, /18[.,]5/);
  // − and +, then one button per mode the valve accepts: no preset, no extra mode.
  assert.equal(buttons(html).filter((tag) => !tag.includes('class="fold')).length, 5);
  assert.match(html, /aria-pressed="true"[^>]*>Programme/);
  assert.doesNotMatch(html, /class="dial/);
});

test('a valve setpoint stops at the bounds its own contract states', () => {
  const atMin = show('ValveCard', valvePoints(), { occupied_heating_setpoint: 4, local_temperature: 19, system_mode: 'heat' });
  assert.match(atMin, /aria-label="[^"]*"\s+disabled/);
  assert.equal(buttons(atMin).filter((tag) => tag.includes('disabled')).length, 1);
  const atMax = show('ValveCard', valvePoints(), { occupied_heating_setpoint: 35, local_temperature: 19, system_mode: 'heat' });
  assert.equal(buttons(atMax).filter((tag) => tag.includes('disabled')).length, 1);
});

test('a read-only valve shows its setpoint and mode without any control', () => {
  const html = show('ValveCard', valvePoints(false, false), { occupied_heating_setpoint: 18.5, local_temperature: 19, system_mode: 'heat' });
  // Only the read-only chart is left to open.
  assert.equal(buttons(html).filter((tag) => !tag.includes('class="fold')).length, 0);
  assert.match(html, /18[.,]5/);
});

test('a room lists its valve as a climate and opens the valve card, not the air-conditioner dial', () => {
  const html = showRoom([{ id: 'fixture:valve', points: valvePoints(), values: { occupied_heating_setpoint: 18.5, local_temperature: 19.2, system_mode: 'heat' } }]);
  assert.match(html, /19/);
  assert.doesNotMatch(html, /class="dial/);
});

const DAYS = ['monday', 'tuesday', 'wednesday', 'thursday', 'friday', 'saturday', 'sunday'];
const schedulePoints = (write = true, groups = false) => [
  ...valvePoints(),
  ...DAYS.map((day) => point(`weekly_schedule_${day}`, { type: 'text' }, write)),
  ...(groups ? [point('schedule_group_to_edit', enumeration('1', '2', '3')), point('schedule_active_group', enumeration('1', '2', '3'))] : []),
];
const valveValues = { occupied_heating_setpoint: 18.5, local_temperature: 19, system_mode: 'auto' };

test('a valve that states its days offers the schedule, one that does not offers nothing', () => {
  assert.match(show('ValveCard', schedulePoints(), valveValues), /aria-expanded="false"[^>]*>Table horaire/);
  assert.doesNotMatch(show('ValveCard', valvePoints(), valveValues), /Table horaire/);
  assert.doesNotMatch(show('ValveCard', schedulePoints(false), valveValues), /Table horaire/);
});

test('the editor shows the day the valve states: seven days, a time and a setpoint per step', () => {
  const html = show('ScheduleEditor', schedulePoints(), { ...valveValues, weekly_schedule_monday: '00:00/17 06:00/20 22:00/16' });
  assert.equal(buttons(html).filter((tag) => tag.includes('aria-pressed')).length, 7);
  // The 00:00 step is shown but fixed.
  assert.equal(inputs(html).filter((tag) => tag.includes('type="time"')).length, 3);
  assert.equal(inputs(html).filter((tag) => tag.includes('type="time"') && tag.includes('disabled')).length, 1);
  assert.equal(inputs(html).filter((tag) => tag.includes('type="number"')).length, 3);
  assert.match(html, /00:00/);
  assert.doesNotMatch(html, /remplace celle de la vanne/);
  // Nothing to save until something changes.
  assert.doesNotMatch(html, /Enregistrer/);
});

test('a valve that never reports its schedule says saving replaces it', () => {
  assert.match(show('ScheduleEditor', schedulePoints(), valveValues), /remplace celle de la vanne/);
});

test('a read-only schedule shows no editor', () => {
  assert.equal(show('ScheduleEditor', schedulePoints(false), valveValues).includes('<ul'), false);
});

test('editing a group the valve does not follow is said', () => {
  const html = show('ScheduleEditor', schedulePoints(true, true), { ...valveValues, weekly_schedule_monday: '00:00/17', schedule_group_to_edit: '2', schedule_active_group: '1' });
  assert.match(html, /groupe 2/);
  assert.match(html, /groupe 1/);
  const same = show('ScheduleEditor', schedulePoints(true, true), { ...valveValues, weekly_schedule_monday: '00:00/17', schedule_group_to_edit: '1', schedule_active_group: '1' });
  assert.doesNotMatch(same, /La vanne suit|la vanne suit/);
});

test('dual setpoints classify without a brand and render independent numeric contracts', () => {
  const points = [
    { ...point('heat_setpoint', number(5, 25, 0.5)), unit: '°C' },
    { ...point('cool_setpoint', number(60, 90, 1), false), unit: '°F' },
  ];
  const html = show('ClimateCard', points, { heat_setpoint: 25, cool_setpoint: null });
  assert.equal(home.deviceKind(hub.devices['fixture:device']), 'climate');
  const [heat, cool] = inputs(html);
  assert.match(heat, /min="5".*max="25".*step="0.5"/);
  assert.match(heat, /value="25"/);
  assert.doesNotMatch(heat, /disabled/);
  assert.match(cool, /disabled/);
  assert.doesNotMatch(cool, /value="0"/);
  assert.equal(buttons(html).filter(tag => /disabled/.test(tag)).length, 3);
  const offline = show('ClimateCard', points, { heat_setpoint: 20 }, { online: false });
  assert.ok(inputs(offline).every(tag => /disabled/.test(tag)));
});

test('a valve offers its chart, which opens on its three periods', () => {
  assert.match(show('ValveCard', valvePoints(), valveValues), /aria-expanded="false"[^>]*>Courbe/);
  const html = show('ValveChart', valvePoints(), valveValues);
  assert.equal(buttons(html).length, 3);
  assert.match(html, /aria-pressed="true"[^>]*>24 h/);
  // Nothing is drawn before the hub has answered.
  assert.doesNotMatch(html, /<svg/);
});
