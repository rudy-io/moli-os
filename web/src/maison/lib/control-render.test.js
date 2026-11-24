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
let icons;
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
  ({ icons } = await vite.ssrLoadModule('/src/maison/lib/icons.js'));
  for (const name of ['ClimateCard', 'CoverTile', 'ImpulseTile', 'SpeakerCard', 'LightTile', 'RoomCard', 'VacuumCard', 'ValveCard', 'RadiatorTile', 'ScheduleEditor', 'ValveChart', 'WeekSchedule', 'AlarmCard']) {
    components[name] = (await vite.ssrLoadModule(`/src/maison/ui/${name}.svelte`)).default;
  }
  components.NasCard = (await vite.ssrLoadModule('/src/maison/ui/infra/NasCard.svelte')).default;
  components.Pieces = (await vite.ssrLoadModule('/src/maison/pages/Pieces.svelte')).default;
  components.Salon = (await vite.ssrLoadModule('/src/maison/pages/Salon.svelte')).default;
  components.SecurityPanel = (await vite.ssrLoadModule('/src/maison/ui/SecurityPanel.svelte')).default;
  components.Dehors = (await vite.ssrLoadModule('/src/maison/pages/Dehors.svelte')).default;
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
  assert.match(room, /data-cover-command="close"/);
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
    // No link of its own any more: the « Volets » chip does it (#171).
    assert.doesNotMatch(normal, /href="#\/pieces\/volets"/);
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
  // Unmeasured but writable: the slider stays, greyed out at its lower bound, range intact.
  const [grey] = inputs(show('CoverTile', points, {}));
  assert.match(grey, /value="0"/);
  assert.match(grey, /min="0"/);
  assert.match(grey, /max="100"/);
  assert.match(grey, /step="1"/);
  // The mirror image: a measured reading keeps the slider live at that value.
  const [live] = inputs(show('CoverTile', points, { opening: 42 }));
  assert.match(live, /value="42"/);
  assert.doesNotMatch(live, /disabled/);
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
  const [, all, ...sockets] = buttons(html);
  assert.doesNotMatch(all, /disabled/);
  assert.match(all, /aria-pressed="true"/);
  assert.equal(sockets.length, 1);
  assert.match(html, /Tout allumé/);
  assert.doesNotMatch(html, /sur 2/);
});

const meross = (values) => ({
  id: 'fixture:strip', native_name: 'Multiprise', model: 'MSS425',
  points: [point('on', { type: 'binary' }, true, 'on_off'),
    ...[1, 2, 3, 4, 5].map((n) => ({ ...point(`on_${n}`, { type: 'binary' }, true, 'on_off'), label: n === 5 ? 'USB' : `Sortie ${n}` }))],
  values,
});

