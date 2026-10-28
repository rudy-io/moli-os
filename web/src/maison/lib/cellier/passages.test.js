import { test } from 'node:test';
import assert from 'node:assert/strict';
import { passages, byMonth } from './passages.js';

const MIN = 60_000;
const T0 = Date.UTC(2026, 2, 14, 10, 30, 0);

const line = (over) => ({
  id: 1,
  product: 'milk',
  provider: 'enseigne-x',
  store: 'Ville-A',
  qty: 1,
  price_cents: 100,
  at: T0,
  source: 'ticket',
  basket: null,
  status: 'confirmed',
  receipt: null,
  label: '',
  line_cents: 100,
  discount_cents: 0,
  unit: null,
  ...over,
});

const receipt = (over) => ({
  id: 7,
  status: 'validated',
  draft: { provider: 'enseigne-x', store: 'Ville-A', at: T0, total_cents: 300, lines: [] },
  ...over,
});

test('lines of one ticket make one passage, newest first', () => {
  const purchases = [
    line({ id: 1, receipt: 7, line_cents: 100 }),
    line({ id: 2, receipt: 7, line_cents: 200 }),
    line({ id: 3, receipt: 8, at: T0 + 5 * 24 * 60 * MIN, line_cents: 50 }),
  ];
  const receipts = [receipt(), receipt({ id: 8, draft: { total_cents: 50 } })];
  const out = passages(purchases, receipts);
  assert.equal(out.length, 2);
  assert.equal(out[0].key, 'ticket:8');
  assert.equal(out[1].key, 'ticket:7');
  assert.equal(out[1].kind, 'ticket');
  assert.equal(out[1].total_cents, 300);
  assert.equal(out[1].lines.length, 2);
  assert.equal(out[1].provider, 'enseigne-x');
  assert.equal(out[1].store, 'Ville-A');
  assert.equal(out[1].at, T0);
});

test('printed total only shows when it differs from the lines', () => {
  const purchases = [line({ id: 1, receipt: 7, line_cents: 100 }), line({ id: 2, receipt: 7, line_cents: 200 })];
  const same = passages(purchases, [receipt()])[0];
  assert.equal(same.printed_cents, null);
  const other = passages(purchases, [receipt({ draft: { total_cents: 340, lines: [] } })])[0];
  assert.equal(other.printed_cents, 340);
  const unknown = passages(purchases, [receipt({ draft: { total_cents: null, lines: [] } })])[0];
  assert.equal(unknown.printed_cents, null);
  const missing = passages(purchases, [])[0];
  assert.equal(missing.printed_cents, null);
});

test('drive lines of one basket make one passage', () => {
  const purchases = [
    line({ id: 1, source: 'drive', basket: 4, line_cents: 150, status: 'provisional' }),
    line({ id: 2, source: 'drive', basket: 4, line_cents: 250, at: T0 + MIN }),
    line({ id: 3, source: 'drive', basket: 5, line_cents: 10, at: T0 - 1000 * MIN }),
  ];
  const out = passages(purchases, []);
  assert.equal(out.length, 2);
  assert.equal(out[0].key, 'basket:4');
  assert.equal(out[0].kind, 'drive');
  assert.equal(out[0].total_cents, 400);
  assert.equal(out[0].printed_cents, null);
  assert.equal(out[1].key, 'basket:5');
});

test('a line with both receipt and basket goes to the ticket passage', () => {
  const purchases = [
    line({ id: 1, receipt: 7, basket: 4, source: 'drive', line_cents: 120 }),
    line({ id: 2, basket: 4, source: 'drive', line_cents: 80 }),
  ];
  const out = passages(purchases, [receipt({ draft: { total_cents: 120, lines: [] } })]);
  assert.equal(out.length, 2);
  const ticket = out.find((p) => p.kind === 'ticket');
  assert.equal(ticket.key, 'ticket:7');
  assert.equal(ticket.receipt, 7);
  assert.equal(ticket.total_cents, 120);
  assert.equal(ticket.printed_cents, null);
  const drive = out.find((p) => p.kind === 'drive');
  assert.equal(drive.receipt, null);
  assert.deepEqual(
    drive.lines.map((l) => l.id),
    [2],
  );
});

test('lines without ticket nor basket are grouped by chain, store and minute', () => {
  const purchases = [
    line({ id: 1, source: 'ticket', at: T0 + 5_000 }),
    line({ id: 2, source: 'ticket', at: T0 + 40_000 }),
    line({ id: 3, source: 'ticket', at: T0 + MIN }),
    line({ id: 4, source: 'ticket', at: T0 + 5_000, store: 'Ville-B' }),
    line({ id: 5, source: 'ticket', at: T0 + 5_000, provider: 'enseigne-y' }),
  ];
  const out = passages(purchases, []);
  assert.equal(out.length, 4);
  const first = out.find((p) => p.lines.length === 2);
  assert.equal(first.kind, 'other');
  assert.equal(first.total_cents, 200);
  assert.equal(first.at, T0 + 5_000);
});

test('a line cost falls back to quantity times price, less the discount', () => {
  const purchases = [
    line({ id: 1, receipt: 7, line_cents: undefined, qty: 2, price_cents: 150, discount_cents: 40 }),
    line({ id: 2, receipt: 7, line_cents: null, qty: 0.5, price_cents: 301 }),
  ];
  assert.equal(passages(purchases, [])[0].total_cents, 260 + 151);
});

test('a draft receipt without purchases makes no passage', () => {
  assert.deepEqual(passages([], [receipt({ status: 'draft' })]), []);
});

test('byMonth totals the passages of each month, newest month first', () => {
  const march = line({ id: 1, receipt: 7, line_cents: 100 });
  const march2 = line({ id: 2, receipt: 8, line_cents: 50, at: T0 + 24 * 60 * MIN });
  const april = line({ id: 3, receipt: 9, line_cents: 70, at: Date.UTC(2026, 3, 2, 9, 0, 0) });
  const out = byMonth(passages([march, march2, april], []));
  assert.deepEqual(
    out.map((m) => [m.month, m.total_cents, m.passages.length]),
    [
      ['2026-04', 70, 1],
      ['2026-03', 150, 2],
    ],
  );
});

test('byMonth of nothing is empty', () => {
  assert.deepEqual(byMonth([]), []);
});
