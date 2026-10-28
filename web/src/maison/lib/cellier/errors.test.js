import { test } from 'node:test';
import assert from 'node:assert/strict';
import { knownRefusal, isHumanOnly, needsCode } from './errors.js';

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

test('the server\'s « only a person » refusal asks for the code', () => {
  assert.deepEqual(knownRefusal('seule une personne (code) envoie ceci'), { key: 'cellier.code.requis' });
  assert.deepEqual(knownRefusal('only a person (with the code) can send this'), { key: 'cellier.code.requis' });
  assert.equal(isHumanOnly('database: locked'), false);
  assert.equal(isHumanOnly(undefined), false);
});

test('the code is needed once one is set and nobody is recognised', () => {
  assert.equal(needsCode({ pin_configured: true, human: false, trusted: true }), true);
  assert.equal(needsCode({ pin_configured: true, human: true }), false);
  assert.equal(needsCode({ pin_configured: false, human: false }), false);
  assert.equal(needsCode(undefined), false);
});