test('a strip with numbered outlets gets a socket for each, none for its every-outlet channel', () => {
  home.home.config = {};
  const html = showRoom([meross({ on: true, on_1: true, on_2: false, on_3: true, on_4: false, on_5: false })]);
  const [, all, ...sockets] = buttons(html);
  assert.equal(sockets.length, 5);
  assert.match(all, /aria-pressed="false"/);
  assert.match(html, /2 sur 5 allumées/);
  assert.match(sockets[0], /aria-pressed="true"/);
  assert.match(sockets[1], /aria-pressed="false"/);
  assert.match(sockets[0], /Multiprise · Sortie 1 : Allumée/);
  // The USB outlet is drawn as ports, the others as sockets.
  assert.equal((html.match(/class="port\b/g) ?? []).length, 2);
  assert.equal((html.match(/class="well\b/g) ?? []).length, 4);
  assert.doesNotMatch(html, /class="pad/);
});

test('home.json outlets name and pick a strip\'s sockets, or make it one switch', () => {
  home.home.config = { outlets: { 'fixture:strip': { on_3: 'Écran', on_1: 'Lampe' } } };
  let html = showRoom([meross({ on: true, on_1: true, on_2: true, on_3: true, on_4: true, on_5: true })]);
  const sockets = buttons(html).slice(2);
  assert.equal(sockets.length, 2);
  assert.match(sockets[0], /Écran/);
  assert.match(sockets[1], /Lampe/);
  assert.match(html, /Tout allumé/);
  home.home.config = { outlets: { 'fixture:strip': {} } };
  html = showRoom([meross({ on: true, on_1: true })]);
  assert.equal(buttons(html).length, 1);
  assert.match(html, /class="pad/);
  home.home.config = {};
});

test('a name given on the dashboard comes before the point\'s and home.json\'s', () => {
  home.home.config = {};
  let html = showRoom([{ ...meross({ on_1: true, on_2: true, on_3: true, on_4: true, on_5: false }), label: { room: 'Workshop', points: { on_2: 'Écran', on_4: 'USB' } } }]);
  let sockets = buttons(html).slice(2);
  assert.match(sockets[1], /Écran/);
  assert.match(sockets[0], /Sortie 1/);
  // Named USB: drawn as ports, as is the outlet the device calls USB.
  assert.equal((html.match(/class="port\b/g) ?? []).length, 4);
  home.home.config = { outlets: { 'fixture:strip': { on_2: 'Box', on_1: 'Lampe' } } };
  html = showRoom([{ ...meross({ on_1: true, on_2: true }), label: { room: 'Workshop', points: { on_2: 'Écran' } } }]);
  sockets = buttons(html).slice(2);
  assert.match(sockets[0], /Écran/);
  assert.match(sockets[1], /Lampe/);
  home.home.config = {};
});

test('a double plug shows only the outlets it reports', () => {
  home.home.config = {};
  const html = showRoom([meross({ on: true, on_1: true, on_2: false })]);
  assert.equal(buttons(html).length, 4);
  assert.match(html, /1 sur 2 allumées/);
});

test('an unreachable strip shows its sockets but takes no order', () => {
  home.home.config = {};
  const html = showRoom([{ ...meross({ on_1: true, on_2: true, on_3: true, on_4: true, on_5: true }), online: false }]);
  // Only its names can still be changed.
  assert.ok(buttons(html).slice(1).every((b) => /disabled/.test(b)));
  assert.doesNotMatch(buttons(html)[0], /disabled/);
  assert.match(html, /Injoignable/);
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

test('a room thermostat says when a valve was turned by hand, and offers to resume the schedule', () => {
  const points = [...valvePoints(), point('valve_override', enumeration('none', 'manual', 'airing')), point('valve_override_until', { type: 'text' }, false)];
  const at = (state) => show('ValveCard', points, { ...valveValues, ...state });
  assert.match(at({ valve_override: 'none' }), /Suit le programme/);
  assert.doesNotMatch(at({ valve_override: 'none', system_mode: 'heat' }), /Suit le programme|Reprendre/);
  const hand = at({ valve_override: 'manual', valve_override_until: '2026-11-21T13:30:00Z' });
  assert.match(hand, /Réglée sur la vanne · programme repris à \d\d:\d\d/);
  assert.match(hand, />Reprendre le programme</);
  const airing = at({ valve_override: 'airing', valve_override_until: 'pas une date' });
  assert.match(airing, /Aération/);
  assert.match(airing, /Chauffage repris à —/);
  assert.match(at({ valve_override: 'manual', system_mode: 'heat' }), />Reprendre la consigne</);
  // A Zigbee valve, without the point: nothing about it.
  assert.doesNotMatch(show('ValveCard', valvePoints(), valveValues), /Suit le programme/);
});

test('a radiator is drawn with what it does, the room against its setpoint, and what comes next', () => {
  const points = [...schedulePoints(), point('valve_override', enumeration('none', 'manual', 'airing')), point('valve_override_until', { type: 'text' }, false)];
  const at = (state, props = {}) => show('RadiatorTile', points, { ...valveValues, valve_override: 'none', ...state }, {}, props);
  const heating = at({ local_temperature: 18.2, occupied_heating_setpoint: 19 });
  assert.match(heating, /class="radiator heating/);
  assert.match(heating, /Chauffe/);
  assert.match(heating, /Chauffe(<!--[^>]*-->)? · <span[^>]*>18[.,]2° pour 19°/);
  assert.match(at({ local_temperature: 19 }), /class="radiator holding[^"]*"[\s\S]*À température/);
  assert.match(at({ local_temperature: 21 }), /Au repos/);
  assert.match(at({ system_mode: 'off' }), /class="radiator off/);
  // Turned down for airing: said, with the way back to the schedule.
  const airing = at({ valve_override: 'airing', valve_override_until: '2026-11-21T09:54:00Z' });
  assert.match(airing, /Aération/);
  assert.match(airing, /class="radiator airing/);
  assert.match(airing, />Reprendre le programme</);
  const hand = at({ valve_override: 'manual', valve_override_until: '2026-11-21T13:30:00Z' });
  assert.match(hand, /Réglée sur la vanne · programme repris à \d\d:\d\d/);
  assert.match(hand, /class="hand\b/);
  assert.doesNotMatch(heating, /Reprendre/);
  // Opening the valve's card: the whole face is the button.
  assert.match(at({}, { onopen: () => {} }), /<button[^>]*class="face\b[^>]*aria-expanded="false"/);
  assert.match(at({}, { icons: true }), /class="short\b/);
});

test('a room draws its valve as a radiator, an air conditioner keeps its button', () => {
  const html = showRoom([{ id: 'fixture:valve', points: valvePoints(), values: { occupied_heating_setpoint: 19, local_temperature: 18, system_mode: 'heat' } }]);
  assert.match(html, /class="radiator heating/);
  assert.doesNotMatch(html, /class="clim/);
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

test('a valve on its own is drawn open or closed, and off', () => {
  const heating = show('ValveCard', valvePoints(), { occupied_heating_setpoint: 20, local_temperature: 19, system_mode: 'heat', running_state: 'heat' });
  assert.match(heating, /class="radiator heating/);
  assert.match(heating, /Vanne ouverte, chauffe/);
  const closed = show('ValveCard', valvePoints(), { occupied_heating_setpoint: 18, local_temperature: 19, system_mode: 'heat', running_state: 'idle' });
  assert.match(closed, /class="radiator closed/);
  assert.match(closed, /Vanne fermée/);
  assert.match(show('ValveCard', valvePoints(), { occupied_heating_setpoint: 18, local_temperature: 19, system_mode: 'off' }), /class="radiator off/);
});

const heatOnlyPoints = () => [
  point('temperature', { type: 'numeric', step: 0.01 }, false, 'temperature'),
  point('target_temperature', number(5, 30, 0.01), true, 'temperature'),
  point('mode', enumeration('off', 'heat')),
];

test('a room thermostat says whether its valve is open, and the valve that only heats gets the radiator, not the dial', () => {
  hub.devices = {
    'room:bureau': { id: 'room:bureau', native_name: 'Bureau', model: null, label: {}, online: true, points: valvePoints(), members: ['matter:valve'],
      state: { occupied_heating_setpoint: { value: 20 }, local_temperature: { value: 19.2 }, system_mode: { value: 'heat' } } },
    'matter:valve': { id: 'matter:valve', native_name: 'Vanne', model: null, label: {}, online: true, points: heatOnlyPoints(), members: [],
      state: { temperature: { value: 22.6 }, target_temperature: { value: 18 }, mode: { value: 'heat' } } },
  };
  const room = render(components.ValveCard, { props: { id: 'room:bureau', name: 'Bureau' } }).body;
  assert.match(room, /class="radiator heating/);
  assert.match(room, /class="phase[^"]*">Chauffe</);
  assert.match(room, /Vanne fermée · radiateur 22,6°/);
  const valve = show('ValveCard', heatOnlyPoints(), { temperature: 17, target_temperature: 18, mode: 'heat' });
  assert.match(valve, /class="radiator heating/);
  assert.match(valve, /Vanne ouverte, chauffe/);
  assert.match(valve, /aria-pressed="true"[^>]*>(Marche|On)</);
  assert.doesNotMatch(valve, /class="dial/);
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

test('a day the valve holds can be copied to other days, one it does not hold cannot', () => {
  assert.match(show('ScheduleEditor', schedulePoints(), { ...valveValues, weekly_schedule_monday: '00:00/17 06:00/20' }), /Copier vers d’autres jours/);
  assert.doesNotMatch(show('ScheduleEditor', schedulePoints(), valveValues), /Copier vers/);
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

test('a valve offers its chart, which opens on its three periods', () => {
  assert.match(show('ValveCard', valvePoints(), valveValues), /aria-expanded="false"[^>]*>Courbe/);
  const html = show('ValveChart', valvePoints(), valveValues);
  assert.equal(buttons(html).length, 3);
  assert.match(html, /aria-pressed="true"[^>]*>24 h/);
  // Nothing is drawn before the hub has answered.
  assert.doesNotMatch(html, /<svg/);
});

// A gate's contacts: trigger relays, one `cycle` order each, no state to show.
const trigger = (key, label) => ({ key, label, kind: enumeration('cycle'), access: { read: false, write: true }, semantic: 'control' });

test('trigger relays are not shutters and show one Impulsion button each', () => {
  const points = [trigger('state_l1', 'State'), trigger('state_l2', 'State')];
  assert.equal(home.deviceKind({ id: 'z2m:0xa', points, state: {} }), 'impulse');
  const html = show('ImpulseTile', points, {});
  assert.match(html, /Fixture/);
  assert.equal(buttons(html).length, 2);
  assert.match(html, /Impulsion · Relais 1/);
  assert.match(html, /Impulsion · Relais 2/);
  assert.match(html, /aria-label="Fixture : Impulsion · Relais 1"/);
  // A trigger has no on/off state: nothing says it is on, off, open or closed.
  assert.doesNotMatch(html, /Allumée|Éteinte|Ouvert|Fermé|<input/);
});

test('trigger buttons stay disabled while the hub is down or the relay is offline', () => {
  const points = [trigger('state_l1', 'State')];
  hub.connected = false;
  assert.match(buttons(show('ImpulseTile', points, {}))[0], /disabled/);
  hub.connected = true;
  assert.match(buttons(show('ImpulseTile', points, {}, { online: false }))[0], /disabled/);
  assert.doesNotMatch(buttons(show('ImpulseTile', points, {}))[0], /disabled/);
});

test('icons-only trigger relays keep a label on every icon button', () => {
  const html = show('ImpulseTile', [trigger('state_l1', 'State'), trigger('state_l2', 'State')], {}, {}, { icons: true });
  assert.equal(buttons(html).length, 2);
  assert.match(html, /aria-label="Fixture : Impulsion · Relais 1"/);
  assert.doesNotMatch(html, />Impulsion · Relais 1</);
});

test('a room shows a one-relay trigger as a pad and a lone control point stays a cover', () => {
  const room = showRoom([
    { id: 'z2m:gate', points: [trigger('state_l1', 'State')], values: {} },
    { id: 'fixture:shutter', points: [point('control', enumeration('open', 'close', 'stop'))], values: {} },
  ]);
  // One relay: a pad with its name and « Impulsion », no relay buttons.
  assert.match(room, /data-pulse="idle"/);
  assert.doesNotMatch(room, /Impulsion · Relais 1/);
  // No measured position: the pad opens the cover tile instead of sending open.
  assert.ok(room.includes(icons.shutter));
  assert.doesNotMatch(room, /data-cover-command/);
});

test('a trigger device with several relays keeps one button per relay', () => {
  const room = showRoom([
    { id: 'z2m:gate', points: [trigger('state_l1', 'State'), trigger('state_l2', 'State')], values: {} },
  ]);
  assert.match(room, /Impulsion · Relais 1/);
  assert.match(room, /Impulsion · Relais 2/);
  assert.doesNotMatch(room, /data-pulse=/);
});

test('a one-relay trigger pad is named, draws a garage door or a gate, and waits for the hub', () => {
  const pad = (html) => buttons(html).find((button) => /data-pulse=/.test(button));
  const garage = showRoom([{ id: 'shelly:door', points: [trigger('relay', 'Ouvrir / fermer')], values: {}, label: { room: 'Workshop', name: 'Porte atelier' } }]);
  assert.match(garage, /<b[^>]*>Porte atelier<\/b><small[^>]*>Impulsion<\/small>/);
  assert.ok(garage.includes(icons.garage));
  assert.doesNotMatch(pad(garage), /disabled/);
  const gate = showRoom([{ id: 'shelly:gate', points: [trigger('relay', 'Ouvrir / fermer')], values: {}, label: { room: 'Workshop', name: 'Portail' } }]);
  assert.ok(gate.includes(icons.gate));
  hub.connected = false;
  assert.match(pad(showRoom([{ id: 'shelly:door', points: [trigger('relay', 'Ouvrir / fermer')], values: {} }])), /disabled/);
  hub.connected = true;
  assert.match(pad(showRoom([{ id: 'shelly:door', points: [{ ...trigger('relay', 'Ouvrir / fermer'), access: { read: false, write: false } }], values: {} }])), /disabled/);
});

test('a cover pad draws the open shutter when open and says closed at 0 %', () => {
  const position = [point('opening', number(0, 100, 1), true, 'position')];
  const opened = showRoom([{ id: 'fixture:shutter', points: position, values: { opening: 42 } }]);
  assert.ok(opened.includes(icons['shutter-open']));
  assert.ok(!opened.includes(icons['shutter-alert']));
  assert.match(opened, /<small[^>]*>Ouvert à 42 %<\/small>/);
  const closed = showRoom([{ id: 'fixture:shutter', points: position, values: { opening: 0 } }]);
  assert.ok(closed.includes(icons.shutter));
  assert.match(closed, /<small[^>]*>Fermé<\/small>/);
  assert.doesNotMatch(closed, /Ouvert à 0/);
  const full = showRoom([{ id: 'fixture:shutter', points: position, values: { opening: 100 } }]);
  assert.match(full, /<small[^>]*>Ouvert<\/small>/);
});

test('an open cover tile draws the open shutter and says open, closed at 0 %', () => {
  const position = [point('opening', number(0, 100, 1), true, 'position')];
  const opened = show('CoverTile', position, { opening: 100 });
  assert.match(opened, /class="device-icon[^"]*open/);
  assert.ok(opened.includes(icons['shutter-open']));
  assert.match(opened, /<small[^>]*>Ouvert<\/small>/);
  const closed = show('CoverTile', position, { opening: 0 });
  assert.ok(closed.includes(icons.shutter));
  assert.match(closed, /<small[^>]*>Fermé<\/small>/);
});

test('a trigger tile named as a garage door draws a garage, a gate stays a gate', () => {
  const points = [trigger('state_l1', 'State'), trigger('state_l2', 'State')];
  assert.ok(show('ImpulseTile', points, {}, {}, { name: 'Porte voiture' }).includes(icons.garage));
  assert.ok(show('ImpulseTile', points, {}, {}, { name: 'Portail' }).includes(icons.gate));
});

test('a cover pad is disabled when its tile could not act: offline, read-only or hub down', () => {
  const cover = (extra = {}) => ({
    id: 'fixture:shutter', points: [point('control', enumeration('open', 'close', 'stop'))], values: {}, ...extra,
  });
  const pad = (html) => buttons(html).find((button) => /class="pad/.test(button));
  assert.doesNotMatch(pad(showRoom([cover()])), /disabled/);
  assert.match(pad(showRoom([cover({ online: false })])), /disabled/);
  assert.match(pad(showRoom([cover({ points: [point('control', enumeration('open', 'close', 'stop'), false)] })])), /disabled/);
  hub.connected = false;
  assert.match(pad(showRoom([cover()])), /disabled/);
  hub.connected = true;
});

test('a presence radar shows someone there, whatever the key of its presence point', () => {
  const presence = (key) => ({ ...point(key, { type: 'binary' }, false, 'occupancy') });
  const lux = { ...point('illuminance', number(0, 10000), false, 'illuminance'), unit: 'lx' };
  const room = showRoom([
    { id: 'z2m:radar', points: [lux, presence('presence')], values: { illuminance: 2000, presence: true } },
  ]);
  assert.match(room, /title="[^"]+ : présence"/);
  const empty = showRoom([
    { id: 'z2m:radar', points: [lux, presence('presence')], values: { illuminance: 2000, presence: false } },
    { id: 'z2m:pir', points: [presence('occupancy')], values: { occupancy: false } },
  ]);
  assert.doesNotMatch(empty, /: présence"/);
});

test('the security panel lists a presence radar beside the motion sensors', () => {
  const presence = (key) => ({ ...point(key, { type: 'binary' }, false, 'occupancy') });
  const at = (values) => Object.fromEntries(Object.entries(values).map(([key, value]) => [key, { value, ts: Date.now() }]));
  hub.devices = {
    'z2m:radar': { id: 'z2m:radar', native_name: 'Radar salon', model: null, label: { room: 'Salon' }, online: true, members: [], points: [presence('presence')], state: at({ presence: true }) },
    'z2m:pir': { id: 'z2m:pir', native_name: 'PIR cellier', model: null, label: { room: 'Cellier' }, online: true, members: [], points: [presence('occupancy')], state: at({ occupancy: false }) },
    // Who is home is no sensor, whatever the people driver's instance is called.
    'personnes:maison': { id: 'personnes:maison', instance: 'personnes', native_name: 'Le foyer', model: null, label: {}, online: true, members: [], points: [presence('anyone_home')], state: at({}) },
  };
  hub.drivers = [{ instance: 'personnes', kind: 'people' }];
  const html = render(components.SecurityPanel, { props: {} }).body;
  hub.drivers = [];
  assert.match(html, /Radar salon/);
  assert.match(html, /PIR cellier/);
  assert.doesNotMatch(html, /Le foyer/);
});

test('the security panel keeps the « Sécurité » room switches beside the panel card', () => {
  const at = (values) => Object.fromEntries(Object.entries(values).map(([key, value]) => [key, { value }]));
  const roomDevice = (id, name, value) => ({
    id, native_name: name, model: null, label: { room: 'Sécurité' }, online: true, members: [],
    points: [point('value', { type: 'binary' }, true, 'on_off')], state: at({ value }),
  });
  hub.devices = {
    'maison:alarme_active': roomDevice('maison:alarme_active', 'Alarme', false),
    'maison:alarme_en_cours': roomDevice('maison:alarme_en_cours', 'Alarme en cours', true),
    'fixture:panel': { id: 'fixture:panel', native_name: "Centrale d'alarme", model: null, label: { room: 'Entrée' }, online: true, members: [], points: [point('system_arm_type', enumeration('disarmed', 'armed', 'home'))], state: at({ system_arm_type: 'armed' }) },
  };
  const html = render(components.SecurityPanel, { props: {} }).body;
  // The room's switches, as before, and the ringing line first.
  assert.match(html, /Alarme : non/);
  assert.match(html, /Alarme en cours : oui/);
  assert.match(html, /Alarme en cours !/);
  // The panel keeps its own card, with its mode.
  assert.match(html, /Centrale d'alarme/);
  assert.match(html, />Armée</);
});

test('a plant sensor shows its soil, and its pot stays out of the room readings (#163)', () => {
  const celsius = (key) => ({ ...point(key, number(), false, 'temperature'), unit: '°C' });
  const percent = (key, semantic) => ({ ...point(key, number(0, 100), false, semantic), unit: '%' });
  const plant = (values) => ({
    id: 'z2m:plant',
    points: [percent('soil_moisture', 'soil_moisture'), point('dry', { type: 'binary' }, false, 'soil_dry'),
      celsius('temperature'), percent('humidity', 'humidity'), percent('soil_warning', 'other')],
    values: { temperature: 30, humidity: 75, soil_warning: 20, dry: false, ...values },
  });
  const thermometer = { id: 'z2m:room', points: [celsius('temperature')], values: { temperature: 20 } };

  const fine = showRoom([plant({ soil_moisture: 54 }), thermometer]);
  assert.equal(home.deviceKind(hub.devices['z2m:plant']), 'plant');
  // The room's temperature is the thermometer's alone (not 25 °C with the pot's 30).
  assert.match(fine, />20,0°C</);
  // The pot's air is no humidity of the room.
  assert.doesNotMatch(fine, /75 %/);
  assert.match(fine, /title="z2m:plant : terre à 54 %"/);
  assert.doesNotMatch(fine, /à arroser/);

  // Thirsty: under the sensor's threshold, or by its own alarm. Never a leak.
  for (const values of [{ soil_moisture: 12 }, { soil_moisture: 54, dry: true }]) {
    const thirsty = showRoom([plant(values), thermometer]);
    assert.match(thirsty, /class="chip warm[^"]*" title="z2m:plant : terre à \d+ %, à arroser"/);
    assert.doesNotMatch(thirsty, /fuite/i);
  }
});

test('the « Volets » chip alone shows the shutters view; with others, the rooms (#171)', () => {
  let saved = '["volets"]';
  globalThis.localStorage = { getItem: () => saved, setItem: () => {} };
  try {
    const entries = [
      { id: 'fixture:shade', model: 'Home Assistant cover', label: { name: 'Shade', room: 'Studio' }, online: true,
        points: [point('control', enumeration('open', 'close', 'stop'), false)], state: {} },
      { id: 'fixture:lamp', model: 'Home Assistant light', label: { name: 'Lamp', room: 'Studio' }, online: true,
        points: [point('on', { type: 'binary' }, false, 'on_off')], state: { on: { value: true } } },
    ];
    hub.devices = Object.fromEntries(entries.map((d) => [d.id, d]));
    home.home.config = {};
    const alone = render(components.Pieces, { props: { route: 'pieces' } }).body;
    // The shutters' grid, under the page's own title and its filters.
    assert.match(alone, /class="cover-grid/);
    assert.match(alone, />Ouvrir</);
    assert.doesNotMatch(alone, />Lamp</);
    assert.match(alone, /<h1 class="page-title[^"]*">Pièces</);
    assert.match(alone, /class="bar/);

    saved = '{"v":2,"ids":["volets","lumieres"]}';
    const mixed = render(components.Pieces, { props: { route: 'pieces' } }).body;
    assert.doesNotMatch(mixed, /class="cover-grid/);
    assert.match(mixed, />Lamp</);
  } finally { delete globalThis.localStorage; }
});

test('« Climat » and « Volets » are two chips; openers go with the shutters (#171)', async () => {
  const filters = await vite.ssrLoadModule('/src/maison/lib/filters.js');
  const ids = filters.CATEGORIES.map((c) => c.id);
  assert.ok(ids.includes('climat') && ids.includes('volets'));
  const kinds = (id) => filters.CATEGORIES.find((c) => c.id === id).kinds;
  assert.deepEqual(kinds('climat'), ['climate']);
  assert.ok(kinds('volets').includes('cover') && kinds('volets').includes('impulse'));
});

test('a filter saved as « climat » before #171 keeps its shutters, a new one does not', async () => {
  const filters = await vite.ssrLoadModule('/src/maison/lib/filters.js');
  const store = {};
  globalThis.localStorage = { getItem: (k) => store[k] ?? null, setItem: (k, v) => { store[k] = v; } };
  try {
    store.old = '["climat"]';
    assert.deepEqual(filters.loadFilter('old'), ['climat', 'volets']);
    filters.saveFilter('new', ['climat']);
    assert.deepEqual(JSON.parse(store.new), { v: 2, ids: ['climat'] });
    assert.deepEqual(filters.loadFilter('new'), ['climat']);
    store.junk = '{"v":2}';
    assert.deepEqual(filters.loadFilter('junk'), []);
  } finally { delete globalThis.localStorage; }
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

test('a button shows its last press while fresh, and always among the sensors (#198)', () => {
  const action = point('action', enumeration('single', 'double', 'hold'), false, 'press');
  const button = (state) => [{ id: 'z2m:button', native_name: 'Bouton', model: 'ZG-101ZL', label: { room: 'Workshop' }, online: true,
    points: [action, { ...point('battery', number(0, 100), false, 'battery'), unit: '%' }], members: [], state }];
  const card = (state, every = false) => {
    hub.devices = Object.fromEntries(button(state).map((d) => [d.id, d]));
    return render(components.RoomCard, { props: { room: home.roomList()[0], every } }).body;
  };
  const now = Date.now();
  home.home.now = now;
  const pressed = (value, minutes) => ({ action: { value, ts: now - minutes * 60_000 }, battery: { value: 100, ts: now } });

  // Fresh: shown, warm, in words.
  const fresh = card(pressed('double', 3));
  assert.match(fresh, /class="chip warm[^"]*" title="Bouton : dernier appui double, il y a 3 min"/);
  assert.match(fresh, />double · il y a 3 min</);
  assert.match(card(pressed('hold', 1)), />maintenu · il y a 1 min</);

  // Old: gone from the room's line, still there among the sensors (not warm).
  const old = pressed('single', 45);
  assert.doesNotMatch(card(old), /dernier appui/);
  assert.match(card(old, true), /class="chip [^w"][^"]*" title="Bouton : dernier appui simple, il y a 45 min"/);

  // No press seen yet (a retained one is never shown): nothing to say.
  assert.doesNotMatch(card({ battery: { value: 100, ts: now } }, true), /dernier appui/);
  // A settable `action` is no press.
  hub.devices = { 'z2m:x': { ...button(pressed('single', 1))[0], id: 'z2m:x', points: [{ ...action, access: { read: true, write: true } }] } };
  assert.doesNotMatch(render(components.RoomCard, { props: { room: home.roomList()[0], every: true } }).body, /dernier appui/);
  // Nor an `action` its driver did not call a press (#211): the key says nothing.
  hub.devices = { 'z2m:x': { ...button(pressed('single', 1))[0], id: 'z2m:x', points: [{ ...action, semantic: 'other' }] } };
  assert.doesNotMatch(render(components.RoomCard, { props: { room: home.roomList()[0], every: true } }).body, /dernier appui/);
});

test('a press shows whatever the brand, one chip for a remote, named by the gesture (#211)', () => {
  const now = Date.now();
  home.home.now = now;
  const at = (value, seconds) => ({ value, ts: now - seconds * 1000 });
  const card = (d) => {
    hub.devices = { [d.id]: { native_name: 'Télécommande', model: null, label: { room: 'Workshop' }, online: true, members: [], ...d } };
    return render(components.RoomCard, { props: { room: home.roomList()[0], every: true } }).body;
  };
  const hueButton = (n) => ({
    ...point(`button${n}`, enumeration('initial_press', 'repeat', 'short_release', 'long_release', 'long_press', 'double_short_release'), false, 'press'),
    label: `Bouton ${n}`,
  });
  const dimmer = { id: 'hue:dimmer', points: [1, 2, 3, 4].map(hueButton) };

  // A Hue dimmer: one chip, the last button pressed, by its name.
  const two = card({ ...dimmer, state: { button1: at('short_release', 600), button2: at('short_release', 120) } });
  assert.equal(two.match(/dernier appui/g)?.length, 1);
  assert.match(two, />Bouton 2 · simple · il y a 2 min</);
  assert.match(two, /title="Télécommande · Bouton 2 : dernier appui simple, il y a 2 min"/);
  // A hold ends on `long_release`: « maintenu », never « relâché ».
  assert.match(card({ ...dimmer, state: { button3: at('long_release', 60) } }), />Bouton 3 · maintenu · il y a 1 min</);
  assert.match(card({ ...dimmer, state: { button1: at('double_short_release', 60) } }), />Bouton 1 · double · il y a 1 min</);

  // Dirigera (`button`, text) and Tuya (`switch_mode1`): their own keys, their own words.
  const dirigera = { id: 'dirigera:remote', points: [point('button', { type: 'text' }, false, 'press')] };
  assert.match(card({ ...dirigera, state: { button: at('long', 60) } }), />maintenu · il y a 1 min</);
  const tuya = { id: 'tuya:switch', points: [point('switch_mode1', enumeration('click', 'double_click', 'press'), false, 'press')] };
  assert.match(card({ ...tuya, state: { switch_mode1: at('double_click', 60) } }), />double · il y a 1 min</);
  // An unknown word shows as told.
  assert.match(card({ ...tuya, state: { switch_mode1: at('rotate_left', 60) } }), />rotate_left · il y a 1 min</);
});

test('a heat pump and its circuit are heating and stay off the room cards; a thermostat stays climate', () => {
  const pump = { id: 'vicare:pac', native_name: 'PAC', model: null, label: { room: 'Workshop' }, online: true, members: [], points: [point('compressor_active', { type: 'binary' }, false)], state: {} };
  const circuit = { ...pump, id: 'vicare:circuit0', native_name: 'Circuit', points: [point('mode', enumeration('heating', 'standby')), point('comfort_temperature', number(4, 37, 1)), point('reduced_temperature', number(4, 37, 1))] };
  const thermostat = { ...pump, id: 'other:thermostat', native_name: 'Thermostat', points: [point('mode', enumeration('heat', 'off')), point('comfort_temperature', number(5, 30, 0.5)), point('target_temperature', number(5, 30, 0.5))] };
  hub.devices = { [pump.id]: pump, [circuit.id]: circuit, [thermostat.id]: thermostat };
  assert.equal(home.deviceKind(pump), 'heating');
  assert.equal(home.deviceKind(circuit), 'heating');
  assert.equal(home.deviceKind(thermostat), 'climate');
  const [room] = home.roomList();
  assert.deepEqual(room.climates.map((d) => d.id), [thermostat.id]);
  hub.devices = { [pump.id]: pump, [circuit.id]: circuit };
  assert.deepEqual(home.roomList(), []);
});

test('a room /api/home added (outside, unlisted) takes no place in the order', () => {
  const lamp = (id, room) => ({
    id, model: 'Home Assistant light', label: { name: id, room }, online: true,
    points: [point('on', { type: 'binary' }, false, 'on_off')], state: {},
  });
  hub.devices = Object.fromEntries([lamp('l:salon', 'Salon'), lamp('l:ext', 'Extérieur'), lamp('l:bureau', 'Bureau')].map((d) => [d.id, d]));
  home.home.config = { rooms: [{ name: 'Bureau' }, { name: 'Extérieur', outside: true, added: true }] };
  assert.deepEqual(home.roomList().map((r) => r.name), ['Bureau', 'Salon', 'Extérieur']);
  home.home.config = {};
});

function nasDevice(native, name, values) {
  return { id: `nas:${native}`, instance: 'nas', native_name: name, model: null, manufacturer: 'Synology', label: {}, online: true, points: [], members: [], state: Object.fromEntries(Object.entries(values).map(([key, value]) => [key, { value }])) };
}

test('the NAS card shows a NAS that answers, its volume, drive, UPS and backup, with no order button', () => {
  const nas = nasDevice('nas', 'DiskStation', { up: true, cpu: 12, memory: 40, temperature: 41, uptime: 50 });
  const group = { instance: 'nas', nas, volumes: [nasDevice('volume_1', 'Volume 1', { used: 63, free: 900, problem: false })], disks: [nasDevice('disk_sda', 'Drive 1', { temperature: 37, problem: false })], ups: nasDevice('ups', 'UPS', { battery: 100, problem: false }), backups: [nasDevice('backup_8', 'Night backup', { last_run: '2026/11/14 03:46:02', problem: false })] };
  hub.devices = Object.fromEntries([nas, ...group.volumes, ...group.disks, group.ups, ...group.backups].map((d) => [d.id, d]));
  const html = render(components.NasCard, { props: { group } }).body;
  for (const word of ['DiskStation', 'Volume 1', 'Drive 1', 'Night backup', '14/11 03:46']) assert.ok(html.includes(word), word);
  assert.equal(buttons(html).length, 0);
});

test('the NAS card of a NAS that sleeps says so and shows no gauge or volume', () => {
  const nas = nasDevice('nas', 'DiskStation', { up: false });
  const group = { instance: 'nas', nas, volumes: [nasDevice('volume_1', 'Volume 1', {})], disks: [], ups: null, backups: [] };
  hub.devices = Object.fromEntries([nas, ...group.volumes].map((d) => [d.id, d]));
  const html = render(components.NasCard, { props: { group } }).body;
  assert.ok(html.includes('DiskStation'));
  assert.ok(!html.includes('Volume 1'));
  assert.ok(!html.includes('class="gauges"'));
});

test('the living room drives an Apple TV: its own apps, and no key it does not have', () => {
  const id = 'appletv:ATV-TEST';
  const keys = ['CursorUp', 'CursorDown', 'CursorLeft', 'CursorRight', 'Confirm', 'Back', 'Home', 'PlayPause', 'Next', 'Previous', 'VolumeUp', 'VolumeDown', 'Options'];
  hub.devices = { [id]: {
    id, native_name: 'Apple TV', model: 'Apple TV', label: { room: 'Salon' }, online: true, members: [],
    points: [
      point('power', { type: 'binary' }, true, 'on_off'),
      { ...point('key', enumeration(...keys)), access: { read: false, write: true } },
      { ...point('app', { type: 'text' }), access: { read: false, write: true } },
      point('apps', { type: 'text' }, false),
    ],
    state: {
      power: { value: true },
      apps: { value: 'App\u00a0Store=com.apple.TVAppStore\nDisney+=com.disney.disneyplus\nNetflix=com.netflix.Netflix\nPhotos=com.apple.TVPhotos' },
    },
  } };
  home.home.config = { salon: { tv: id } };
  const html = render(components.Salon, { props: {} }).body;
  // Its favourites it has, in the usual order; none it lacks, nor Android's.
  assert.ok(html.indexOf('>Netflix<') > 0 && html.indexOf('>Netflix<') < html.indexOf('>Disney+<'), 'Netflix then Disney+');
  assert.doesNotMatch(html, />YouTube</);
  assert.doesNotMatch(html, />Photos</, 'not a favourite');
  // No mute on an Apple TV; the keys it has stay.
  assert.doesNotMatch(html, /Couper le son/);
  assert.match(html, /aria-label="Options"/);
  // A device that announces no keys keeps every one (an older driver).
  hub.devices[id].points = hub.devices[id].points.filter((p) => p.key !== 'key');
  assert.match(render(components.Salon, { props: {} }).body, /Couper le son/);
  // An app listed twice (no favourite among them): shown once, the page stands.
  hub.devices[id].state.apps = { value: 'Arte=tv.arte\nArte=tv.arte\nPhotos=com.apple.TVPhotos' };
  const twice = render(components.Salon, { props: {} }).body;
  assert.equal(twice.split('>Arte<').length, 2, 'Arte once');
  assert.match(twice, />Photos</);
});

test('a plug that measures its draw says it on its pad, only while on', () => {
  const plug = (on, power, unit = 'W') => ({
    id: 'fixture:plug', model: 'Smart plug', label: { room: 'Workshop', name: 'Floor lamp' },
    points: [point('on', { type: 'binary' }, true, 'on_off'), { ...point('power', { type: 'numeric' }, false, 'power'), unit }],
    values: { on, power },
  });
  const lit = showRoom([plug(true, 42.4)]);
  assert.match(lit, /42 W/);
  assert.match(lit, /title="Floor lamp · [^"]*42 W"/);
  assert.match(showRoom([plug(true, 1.5, 'kW')]), /1[,.]5 kW/);
  const off = showRoom([plug(false, 0)]);
  assert.doesNotMatch(off, /0 W/);
  assert.match(off, /title="Floor lamp"/);
  // No reading yet: the plain state, never an invented figure.
  assert.doesNotMatch(showRoom([plug(true, null)]), / W</);
});

test('the week shows every day as bands, a boundary to drag between two steps', () => {
  const html = show('WeekSchedule', schedulePoints(), { ...valveValues, weekly_schedule_monday: '00:00/17 06:00/20 22:00/16' });
  // Seven days to choose, one row each.
  assert.equal(buttons(html).filter((tag) => tag.includes('aria-pressed')).length, 7);
  // Monday's two changes can be dragged; the other days hold one step and none.
  const edges = buttons(html).filter((tag) => tag.includes('class="edge'));
  assert.equal(edges.length, 2);
  assert.match(edges[0], /06:00 vers 20°/);
  assert.match(html, /00:00 → 06:00 : 17°/);
  assert.match(html, /22:00 → 00:00 : 16°/);
  // The colour of a step is its warmth on the shared scale.
  assert.match(html, /var\(--warm\) 63%/);
});

test('beside the week, the card leaves the schedule out and the editor opens the chosen day', () => {
  assert.doesNotMatch(show('ValveCard', schedulePoints(), valveValues, {}, { schedule: false }), /Table horaire/);
  const html = show('ScheduleEditor', schedulePoints(), { ...valveValues, weekly_schedule_wednesday: '00:00/17 07:00/20' }, {}, { day: 'wednesday' });
  assert.equal(inputs(html).filter((tag) => tag.includes('type="time"')).length, 2);
});

test('an alarm panel shows its arm mode, health and its settings', () => {
  const html = show('AlarmCard', [
    point('system_arm_type', enumeration('disarmed', 'armed', 'home')),
    point('bat_status', number(0, 100, 1), false),
    point('gsm_status', enumeration('Normal'), false),
    point('dc_status', enumeration('normal'), false),
    point('inside_siren_en', { type: 'binary' }),
    point('arm_delay', number(0, 300, 1)),
    point('inside_siren_sound', enumeration('ON', 'OFF')),
  ], {
    system_arm_type: 'disarmed',
    bat_status: 92,
    gsm_status: 'Normal',
    dc_status: 'normal',
    inside_siren_en: true,
    arm_delay: 30,
    inside_siren_sound: 'ON',
  });
  // The three arm modes, the current one pressed.
  assert.match(html, /role="group"/);
  assert.match(html, /aria-pressed="true"/);
  assert.match(html, />Désarmée</);
  assert.match(html, />Armée</);
  assert.match(html, />Maison</);
  // Health and the settings the panel takes, in the house's words.
  assert.match(html, /92 %/);
  assert.match(html, /GSM OK/);
  // A delay is a setting, never a countdown: it lives in the settings only.
  assert.doesNotMatch(html, /Armement dans|Alarme dans/);
  assert.match(html, /Réglages/);
  assert.match(html, /Délai d'armement/);
  // Settings are grouped, numbers are sliders with a step-by-step value,
  // and the siren's on/off is a two-word choice.
  assert.match(html, />Délais</);
  assert.match(html, />Sirène</);
  assert.match(html, /type="range"/);
  assert.match(html, /aria-label="Diminuer : Délai d'armement"/);
  assert.match(html, /aria-label="Augmenter : Délai d'armement"/);
  const segments = buttons(html).filter((tag) => /class="segment\b/.test(tag));
  assert.equal(segments.length, 5, 'three arm modes, two siren sounds');
  assert.equal(segments.filter((tag) => /aria-pressed="true"/.test(tag)).length, 2, 'the current mode and Fort');
  assert.match(html, />Fort</);
  assert.match(html, />Muet</);
  assert.equal(buttons(html).filter((tag) => /role="switch"/.test(tag)).length, 1);
});

test('an alarm panel reaches the house and its room, not the sensors', () => {
  const panel = {
    id: 'fixture:panel', native_name: 'Panel', model: null, label: { room: 'Hall' }, online: true, members: [],
    points: [point('system_arm_type', enumeration('disarmed', 'armed', 'home'))],
    state: { system_arm_type: { value: 'disarmed' } },
  };
  hub.devices = { [panel.id]: panel };
  assert.equal(home.deviceKind(panel), 'alarm');
  assert.deepEqual(home.alarmDevices().map((d) => d.id), [panel.id]);
  const room = home.roomList()[0];
  assert.deepEqual(room.alarms.map((d) => d.id), [panel.id]);
  assert.equal(room.sensors.length, 0);
});

test('the villa demo shows its alarm panel in the cameras safety panel', () => {
  const fixture = JSON.parse(readFileSync(new URL('../../../../demo/villa/devices/tuya.json', import.meta.url), 'utf8')).devices;
  const panel = fixture.find((d) => d.points.some((p) => p.key === 'system_arm_type'));
  assert.ok(panel, 'the villa has an alarm panel');
  hub.devices = {
    [panel.id]: {
      ...panel,
      label: {},
      online: true,
      members: [],
      state: Object.fromEntries(Object.entries(panel.state).map(([key, value]) => [key, { value }])),
    },
  };
  assert.equal(home.deviceKind(hub.devices[panel.id]), 'alarm');
  const html = render(components.SecurityPanel, { props: {} }).body;
  // Its arm mode, health and the settings it takes, inside the safety panel.
  assert.match(html, /Centrale d'alarme/);
  assert.match(html, />Armée</);
  assert.match(html, />Maison</);
  assert.match(html, /96 %/);
  assert.match(html, /Secteur OK/);
});

test('the outside page shows the sun: its height and way, its arc by day, the façades it is on (#258)', () => {
  const at = (iso) => Date.parse(iso);
  const dev = (id, points, values) => ({
    id, native_name: id, model: null, label: {}, online: true, members: [], points,
    state: Object.fromEntries(Object.entries(values).map(([key, value]) => [key, { value, ts: at('2026-11-20T10:00:00Z') }])),
  });
  const text = (key) => point(key, { type: 'text' }, false, 'config');
  const deg = (key) => point(key, number(-90, 360, 0.5), false);
  const weather = dev('meteo:x', [text('sunrise'), text('sunset')], { sunrise: '2026-11-20T07:15', sunset: '2026-11-20T16:15' });
  const sun = (values, withFacades = true) =>
    dev('dehors:soleil', [deg('elevation'), deg('azimuth'), ...(withFacades ? [text('facades'), deg('sud_incidence'), point('sud_sun', { type: 'binary' }, false), deg('est_incidence'), point('est_sun', { type: 'binary' }, false)] : [])], values);
  const page = (devices, now) => {
    home.home.config = { outdoor: { weather: 'meteo:x' } };
    home.home.now = at(now);
    hub.devices = Object.fromEntries(devices.map((d) => [d.id, d]));
    return render(components.Dehors, { props: {} }).body.replace(/<!--.*?-->/g, '');
  };

  // By day: height and way, the sun on its arc, the façade it is on lit.
  const day = page(
    [weather, sun({ elevation: 18.5, azimuth: 172, facades: 'sud=Façade sud\nest=Façade est', sud_incidence: 35, sud_sun: true, est_incidence: 75.5, est_sun: false })],
    '2026-11-20T11:45:00',
  );
  assert.match(day, /hauteur 19° · S</);
  assert.match(day, /class="sunarc/);
  assert.match(day, /<i style="left:/, 'the sun on its arc');
  assert.match(day, /Soleil sur/);
  assert.match(day, /class="chip [\w-]* ?warm"[^>]*title="Incidence : 35°"[^>]*>.*?Façade sud/s);
  assert.match(day, /class="chip [\w-]*"[^>]*title="Incidence : 76°"[^>]*>.*?Façade est/s);

  // At night: the arc stays, without the sun on it; no façade lit.
  const night = page(
    [weather, sun({ elevation: -20, azimuth: 300, facades: 'sud=Façade sud\nest=Façade est', sud_incidence: 120, sud_sun: false, est_incidence: 170, est_sun: false })],
    '2026-11-20T21:00:00',
  );
  assert.match(night, /class="sunarc/);
  assert.doesNotMatch(night, /<i style="left:/);
  assert.doesNotMatch(night, /chip [\w-]* ?warm/);

  // No façade declared: no « Soleil sur »; no `dehors:soleil` at all: no height either.
  assert.doesNotMatch(page([weather, sun({ elevation: 18.5, azimuth: 172 }, false)], '2026-11-20T11:45:00'), /Soleil sur/);
  const bare = page([weather], '2026-11-20T11:45:00');
  assert.doesNotMatch(bare, /hauteur|Soleil sur/);
  assert.match(bare, /class="sunarc/, 'the arc needs only sunrise and sunset');
});

test('each sensor chip says which device tells it: « <name> : <value> »', () => {
  const lux = { ...point('illuminance', number(0, 100000), false, 'illuminance'), unit: 'lx' };
  const hum = { ...point('humidity', number(0, 100), false, 'humidity'), unit: '%' };
  const co2 = { ...point('co2', number(0, 5000), false, 'co2'), unit: 'ppm' };
  const room = showRoom([
    { id: 'z2m:sud', native_name: 'Capteur luminosité Sud', points: [lux], values: { illuminance: 118 } },
    { id: 'z2m:est', native_name: 'Capteur luminosité Est', points: [lux], values: { illuminance: 51 } },
    { id: 'z2m:th', native_name: 'Sonde salle', points: [hum, co2], values: { humidity: 62, co2: 1200 } },
    { id: 'z2m:door', native_name: 'Porte atelier', points: [point('contact', { type: 'binary' }, false, 'contact')], values: { contact: true } },
    { id: 'z2m:window', native_name: 'Fenêtre bureau', points: [point('contact', { type: 'binary' }, false, 'contact')], values: { contact: false } },
  ]);
  // Two light sensors in one room: each says its name.
  assert.match(room, /title="Capteur luminosité Sud : 118 lx"/);
  assert.match(room, /title="Capteur luminosité Est : 51 lx"/);
  assert.match(room, /title="Sonde salle : 62 %"/);
  // A graded measure keeps its grade after the value.
  assert.match(room, /title="Sonde salle : 1[\s\u202f]?200 ppm · [^"]+"/);
  assert.doesNotMatch(room, /Éclairement/);
  // A door or a window too.
  assert.match(room, /title="Porte atelier : fermée"/);
  assert.match(room, /title="Fenêtre bureau : ouverte"/);
});


test('a helper is no plug: a switch is a pad saying Oui / Non, a value a chip with its name', () => {
  const helper = { manufacturer: 'Moli', model: 'Aide' };
  const html = showRoom([
    { id: 'maison:lave_linge_en_cours', native_name: 'Lave-linge en cours', points: [point('on', { type: 'binary' }, true, 'on_off')], values: { on: true }, ...helper },
    { id: 'maison:mode', native_name: 'Mode maison', points: [point('state', enumeration('Normal', 'Nuit'))], values: { state: 'Nuit' }, ...helper },
    { id: 'z2m:plug', native_name: 'Prise', points: [point('state', { type: 'binary' }, true, 'on_off')], values: { state: false } },
  ]);
  assert.equal(home.deviceKind(hub.devices['maison:lave_linge_en_cours']), 'helper');
  assert.equal(home.deviceKind(hub.devices['z2m:plug']), 'plug');
  const room = home.roomList()[0];
  assert.deepEqual(room.helpers.map((d) => d.id).sort(), ['maison:lave_linge_en_cours', 'maison:mode']);
  assert.ok(!room.plugs.some((d) => d.id.startsWith('maison:')), 'no helper among the plugs');
  const text = html.replace(/<!--[\s\S]*?-->/g, '');
  assert.match(text, /<b[^>]*>Lave-linge en cours<\/b><small[^>]*>Oui<\/small>/);
  assert.doesNotMatch(text, /Lave-linge en cours<\/b><small[^>]*>Allumée/);
  assert.match(text, /title="Mode maison : Nuit · Aide"/);
  // An icon per kind, never the plug's, and the dashed mark of a helper.
  assert.equal(home.helperIcon(hub.devices['maison:lave_linge_en_cours']), 'toggle');
  assert.equal(home.helperIcon(hub.devices['maison:mode']), 'helper-select');
  assert.ok(text.includes(icons.toggle) && text.includes(icons['helper-select']));
  assert.match(text, /class="pad[^"]*helper[^"]*"[^>]*title="Lave-linge en cours · Aide"/);
  assert.match(text, /class="chip[^"]*helper[^"]*"/);
  assert.doesNotMatch(text.split('Lave-linge en cours')[0].split('<button').pop(), new RegExp(icons.socket.slice(0, 20)));
});
