import { test } from 'node:test';
import assert from 'node:assert/strict';
import { isAlarm, setupOf, sensorActive, blocking, toneOf, secondsLeft, delay, journal } from './alarm.js';

const device = {
  manufacturer: 'Moli',
  points: [{ key: 'mode' }, { key: 'state' }, { key: 'setup' }],
  state: {
    setup: {
      value: JSON.stringify({
        modes: { night: { exit: 5, entry: 5, trigger: 480 } },
        sensors: [
          { point: 'z2m:porte/contact', name: 'Porte', modes: ['home', 'night'] },
          { point: 'frigate:zone-cour/person', name: 'Cour', modes: ['night'] },
        ],
      }),
    },
  },
};

test('the alarm device and its setup', () => {
  assert.ok(isAlarm(device));
  assert.ok(!isAlarm({ manufacturer: 'Frigate', points: device.points }));
  assert.equal(setupOf(device).sensors.length, 2);
  assert.deepEqual(setupOf({}), { modes: {}, sensors: [] });
});

test('what a sensor sees: an open door, someone in a zone', () => {
  assert.equal(sensorActive('z2m:porte/contact', true), false, 'closed');
  assert.equal(sensorActive('z2m:porte/contact', false), true, 'open');
  assert.equal(sensorActive('frigate:zone-cour/person', 1), true);
  assert.equal(sensorActive('frigate:zone-cour/person', 0), false);
  assert.equal(sensorActive('x:y/on', null), false);
});

test('what would block arming, mode by mode', () => {
  const s = setupOf(device);
  const values = { 'z2m:porte/contact': false, 'frigate:zone-cour/person': 0 };
  assert.deepEqual(blocking(s, 'home', (p) => values[p]), ['Porte']);
  values['z2m:porte/contact'] = true;
  values['frigate:zone-cour/person'] = 2;
  assert.deepEqual(blocking(s, 'home', (p) => values[p]), []);
  assert.deepEqual(blocking(s, 'night', (p) => values[p]), ['Cour']);
});

test('tones, countdowns and delays', () => {
  assert.equal(toneOf('armed_night'), 'armed');
  assert.equal(toneOf('triggered'), 'alert');
  assert.equal(toneOf('disarmed'), 'off');
  assert.equal(secondsLeft(105, 100_200), 5);
  assert.equal(secondsLeft(0, 1), null);
  assert.equal(secondsLeft(90, 100_000), 0);
  assert.deepEqual(delay(480), { n: 8, unit: 'min' });
  assert.deepEqual(delay(20), { n: 20, unit: 's' });
  assert.equal(delay(0), null);
});

test('the journal pairs each event with its cause', () => {
  const j = journal(
    [
      [1000, 'armed'],
      [5000, 'triggered'],
      [9000, 'disarmed'],
    ],
    [
      [4999, 'Cour'],
      [9000, ''],
    ],
  );
  assert.deepEqual(
    j.map((e) => [e.what, e.cause]),
    [
      ['disarmed', ''],
      ['triggered', 'Cour'],
      ['armed', ''],
    ],
  );
});

test('a silent sensor is named in the journal, from the point `silent`', () => {
  const j = journal(
    [
      [1000, 'armed'],
      [70000, 'silent'],
    ],
    [[1000, '']],
    [[69999, 'Porte du garage']],
  );
  assert.deepEqual(
    j.map((e) => [e.what, e.cause]),
    [
      ['silent', 'Porte du garage'],
      ['armed', ''],
    ],
  );
});

import { candidates, draftOf, withoutMode, problems } from './alarm.js';

test('what can be a sensor: doors, presence, camera zones', () => {
  const devices = [
    device,
    { id: 'z2m:porte', points: [{ key: 'contact', semantic: 'contact', label: 'contact' }, { key: 'battery' }] },
    { id: 'frigate:zone-cour', points: [{ key: 'person', label: 'Personnes' }, { key: 'all', label: 'Objets' }] },
    { id: 'hue:lampe', points: [{ key: 'on' }] },
  ];
  const names = { 'z2m:porte': 'Porte atelier', 'frigate:zone-cour': 'Zone cour' };
  const c = candidates(devices, (d) => names[d.id]);
  assert.deepEqual(c, [
    { point: 'z2m:porte/contact', name: 'Porte atelier' },
    { point: 'frigate:zone-cour/person', name: 'Zone cour (Personnes)' },
  ]);
});

test('a draft: a mode turned off leaves the sensors, and what blocks saving', () => {
  const d = draftOf(setupOf(device));
  assert.deepEqual(problems(d), []);
  const without = withoutMode(d, 'night');
  assert.deepEqual(Object.keys(without.modes), []);
  assert.ok(without.sensors.every((s) => !s.modes.includes('night')));
  assert.ok(problems(without).includes('alarme.reglages.aucun_mode'));
  d.modes.night.exit = 4000;
  assert.ok(problems(d).includes('alarme.reglages.delai_invalide'));
});

import { unchanged } from './alarm.js';

test('a draft that changes nothing is told apart, `immediate: false` or not', () => {
  const setup = { modes: { night: { exit: 5, entry: 5, trigger: 480 } }, sensors: [{ point: 'z2m:porte/contact', name: 'Porte', modes: ['night'] }] };
  const draft = { modes: { night: { exit: 5, entry: 5, trigger: 480 } }, sensors: [{ point: 'z2m:porte/contact', name: 'Porte', modes: ['night'], immediate: false }] };
  assert.ok(unchanged(draft, setup));
  draft.sensors[0].immediate = true;
  assert.ok(!unchanged(draft, setup));
  draft.sensors[0].immediate = false;
  draft.modes.night.exit = 30;
  assert.ok(!unchanged(draft, setup));
});
