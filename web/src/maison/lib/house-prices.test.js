import { test } from 'node:test';
import assert from 'node:assert/strict';
import { isoDay, priceOk, rowsFrom, rowsOk, pricesFrom, startOf } from './house-prices.js';

const show = (v) => String(v).replace('.', ',');

test('a price is per kWh, with a comma or a dot, never in cents', () => {
  assert.ok(priceOk('0,2016'));
  assert.ok(priceOk(' 0.19 '));
  assert.ok(!priceOk(''));
  assert.ok(!priceOk('20,16'));
  assert.ok(!priceOk('0'));
  assert.ok(!priceOk('abc'));
});

test('an unchanged day keeps its moment, a changed one starts at local midnight', () => {
  const at = new Date(2026, 10, 20, 6, 47).getTime();
  const [row] = rowsFrom([{ since: at, price: 0.19 }], show);
  assert.deepEqual(row, { day: '2026-11-20', since: at, typed: '0,19' });
  assert.equal(startOf(row), at);
  assert.equal(startOf({ ...row, day: '2026-02-01' }), new Date(2026, 1, 1).getTime());
  assert.equal(startOf({ day: '2023-08-01', since: null, typed: '0,2276' }), new Date(2023, 7, 1).getTime());
});

test('the list is sent as the API takes it, only when every row makes sense', () => {
  const rows = [
    { day: '2023-08-01', since: null, typed: '0,2276' },
    { day: '2024-02-01', since: null, typed: '0.2516' },
  ];
  assert.ok(rowsOk(rows));
  assert.deepEqual(pricesFrom(rows), [
    { since: new Date(2023, 7, 1).getTime(), price: 0.2276 },
    { since: new Date(2024, 1, 1).getTime(), price: 0.2516 },
  ]);
  assert.ok(!rowsOk([]), 'an empty list: refused, every cost would go');
  assert.ok(!rowsOk([...rows, { day: '2024-02-01', since: null, typed: '0,25' }]), 'two prices the same day');
  assert.ok(!rowsOk([{ day: '', since: null, typed: '0,2' }]));
  assert.equal(isoDay(new Date(2024, 0, 5, 23, 30).getTime()), '2024-01-05');
});
