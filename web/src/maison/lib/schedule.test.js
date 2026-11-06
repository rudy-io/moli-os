import { test } from 'node:test';
import assert from 'node:assert/strict';
import { DAYS, scheduleSpec, hhmm, minutesOf, normalize, parseDay, formatDay, stepAt, addStep, moveStep, setStepDeg, removeStep, sameDay, suggestMinute, writeDays } from './schedule.js';

const point = (key, kind, write = true) => ({ key, kind, access: { read: true, write }, semantic: 'other' });
const text = { type: 'text' };
const valve = ({ min = 4, max = 35, groups = false, write = true, days = DAYS } = {}) => ({
  online: true,
  points: [
    point('occupied_heating_setpoint', { type: 'numeric', min, max, step: 0.5 }),
    point('system_mode', { type: 'enum', values: ['off', 'auto', 'heat'] }),
    ...days.map((d) => point(`weekly_schedule_${d}`, text, write)),
    ...(groups ? [point('schedule_group_to_edit', { type: 'enum', values: ['1', '2', '3'] })] : []),
  ],
});
const spec = scheduleSpec(valve());

test('the spec is the valve\'s own: setpoint bounds from its contract, steps per day by model', () => {
  assert.deepEqual(spec, { days: DAYS, min: 4, max: 35, step: 0.5, stepBase: 4, maxSteps: 6 });
  const zbt = scheduleSpec(valve({ min: 5, max: 30, groups: true }));
  assert.equal(zbt.maxSteps, 12);
  assert.deepEqual([zbt.min, zbt.max], [5, 30]);
});

test('no spec when no day can be written, or the setpoint has no bounds', () => {
  assert.equal(scheduleSpec(valve({ write: false })), null);
  assert.equal(scheduleSpec(valve({ days: [] })), null);
  assert.equal(scheduleSpec({ online: true, points: [point('weekly_schedule_monday', text)] }), null);
  assert.equal(scheduleSpec({ ...valve(), online: false }), null);
  // Only the days the valve states are editable.
  assert.deepEqual(scheduleSpec(valve({ days: ['monday', 'friday'] })).days, ['monday', 'friday']);
});

test('times read and write as HH:mm, unreadable ones are null', () => {
  assert.equal(hhmm(0), '00:00');
  assert.equal(hhmm(7 * 60 + 5), '07:05');
  assert.equal(hhmm(99999), '23:59');
  assert.equal(minutesOf('7:30'), 450);
  for (const bad of ['24:00', '12:60', 'noon', '', null, '12:5']) assert.equal(minutesOf(bad), null);
});

test('a day round-trips as the valve states it', () => {
  const day = '00:00/17 06:30/20.5 08:00/17 17:00/21 22:00/16.5';
  assert.equal(formatDay(spec, parseDay(spec, day)), day);
  // A minute the valve holds is kept, not moved onto the editor's grid.
  assert.equal(formatDay(spec, parseDay(spec, '00:00/17 06:10/20')), '00:00/17 06:10/20');
});

test('reading is tolerant: junk dropped, order and anchor restored, bounds applied', () => {
  assert.deepEqual(parseDay(spec, '17:00/21 junk 06:00/20 06:00/25 25:00/30 00:00/99'), [
    { min: 0, deg: 35 }, { min: 360, deg: 20 }, { min: 1020, deg: 21 },
  ]);
  assert.deepEqual(parseDay(spec, '06:00/20'), [{ min: 0, deg: 20 }, { min: 360, deg: 20 }]);
  assert.deepEqual(parseDay(spec, ''), [{ min: 0, deg: 4 }]);
  assert.deepEqual(parseDay(spec, null), [{ min: 0, deg: 4 }]);
});

test('a day never holds more steps than the valve takes', () => {
  const many = Array.from({ length: 20 }, (_, i) => `${hhmm(i * 60)}/20`).join(' ');
  assert.equal(parseDay(spec, many).length, 6);
  assert.equal(parseDay(scheduleSpec(valve({ min: 5, max: 30, groups: true })), many).length, 12);
  let day = [{ min: 0, deg: 18 }];
  for (let h = 1; h < 10; h++) day = addStep(spec, day, h * 60);
  assert.equal(day.length, 6);
});

test('setpoints snap to the valve\'s step inside its bounds', () => {
  assert.equal(formatDay(spec, [{ min: 0, deg: 20.3 }]), '00:00/20.5');
  assert.equal(formatDay(spec, [{ min: 0, deg: 2 }]), '00:00/4');
  const zbt = scheduleSpec(valve({ min: 5, max: 30, groups: true }));
  assert.equal(formatDay(zbt, [{ min: 0, deg: 33 }]), '00:00/30');
  assert.equal(formatDay(zbt, [{ min: 0, deg: 4 }]), '00:00/5');
  assert.equal(formatDay(spec, [{ min: 0, deg: 'x' }]), '00:00/4');
});

test('the step in force is the last one at or before the minute', () => {
  const day = parseDay(spec, '00:00/17 06:00/20 22:00/16');
  assert.equal(stepAt(spec, day, 0).deg, 17);
  assert.equal(stepAt(spec, day, 359).deg, 17);
  assert.equal(stepAt(spec, day, 360).deg, 20);
  assert.equal(stepAt(spec, day, 1439).deg, 16);
});

