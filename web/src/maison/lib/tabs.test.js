import { test } from 'node:test';
import assert from 'node:assert/strict';
import { printersPresent, camerasUseful } from './tabs.js';

const point = (key, semantic = 'other') => ({ key, semantic, kind: { type: 'binary' }, access: { read: true, write: false } });
const dev = (id, points, extra = {}) => ({ id, instance: id.split(':')[0], points, label: {}, ...extra });
const house = (...list) => Object.fromEntries(list.map((d) => [d.id, d]));

// A house like one with only plugs, smoke detectors, a meter and thermometers.
const quiet = house(
  dev('z2m:smoke', [point('smoke', 'smoke'), point('battery', 'battery')]),
  dev('meross:plug', [point('on', 'on_off'), point('power', 'power')]),
  dev('matter:meter', [point('temperature', 'temperature')]),
  dev('personnes:a', [point('home', 'occupancy')]),
);
const drivers = [{ instance: 'personnes', kind: 'people' }, { instance: 'z2m', kind: 'z2m' }];

test('no printer, no Impression tab; a printer brings it', () => {
  assert.equal(printersPresent(quiet), false);
  assert.equal(printersPresent(house(dev('bambu:p1', [point('progress')]))), false, 'progress alone is no printer');
  assert.equal(printersPresent(house(dev('klipper:k2', [point('progress'), point('nozzle_temperature')]))), true);
  assert.equal(printersPresent(undefined), false);
});

test('no camera and nothing for the Sécurité card: no Caméras tab', () => {
  assert.equal(camerasUseful(quiet, drivers, {}), false, 'a person at home is no motion sensor');
  assert.equal(camerasUseful(undefined, undefined, undefined), false);
});

test('a camera, a door, a motion sensor or the alarm brings the Caméras tab', () => {
  assert.equal(camerasUseful(house(dev('reolink:door', [point('motion')], { camera: true })), [], {}), true);
  assert.equal(camerasUseful(house(dev('z2m:door', [point('contact', 'contact')])), [], {}), true);
  assert.equal(camerasUseful(house(dev('z2m:pir', [point('occupancy', 'occupancy')])), [], {}), true);
  assert.equal(camerasUseful(house(dev('z2m:radar', [point('presence', 'occupancy')])), [], {}), true);
  assert.equal(camerasUseful(house(dev('tuya:siren', [point('on')], { label: { room: 'Sécurité' } })), [], {}), true);
  assert.equal(camerasUseful({}, [], { doors: [{ id: 'tuya:x', name: 'Entrée' }] }), true);
});

test('turned-off tabs: read, toggled, and never Accueil', async () => {
  const { readHidden, toggleHidden, menuTabs } = await import('./tabs.js');
  assert.deepEqual(readHidden(null), []);
  assert.deepEqual(readHidden('pas du json'), []);
  assert.deepEqual(readHidden('{"plan":true}'), []);
  assert.deepEqual(readHidden('["plan", "", 3, "parler"]'), ['plan', 'parler']);

  let hidden = toggleHidden([], 'plan', false);
  hidden = toggleHidden(hidden, 'dehors', false);
  assert.deepEqual(hidden, ['dehors', 'plan']);
  assert.deepEqual(toggleHidden(hidden, 'plan', true), ['dehors']);
  assert.deepEqual(toggleHidden(hidden, '', false), hidden, 'Accueil stays');

  const tabs = [{ path: '' }, { path: 'plan' }, { path: 'salon' }, { path: 'dehors' }];
  assert.deepEqual(menuTabs(tabs, ['plan', 'dehors', '']).map((x) => x.path), ['', 'salon']);
});
