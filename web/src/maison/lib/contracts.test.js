import { test } from 'node:test';
import assert from 'node:assert/strict';
import { toMin, hhmm, overlap, freeWindow, toDraft, toContract, problems, inForce, changeMarks } from './contracts.js';

const contract = { from: '2026-01-01', offpeak: ['14:38-17:08', '01:38-07:08'], prices: { HP: 0.221, HC: 0.1747 }, monthly_fee: 17.96 };

test('times read and write back', () => {
  assert.equal(toMin('01:38'), 98);
  assert.equal(toMin('24:00'), 1440);
  assert.equal(toMin('24:01'), null);
  assert.equal(toMin('7h'), null);
  assert.equal(hhmm(98), '01:38');
});

test('a contract makes the round trip, windows in the order of the day', () => {
  const d = toDraft(contract);
  assert.deepEqual(d.windows, [{ from: 878, to: 1028 }, { from: 98, to: 428 }]);
  assert.deepEqual(toContract(d), { from: '2026-01-01', prices: { HC: 0.1747, HP: 0.221 }, offpeak: ['01:38-07:08', '14:38-17:08'], monthly_fee: 17.96 });
  // Without off-peak hours: one price.
  assert.deepEqual(toContract({ ...d, windows: [], base: 0.19, fee: null }), { from: '2026-01-01', prices: { Base: 0.19 } });
});

test('windows overlap, past midnight too', () => {
  assert.equal(overlap([{ from: 1320, to: 120 }, { from: 120, to: 360 }]), null, 'end to end');
  assert.deepEqual(overlap([{ from: 1320, to: 120 }, { from: 60, to: 360 }]), [0, 1]);
});

test('a new window goes where none is', () => {
  assert.deepEqual(freeWindow([{ from: 0, to: 600 }]), { from: 600, to: 720 });
  assert.equal(freeWindow([{ from: 0, to: 1440 }]), null);
});

test('drafts are checked before saving', () => {
  const ok = toDraft(contract);
  assert.deepEqual(problems([ok]), []);
  const kinds = (list) => problems(list).map((p) => p.what);
  assert.deepEqual(kinds([ok, { ...ok }]), ['same_date']);
  assert.deepEqual(kinds([{ ...ok, from: '' }]), ['date']);
  assert.deepEqual(kinds([{ ...ok, windows: [{ from: 60, to: 60 }] }]), ['window']);
  assert.deepEqual(kinds([{ ...ok, windows: [{ from: 60, to: 180 }, { from: 120, to: 240 }] }]), ['overlap']);
  assert.deepEqual(kinds([{ ...ok, hc: '' }]), ['price']);
  assert.deepEqual(kinds([{ ...ok, windows: [], base: null }]), ['price']);
  assert.deepEqual(kinds([{ ...ok, fee: -1 }]), ['fee']);
});

test('the contract in force is the latest begun', () => {
  const list = [{ from: '2027-01-01' }, { from: '2026-01-01' }, { from: '2026-06-01' }];
  assert.equal(inForce(list, '2026-10-25'), 2);
  assert.equal(inForce(list, '2025-12-31'), -1);
});

test('change times too close drop to a second row', () => {
  const marks = changeMarks([{ from: 98, to: 428 }, { from: 878, to: 1028 }]);
  assert.deepEqual(marks.map((m) => [m.text, m.row]), [['01:38', 0], ['07:08', 0], ['14:38', 0], ['17:08', 1]]);
});

test('two windows end to end make one change at each end', () => {
  assert.deepEqual(changeMarks([{ from: 98, to: 480 }, { from: 480, to: 600 }]).map((m) => m.text), ['01:38', '10:00']);
});
