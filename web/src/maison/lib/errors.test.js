import { test } from 'node:test';
import assert from 'node:assert/strict';
import { orderError } from './errors.js';

test("a driver's refusal is shown in its own words, without the hub's prefix", () => {
  assert.deepEqual(orderError('driver error: désarmer d’abord : un autre mode se choisit alarme désarmée'), {
    text: 'désarmer d’abord : un autre mode se choisit alarme désarmée',
  });
});

test("what the hub refuses in English is said in the house's words", () => {
  assert.deepEqual(orderError('Forbidden'), { key: 'commun.refuse' });
  assert.deepEqual(orderError('mirror house: the order was not sent (the real house acts)'), { key: 'commun.maison_miroir' });
  assert.deepEqual(orderError('driver unavailable'), { key: 'commun.pilote_indisponible' });
  assert.deepEqual(orderError('driver busy'), { key: 'commun.pilote_indisponible' });
  assert.deepEqual(orderError('driver did not answer in time'), { key: 'commun.appareil_injoignable' });
  assert.deepEqual(orderError('point is read-only'), { key: 'commun.non_pilotable' });
});

test('anything else as it is', () => {
  assert.deepEqual(orderError('invalid value: 7'), { text: 'invalid value: 7' });
  assert.deepEqual(orderError(undefined), { text: '' });
});
