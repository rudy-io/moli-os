import { test } from 'node:test';
import assert from 'node:assert/strict';
import { knownRefusal } from './errors.js';

test('a receipt gone or validated elsewhere', () => {
  assert.deepEqual(knownRefusal('not found: receipt 12'), { key: 'cellier.ticket.parti' });
  assert.deepEqual(knownRefusal('receipt 12 is validated: it cannot be edited'), { key: 'cellier.ticket.parti' });
  assert.deepEqual(knownRefusal('receipt 12 is validated: it cannot be abandoned'), { key: 'cellier.ticket.parti' });
});

test('a duplicate, a purchase or a product that no longer exists', () => {
  assert.deepEqual(knownRefusal('this receipt looks already recorded: 3'), { key: 'cellier.ticket.doublon_sans_date' });
  assert.deepEqual(knownRefusal('not found: purchase 4'), { key: 'cellier.erreurs.achat' });
  assert.deepEqual(knownRefusal('not found: product pain-complet'), { key: 'cellier.erreurs.produit', params: { id: 'pain-complet' } });
});

test('a line to check, by its number', () => {
  assert.deepEqual(knownRefusal('line 3: a purchase needs a quantity above zero'), { key: 'cellier.erreurs.ligne', params: { n: 3 } });
  assert.deepEqual(knownRefusal('line 2 has both a product and a new product'), { key: 'cellier.erreurs.ligne', params: { n: 2 } });
});

test('anything else is said generically', () => {
  assert.equal(knownRefusal('database: locked'), null);
  assert.equal(knownRefusal(''), null);
  assert.equal(knownRefusal(undefined), null);
});
