import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  lineCost,
  gapOf,
  parseAmount,
  toCents,
  centsText,
  qtyText,
  draftProblems,
  listMatches,
  pruneItemsDone,
  dots,
  qtyPrice,
  toLocalInput,
  fromLocalInput,
} from './draft.js';

const line = (over) => ({
  label: 'LAIT 1L',
  name: 'Lait',
  qty: 1,
  unit: null,
  price_cents: 100,
  discount_cents: 0,
  line_cents: 100,
  vat: null,
  aisle: null,
  keep: true,
  reason: '',
  product: 'milk',
  new_product: null,
  ...over,
});

test('a line costs quantity times price, rounded, less the discount', () => {
  assert.equal(lineCost(2, 399), 798);
  assert.equal(lineCost(0.532, 1299), 691);
  assert.equal(lineCost(2, 150, 40), 260);
  assert.equal(lineCost(0.5, 301), 151, 'half away from zero');
});

test('the gap is the printed total less every line', () => {
  const draft = { total_cents: 500, lines: [line({ line_cents: 300 }), line({ line_cents: 150, keep: false })] };
  assert.equal(gapOf(draft), 50);
  assert.equal(gapOf({ ...draft, total_cents: null }), null);
});

test('amounts typed in French or English', () => {
  assert.equal(parseAmount('2,58'), 2.58);
  assert.equal(parseAmount(' 2.58 € '), 2.58);
  assert.equal(parseAmount('0,532'), 0.532);
  assert.equal(parseAmount('12'), 12);
  assert.equal(parseAmount(''), null);
  assert.equal(parseAmount('-1'), null);
  assert.equal(parseAmount('abc'), null);
  assert.equal(parseAmount('1,2,3'), null);
  assert.equal(toCents('2,585'), 259);
  assert.equal(toCents('x'), null);
});

test('amounts shown to edit', () => {
  assert.equal(centsText(258, 'fr-FR'), '2,58');
  assert.equal(centsText(123456, 'en'), '1234.56');
  assert.equal(centsText(null), '');
  assert.equal(qtyText(0.532, 'fr-FR'), '0,532');
  assert.equal(qtyText(2, 'en'), '2');
});

test('what stops a validation, kept lines only', () => {
  const draft = {
    lines: [
      line(),
      line({ product: null }),
      line({ product: null, new_product: '  ' }),
      line({ product: null, new_product: 'Pain' }),
      line({ qty: 0 }),
      line({ discount_cents: -5 }),
      line({ product: null, keep: false }),
    ],
  };
  assert.deepEqual(draftProblems(draft), [
    { index: 1, why: 'product' },
    { index: 2, why: 'name' },
    { index: 4, why: 'qty' },
    { index: 5, why: 'price' },
  ]);
});

test('the list lines a receipt may settle share a kept product', () => {
  const draft = { lines: [line(), line({ product: 'eggs', keep: false }), line({ product: null, new_product: 'Pain' })] };
  const items = [
    { id: 1, product: 'milk' },
    { id: 2, product: 'eggs' },
    { id: 3, product: null },
    { id: 4, product: 'milk' },
  ];
  assert.deepEqual(
    listMatches(draft, items).map((i) => i.id),
    [1, 4],
  );
});

test('a list item ticked stays only while a kept line has its product', () => {
  const items = [
    { id: 1, product: 'milk' },
    { id: 2, product: 'eggs' },
    { id: 3, product: 'bread' },
  ];
  const draft = { items_done: [1, 2, 3, 9], lines: [line(), line({ product: 'eggs', keep: false }), line({ product: 'flour' })] };
  assert.deepEqual(pruneItemsDone(draft, items), [1], 'eggs unticked, bread re-pointed to flour, 9 closed elsewhere');
  draft.lines[1].keep = true;
  assert.deepEqual(pruneItemsDone(draft, items), [1, 2]);
});

test('dots join the parts that exist', () => {
  assert.equal(dots(['Ville-A', null, '', 'lundi', false, '3 lignes']), 'Ville-A · lundi · 3 lignes');
  assert.equal(dots([]), '');
});

test('quantity times price, the unit when weighed', () => {
  const money = (c) => `${(c / 100).toFixed(2)} €`;
  assert.equal(qtyPrice({ qty: 1, unit: null, price_cents: 129 }, money, 'en'), '1.29 €');
  assert.equal(qtyPrice({ qty: 2, unit: null, price_cents: 129 }, money, 'en'), '2 × 1.29 €');
  assert.equal(qtyPrice({ qty: 0.532, unit: 'kg', price_cents: 1299 }, money, 'fr-FR'), '0,532 kg × 12.99 €/kg');
});

test('a datetime-local value round-trips to the minute', () => {
  const at = new Date(2026, 9, 28, 10, 30).getTime();
  assert.equal(toLocalInput(at), '2026-10-28T10:30');
  assert.equal(fromLocalInput('2026-10-28T10:30'), at);
  assert.equal(fromLocalInput(''), null);
  assert.equal(toLocalInput(null), '');
});
