import { test } from 'node:test';
import assert from 'node:assert/strict';
import { parseRecipes, changesFrom } from './coffee-recipes.js';

const recipes = JSON.stringify({
  coffee: {
    strength: { value: 7, min: 1, max: 10, step: 1 },
    water: { value: 100, min: 25, max: 240, step: 5, unit: 'ml' },
    temperature: { value: 1, min: 0, max: 2, step: 1 },
  },
});

test('the recipes read, or none', () => {
  assert.equal(parseRecipes(recipes).coffee.water.value, 100);
  assert.equal(parseRecipes(null), null);
  assert.equal(parseRecipes('not json'), null);
});

test('only what differs from the machine leaves', () => {
  const coffee = parseRecipes(recipes).coffee;
  assert.equal(changesFrom(coffee, { strength: 7, water: 100, temperature: 1 }), null);
  assert.deepEqual(changesFrom(coffee, { strength: 5, water: 100, temperature: 2 }), { strength: 5, temperature: 2 });
  assert.equal(changesFrom(null, { water: 120 }), null);
});
