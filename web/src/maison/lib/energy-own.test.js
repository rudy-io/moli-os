import { test } from 'node:test';
import assert from 'node:assert/strict';
import { ownUse } from './energy-own.js';

const bucket = (kwh) => ({ meters: Object.fromEntries(Object.entries(kwh).map(([id, v]) => [id, { kwh: v }])) });

test('neither panels nor a battery: nothing to add', () => {
  assert.equal(ownUse([{ id: 'linky', role: 'grid' }, { id: 'pince', role: 'total' }]), null);
  // Panels without an export meter: what was kept is unknown, as on the server.
  assert.equal(ownUse([{ id: 'pv', role: 'solar' }]), null);
});

test('the panels: what was made and not sent out', () => {
  const own = ownUse([
    { id: 'pv', role: 'solar' },
    { id: 'inj', role: 'export' },
  ]);
  assert.equal(own(bucket({ pv: 5, inj: 2 })), 3);
  // Meters read at different moments: never more out than made.
  assert.equal(own(bucket({ pv: 1, inj: 2 })), 0);
  assert.equal(own(bucket({})), 0);
});

test('a battery: less what it stored, plus what it gave back', () => {
  const meters = [
    { id: 'pv', role: 'solar' },
    { id: 'inj', role: 'export' },
    { id: 'in', role: 'charge' },
    { id: 'out', role: 'discharge' },
  ];
  const own = ownUse(meters);
  // Midday: 10 made, 1 sent out, 4 stored → 5 used of the house's own.
  assert.equal(own(bucket({ pv: 10, inj: 1, in: 4, out: 0 })), 5);
  // Evening: nothing made, 2 given back.
  assert.equal(own(bucket({ pv: 0, inj: 0, in: 0, out: 2 })), 2);
  // A battery charged off-peak, no panels: what it stored was bought, not used.
  const alone = ownUse(meters.slice(2));
  assert.equal(alone(bucket({ in: 4, out: 0 })), -4);
});
