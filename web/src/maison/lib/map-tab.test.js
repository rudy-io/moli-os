import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mapUseful } from './map-tab.js';

const person = (id, latitude) => ({ id: `personnes:${id}`, state: latitude === undefined ? {} : { latitude: { value: latitude } } });

test('no phone and nobody located: no map', () => {
  assert.equal(mapUseful([{ kind: 'people' }, { kind: 'z2m' }], { 'personnes:a': person('a'), 'personnes:b': person('b', null) }), false);
  assert.equal(mapUseful(undefined, undefined), false);
  assert.equal(mapUseful([], { x: { id: 'z2m:x', state: { latitude: { value: 48.8 } } } }), false, 'only people count');
});

test('a phone driver or someone located brings the map back', () => {
  assert.equal(mapUseful([{ kind: 'phones' }], {}), true);
  assert.equal(mapUseful([], { 'personnes:a': person('a', 48.85) }), true);
});
