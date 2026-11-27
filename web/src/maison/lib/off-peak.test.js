import test from 'node:test';
import assert from 'node:assert/strict';
import { offPeakShare } from './off-peak.js';

const summary = (meters, month) => ({ meters, month: { meters: month } });

test('the off-peak share of a house with off-peak and peak meters', () => {
  const s = summary(
    [
      { id: 'hc', name: 'Heures creuses', role: 'grid' },
      { id: 'hp', name: 'Heures pleines', role: 'grid' },
      { id: 'gaz', name: 'Gaz', role: 'gas' },
    ],
    { hc: { kwh: 30 }, hp: { kwh: 70 }, gaz: { kwh: 500 } },
  );
  assert.equal(offPeakShare(s), 30);
});

test('a single price says nothing, rather than « 0 % »', () => {
  const s = summary([{ id: 'linky', name: 'Linky', role: 'grid' }], { linky: { kwh: 300 } });
  assert.equal(offPeakShare(s), null);
});

test('too little this month, or nothing known, says nothing', () => {
  const meters = [
    { id: 'hc', name: 'HC', role: 'grid' },
    { id: 'hp', name: 'HP', role: 'grid' },
  ];
  assert.equal(offPeakShare(summary(meters, { hc: { kwh: 0.2 }, hp: { kwh: 0.3 } })), null);
  assert.equal(offPeakShare(summary(meters, {})), null);
  assert.equal(offPeakShare(null), null);
});