test('a step is added on the grid with the setpoint it lands in', () => {
  const day = parseDay(spec, '00:00/17 06:00/20');
  assert.equal(formatDay(spec, addStep(spec, day, 7 * 60 + 7)), '00:00/17 06:00/20 07:00/20');
  assert.equal(formatDay(spec, addStep(spec, day, 3 * 60)), '00:00/17 03:00/17 06:00/20');
  // 00:00, an occupied minute and a past-midnight minute change nothing.
  assert.equal(formatDay(spec, addStep(spec, day, 0)), formatDay(spec, day));
  assert.equal(formatDay(spec, addStep(spec, day, 360)), formatDay(spec, day));
  assert.equal(formatDay(spec, addStep(spec, day, 1e6)), '00:00/17 06:00/20 23:45/20');
});

test('a step moves, but never the 00:00 one, never onto 00:00 or onto another step', () => {
  const day = parseDay(spec, '00:00/17 06:00/20 18:00/21');
  assert.equal(formatDay(spec, moveStep(spec, day, 1, 6 * 60 + 40)), '00:00/17 06:45/20 18:00/21');
  assert.equal(formatDay(spec, moveStep(spec, day, 1, 19 * 60)), '00:00/17 18:00/21 19:00/20');
  assert.equal(formatDay(spec, moveStep(spec, day, 0, 60)), formatDay(spec, day));
  assert.equal(formatDay(spec, moveStep(spec, day, 1, 0)), formatDay(spec, day));
  assert.equal(formatDay(spec, moveStep(spec, day, 1, 18 * 60)), formatDay(spec, day));
  assert.equal(formatDay(spec, moveStep(spec, day, 9, 60)), formatDay(spec, day));
});

test('a setpoint is changed and a step removed, the 00:00 step staying', () => {
  const day = parseDay(spec, '00:00/17 06:00/20');
  assert.equal(formatDay(spec, setStepDeg(spec, day, 1, 22.2)), '00:00/17 06:00/22');
  assert.equal(formatDay(spec, setStepDeg(spec, day, 5, 22)), formatDay(spec, day));
  assert.equal(formatDay(spec, removeStep(spec, day, 1)), '00:00/17');
  assert.equal(formatDay(spec, removeStep(spec, day, 0)), formatDay(spec, day));
});

test('two days are the same when they make the same text', () => {
  assert.equal(sameDay(spec, parseDay(spec, '00:00/17 06:00/20'), parseDay(spec, '6:00/20  00:00/17')), true);
  assert.equal(sameDay(spec, parseDay(spec, '00:00/17'), parseDay(spec, '00:00/18')), false);
  assert.deepEqual(normalize(spec, undefined), [{ min: 0, deg: 4 }]);
});

test('a suggested step goes in the middle of the longest stretch, on the grid', () => {
  assert.equal(suggestMinute(spec, [{ min: 0, deg: 17 }]), 12 * 60);
  assert.equal(suggestMinute(spec, parseDay(spec, '00:00/17 06:00/20 22:00/16')), 14 * 60);
  assert.equal(suggestMinute(spec, parseDay(spec, '00:00/17 23:45/20')), 12 * 60);
  const full = Array.from({ length: 6 }, (_, i) => ({ min: i * 60, deg: 18 }));
  assert.equal(suggestMinute(spec, full), null);
  // No free grid minute left between two steps a grid apart.
  const tight = scheduleSpec(valve({ min: 5, max: 30, groups: true }));
  const packed = Array.from({ length: 12 }, (_, i) => ({ min: i * 15, deg: 18 }));
  assert.equal(suggestMinute(tight, packed), null);
  const day = parseDay(spec, '00:00/17 00:15/18');
  assert.equal(day.length, 2);
  assert.equal(formatDay(spec, addStep(spec, day, suggestMinute(spec, day))), '00:00/17 00:15/18 12:15/18');
});

test('an emptied or unreadable setpoint changes nothing, it never becomes the valve\'s minimum', () => {
  const day = parseDay(spec, '00:00/17 06:00/20');
  for (const bad of ['', null, undefined, NaN, 'abc', Infinity]) assert.equal(formatDay(spec, setStepDeg(spec, day, 1, bad)), formatDay(spec, day));
  assert.equal(formatDay(spec, setStepDeg(spec, day, 1, 0)), '00:00/17 06:00/4');
});

test('the days are written in turn and the first one not done stops the rest', async () => {
  const sent = [];
  const outcomes = { monday: 'ok', tuesday: 'held', wednesday: 'ok' };
  const write = async (day) => (sent.push(day), outcomes[day]);
  assert.deepEqual(await writeDays(['monday', 'tuesday', 'wednesday'], write), { written: ['monday'], stopped: 'held' });
  assert.deepEqual(sent, ['monday', 'tuesday']);
  assert.deepEqual(await writeDays(['monday'], write), { written: ['monday'], stopped: null });
  assert.deepEqual(await writeDays([], write), { written: [], stopped: null });
  assert.deepEqual(await writeDays(['x'], async () => undefined), { written: [], stopped: 'error' });
  assert.deepEqual(await writeDays(['x', 'monday'], async (d) => (d === 'x' ? 'error' : 'ok')), { written: [], stopped: 'error' });
});
