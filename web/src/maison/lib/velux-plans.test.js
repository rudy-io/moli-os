import { test } from 'node:test';
import assert from 'node:assert/strict';
import { grouped, toCloud, same } from './velux-plans.js';

// Three cards VELUX keeps one per room of its app, one of them a room twice.
const close = (id, set) => ({ id, name: 'Fermeture des stores', days: [0, 1, 2, 3, 4, 5, 6], ref: 'sunset', min: 60, set });
const cards = [
  close(3, { 'velux:a': 0, 'velux:b': 0 }),
  { id: 7, name: 'Ouverture', days: [0, 1, 2, 3, 4], ref: 'time', min: 450, set: { 'velux:c': 50 } },
  close(35, { 'velux:c': 0 }),
  close(44, { 'velux:c': 0 }),
  { ...close(50, { 'velux:d': 0 }), off: true },
];

test('the cards of one moment are one line, a paused one apart', () => {
  const lines = grouped(cards);
  assert.equal(lines.length, 3);
  const [shut] = lines;
  assert.deepEqual(shut.at, [0, 2, 3]);
  assert.deepEqual(shut.ids, [3, 35, 44]);
  assert.deepEqual(shut.set, { 'velux:a': 0, 'velux:b': 0, 'velux:c': 0 });
  assert.equal(lines[2].off, true);
});

test('a line goes back with all its cards, each device once', () => {
  const back = toCloud(grouped(cards)[0]);
  assert.equal(back.id, 3);
  assert.deepEqual(back.ids, [35, 44]);
  assert.equal(back.at, undefined);
  assert.deepEqual(Object.keys(back.set), ['velux:a', 'velux:b', 'velux:c']);
  // A new card has no id at all.
  assert.deepEqual(toCloud({ name: 'x', days: [0], ref: 'time', min: 0, set: {}, at: [], ids: [] }), { name: 'x', days: [0], ref: 'time', min: 0, set: {} });
});

test('sent as lines, read back split by room: the same', () => {
  const sent = grouped(cards).map(toCloud);
  assert.ok(same(cards, sent));
  const moved = sent.map((c) => (c.id === 3 ? { ...c, min: 45 } : c));
  assert.ok(!same(cards, moved));
});
