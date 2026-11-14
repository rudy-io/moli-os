import { test } from 'node:test';
import assert from 'node:assert/strict';
import { isOutside, measuresPool, outdoorView } from './outdoor.js';

const dev = (id, ...keys) => ({ id, points: keys.map((key) => ({ key })) });
const room = (name, parts = {}) => ({
  name,
  icon: 'home',
  group: null,
  lights: [],
  plugs: [],
  covers: [],
  impulses: [],
  climates: [],
  media: [],
  sensors: [],
  lit: 0,
  ...parts,
});
const layout = [
  { name: 'Salon', outside: false },
  { name: 'Extérieur', outside: true },
  { name: 'Piscine', outside: true },
];

test('a room is outside when /api/home says so, and only then', () => {
  assert.equal(isOutside(layout, 'Extérieur'), true);
  assert.equal(isOutside(layout, 'Salon'), false);
  assert.equal(isOutside(layout, 'Garage'), false);
  assert.equal(isOutside(undefined, 'Extérieur'), false);
});

test('only the outside rooms show, empty ones left out', () => {
  const lamp = dev('z2m:lamp', 'state');
  const v = outdoorView({
    rooms: [room('Salon', { lights: [dev('z2m:salon', 'state')] }), room('Extérieur', { lights: [lamp] }), room('Piscine')],
    layoutRooms: layout,
    on: (id) => id === 'z2m:lamp',
  });
  assert.deepEqual(v.rooms.map((r) => r.name), ['Extérieur']);
  assert.equal(v.rooms[0].lit, 1);
});

test('the pool is the named one, else the outside device measuring one', () => {
  const probe = dev('iopool:bassin', 'ph', 'orp', 'temperature');
  const rooms = [room('Piscine', { sensors: [probe], plugs: [dev('tuya:pompe', 'switch_1')] })];
  const found = outdoorView({ rooms, layoutRooms: layout });
  assert.equal(found.pool, 'iopool:bassin');
  // On the pool's card, not twice: the pump only when home.json names it.
  assert.deepEqual(found.rooms[0].sensors, []);
  assert.equal(found.rooms[0].plugs.length, 1);
  const named = outdoorView({ rooms, layoutRooms: layout, pool: 'piscine:autre', pump: 'tuya:pompe' });
  assert.equal(named.pool, 'piscine:autre');
  assert.deepEqual(named.rooms[0].plugs, []);
  assert.equal(named.rooms[0].sensors.length, 1);
  assert.equal(measuresPool(dev('z2m:t', 'temperature')), false);
});

test('garden_lights still show, once', () => {
  const v = outdoorView({
    rooms: [room('Extérieur', { lights: [dev('z2m:a', 'state')] })],
    layoutRooms: layout,
    garden: [{ id: 'z2m:a' }, { id: 'z2m:b', name: 'Fond' }, {}],
  });
  assert.deepEqual(v.garden, [{ id: 'z2m:b', name: 'Fond' }]);
});

test('a camera outside is a tile, not a chip, and never twice', () => {
  const cam = { ...dev('reolink:allee'), camera: true };
  const other = { ...dev('reolink:jardin'), camera: true };
  const v = outdoorView({
    rooms: [room('Extérieur', { sensors: [cam, other, dev('z2m:t', 'temperature')] })],
    layoutRooms: layout,
    cams: [{ id: 'reolink:allee', name: 'Allée' }],
  });
  assert.deepEqual(v.cams, [{ id: 'reolink:jardin' }]);
  assert.deepEqual(v.rooms[0].sensors.map((d) => d.id), ['z2m:t']);
});

test('a device another card shows is not repeated', () => {
  const probe = dev('z2m:sonde', 'temperature', 'humidity');
  const v = outdoorView({
    rooms: [room('Extérieur', { sensors: [probe], lights: [dev('z2m:a', 'state')] })],
    layoutRooms: layout,
    shown: ['z2m:sonde'],
  });
  assert.deepEqual(v.rooms[0].sensors, []);
  assert.equal(v.rooms[0].lights.length, 1);
});
