import { test } from 'node:test';
import assert from 'node:assert/strict';
import { reader, unitReader, mbits, boxIssues } from './box.js';

const values = {
  'box:main/temp_hdd0': 56,
  'box:main/temp_cpu': 60,
  'box:main/fan0_speed': 1900,
  'box:main/raid_degraded': false,
  'box:main/free_space': 55,
  'box:main/rate_down': 1000,
};
const read = reader((device, key) => values[`${device}/${key}`] ?? null);
const devices = {
  'box:main': { points: [{ key: 'rate_down', unit: 'kB/s' }, { key: 'rate_up' }, { key: 'free_space', unit: '%' }] },
};
const unit = unitReader((id) => devices[id] ?? null);
const t = (key, p) => `${key} ${JSON.stringify(p ?? {})}`;

const box = {
  name: 'Box',
  degraded: 'box:main/raid_degraded',
  free: 'box:main/free_space',
  temperatures: [
    { label: 'Disque', point: 'box:main/temp_hdd0', warn: 50, alert: 55 },
    { label: 'CPU', point: 'box:main/temp_cpu', warn: 80, alert: 90 },
  ],
  fans: [{ label: 'Ventilateur', point: 'box:main/fan0_speed', warn: 1800 }],
};

test('a point reads through its device and key', () => {
  assert.equal(read('box:main/temp_cpu'), 60);
  assert.equal(read('nothing'), null);
  assert.equal(read(undefined), null);
});

test('a point says its own unit; none, unknown device or no point: null', () => {
  assert.equal(unit('box:main/rate_down'), 'kB/s');
  assert.equal(unit('box:main/rate_up'), null);
  assert.equal(unit('box:other/rate_down'), null);
  assert.equal(unit('box:main/nothing'), null);
  assert.equal(unit(undefined), null);
});

test('rates become Mbit/s from a known unit only', () => {
  assert.equal(mbits(1000, 'kB/s'), 8);
  assert.equal(mbits(7.03, 'kB/s'), 0.1);
  assert.equal(mbits(50, 'Mbit/s'), 50);
  assert.equal(mbits(2_500_000, 'B/s'), 20);
  assert.equal(mbits(1000, null), null, 'no unit: nothing guessed');
  assert.equal(mbits(1000, '%'), null, 'not a rate');
  assert.equal(mbits(null, 'kB/s'), null);
  assert.equal(mbits(Number.NaN, 'kB/s'), null);
});

test('a hot disk and a fast fan need a look; the rest is fine', () => {
  const issues = boxIssues(box, read, t);
  assert.equal(issues.length, 2);
  assert.match(issues[0], /alerte_chaud.*Disque.*56/);
  assert.match(issues[1], /alerte_ventilateur.*1900/);
});

test('a degraded array and a full disk are said; no box, nothing', () => {
  values['box:main/raid_degraded'] = true;
  values['box:main/free_space'] = 5;
  const issues = boxIssues(box, read, t);
  assert.ok(issues.some((i) => i.includes('alerte_raid')));
  assert.ok(issues.some((i) => i.includes('alerte_plein') && i.includes('95')));
  assert.deepEqual(boxIssues(null, read, t), []);
});

test('a box without a name is called by the word of the catalogue', () => {
  const issues = boxIssues({ degraded: 'box:main/raid_degraded' }, read, t);
  assert.match(issues[0], /alerte_raid.*systeme\.infra\.box\.titre/);
});
