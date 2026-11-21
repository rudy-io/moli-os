import { test } from 'node:test';
import assert from 'node:assert/strict';
import { dayOf, at, timeAt, bars, playable, windowFrom, windowOf, listed, labelsOf, hours, outline, readable, latest, WINDOW, extract, extractName, recordedIn, toField, fromField, EXTRACT_MAX } from './review.js';

test('a day, local, from midnight to midnight', () => {
  const noon = new Date(2026, 10, 15, 12, 30).getTime() / 1000;
  const [start, end] = dayOf(noon);
  assert.equal(start, new Date(2026, 10, 15).getTime() / 1000);
  assert.equal(end, new Date(2026, 10, 16).getTime() / 1000);
});

test('times and places on the frieze', () => {
  assert.equal(at(150, 100, 200), 0.5);
  assert.equal(at(50, 100, 200), 0);
  assert.equal(at(250, 100, 200), 1);
  assert.equal(timeAt(0.25, 100, 200), 125);
  assert.equal(timeAt(-1, 100, 200), 100);
});

test('recorded stretches become bars, the close ones joined', () => {
  const rec = [
    { start: 150, end: 160 },
    { start: 100, end: 110 },
    { start: 110.05, end: 120 },
    { start: 190, end: 260 },
  ];
  const b = bars(rec, 100, 200, 0.001);
  assert.equal(b.length, 3);
  assert.deepEqual(b[0], { left: 0, width: 0.2 });
  assert.ok(Math.abs(b[1].left - 0.5) < 1e-9);
  assert.ok(Math.abs(b[2].left + b[2].width - 1) < 1e-9, 'clipped at the end of the frieze');
  assert.deepEqual(bars([{ start: 10, end: 20 }], 100, 200), []);
});

test('a press on a hole plays the next recording', () => {
  const rec = [
    { start: 100, end: 110 },
    { start: 150, end: 160 },
  ];
  assert.equal(playable(105, rec), 105);
  assert.equal(playable(120, rec), 150);
  assert.equal(playable(170, rec), null);
});

test('the video fetched: a window, never past now', () => {
  assert.deepEqual(windowFrom(1000.7, 5000), { start: 1000, end: 1000 + WINDOW });
  assert.deepEqual(windowFrom(1000, 1100), { start: 1000, end: 1098 });
  assert.equal(windowFrom(1000, 1002), null);
  assert.deepEqual(windowOf({ start: 1000, end: 1020 }, 5000), { start: 996, end: 1024 });
  assert.deepEqual(windowOf({ start: 1000, end: null }, 1030), { start: 996, end: 1028 });
});

test('the list: newest first, filtered by label', () => {
  const items = [
    { id: 'a', start: 1, labels: ['person'] },
    { id: 'b', start: 3, labels: ['car'] },
    { id: 'c', start: 2, labels: ['person', 'car'] },
  ];
  assert.deepEqual(
    listed(items).map((i) => i.id),
    ['b', 'c', 'a'],
  );
  assert.deepEqual(
    listed(items, 'person').map((i) => i.id),
    ['c', 'a'],
  );
  assert.deepEqual(labelsOf(items), ['car', 'person']);
});

test('hour marks every three hours', () => {
  const [start, end] = dayOf(new Date(2026, 10, 15, 12).getTime() / 1000);
  const h = hours(start, end);
  assert.deepEqual(
    h.map((m) => m.hour),
    [0, 3, 6, 9, 12, 15, 18, 21, 0],
  );
  assert.equal(h[4].fraction, 0.5);
});

test('zones drawn on the picture, names read', () => {
  const zone = {
    points: [
      [0, 1],
      [0.5, 0.25],
      [1, 1],
    ],
  };
  assert.equal(outline(zone, 1600, 900), '0.0,900.0 800.0,225.0 1600.0,900.0');
  assert.equal(readable('cour_interieure'), 'cour interieure');
});

test('the newest items of every camera, mixed', () => {
  const byCamera = [
    { camera: 'a', items: [{ id: 1, start: 10 }, { id: 2, start: 30 }] },
    { camera: 'b', items: [{ id: 3, start: 20 }] },
  ];
  assert.deepEqual(
    latest(byCamera).map((x) => `${x.camera}${x.item.id}`),
    ['a2', 'b3', 'a1'],
  );
  assert.equal(latest(byCamera, 2).length, 2);
});

test('an extract is whole seconds, recorded already, never empty nor over an hour', () => {
  assert.deepEqual(extract(100.4, 160.2, 1000), { start: 100, end: 161 });
  assert.deepEqual(extract(990, 1100, 1000), { start: 990, end: 998 });
  assert.equal(extract(200, 200.5, 1000), null);
  assert.equal(extract(300, 200, 1000), null);
  assert.equal(extract(0, EXTRACT_MAX + 1, 1e9), null);
  assert.deepEqual(extract(0, EXTRACT_MAX, 1e9), { start: 0, end: EXTRACT_MAX });
});

test('an extract is named after its camera and its local time', () => {
  const t = new Date(2026, 10, 20, 11, 21, 5).getTime() / 1000;
  assert.equal(extractName('Entrée', t), 'entree-2026-11-20-11h21m05.mp4');
  assert.equal(extractName('Allée du portail !', t), 'allee-du-portail-2026-11-20-11h21m05.mp4');
  assert.equal(extractName('', t), 'camera-2026-11-20-11h21m05.mp4');
});

test('an extract needs something recorded in it', () => {
  const recorded = [{ start: 100, end: 200 }, { start: 500, end: 600 }];
  assert.equal(recordedIn(150, 170, recorded), true);
  assert.equal(recordedIn(190, 510, recorded), true);
  assert.equal(recordedIn(250, 400, recorded), false);
  assert.equal(recordedIn(250, 400, []), true);
});

test('a moment goes to a date-and-time field and back, in local time', () => {
  const t = new Date(2026, 10, 20, 9, 5, 7).getTime() / 1000;
  assert.equal(toField(t), '2026-11-20T09:05:07');
  assert.equal(fromField('2026-11-20T09:05:07'), t);
  assert.equal(fromField('2026-11-20T09:05'), t - 7);
  assert.equal(fromField(''), null);
  assert.equal(fromField('hier'), null);
});
