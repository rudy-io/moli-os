import { test } from 'node:test';
import assert from 'node:assert/strict';

const implementation = await import('./somfy-programmes.js').catch(error => {
  if (error.code !== 'ERR_MODULE_NOT_FOUND') throw error;
  return {};
});
const fresh = { scenarios: true, calendar_days: true, calendar_rules: true };
const definition = (id, raw, label = null) => ({ id, label, raw });
const action = (name = 'open', parameters = [], deviceURL = 'io://example.invalid/cover') => ({ deviceURL, commands: [{ name, parameters }] });
const time = (extra = {}) => ({ startActionGroupTimeTrigger: { triggerType: 'TIME', hours: 7, minutes: 30, localActionGroup: { actions: [action()] }, ...extra } });
const day = (triggers = [time()]) => ({ oid: 'day-example', label: 'Example day', dayTimeTriggers: triggers });
const weekly = (extra = {}) => ({ weeklyCalendarRule: { dayMask: 31, period: 1, localCalendarDay: day(), ...extra } });
const catalog = (extra = {}) => ({
  target_labels: [{ device_url: 'io://example.invalid/cover', label: 'Example cover' }],
  collections: {
    scenarios: { data: { definitions: [definition('scene-example', { label: 'Example scene', actions: [action('close')] })] } },
    calendar_days: { data: { definitions: [definition('day-example', day())] } },
    ...extra,
  },
});
function describe(kind, raw, source = catalog(), freshness = fresh) {
  assert.equal(typeof implementation.createProgrammeContext, 'function', 'programme context is implemented');
  assert.equal(typeof implementation.describeProgramme, 'function', 'programme presentation is implemented');
  return implementation.describeProgramme(kind, definition('example', raw), implementation.createProgrammeContext(source, freshness));
}

test('weekday masks preserve Monday-first provider bits and reject invented days', () => {
  for (const [mask, expected] of [[1, [0]], [64, [6]], [31, [0, 1, 2, 3, 4]], [127, [0, 1, 2, 3, 4, 5, 6]], [0, []]]) {
    assert.deepEqual(describe('calendar_rules', weekly({ dayMask: mask })).days, { kind: 'weekdays', weekdays: expected });
  }
  for (const mask of [-1, 128, 1.5, 2 ** 54, null, '31']) {
    const result = describe('calendar_rules', weekly({ dayMask: mask }));
    assert.equal(result.days.kind, 'unknown');
    assert.ok(result.warnings.includes('invalid_days'));
  }
});

test('date bounds distinguish missing, unbounded, valid and invalid calendar dates', () => {
  assert.equal(describe('calendar_rules', weekly()).dates.start, null);
  assert.deepEqual(describe('calendar_rules', weekly({ startYear: -1, startMonth: -1, startDay: -1 })).dates.start, { kind: 'unbounded' });
  assert.deepEqual(describe('calendar_rules', weekly({ startYear: 2024, startMonth: 2, startDay: 29 })).dates.start, { kind: 'date', year: 2024, month: 2, day: 29 });
  for (const fields of [{ startYear: -1, startMonth: 2, startDay: 3 }, { startYear: 2023, startMonth: 2, startDay: 29 }, { startYear: 2026, startMonth: 0, startDay: 4 }, { startYear: 2 ** 54, startMonth: 1, startDay: 1 }]) {
    const result = describe('calendar_rules', weekly(fields));
    assert.deepEqual(result.dates.start, { kind: 'unknown' });
    assert.ok(result.warnings.includes('invalid_date'));
  }
});

test('specific-day and daily rules expose their declared days without guessing weekly periods', () => {
  assert.deepEqual(describe('calendar_rules', { specificDayCalendarRule: { year: 2026, month: 11, day: 11, localCalendarDay: day() } }).days,
    { kind: 'specific', date: { kind: 'date', year: 2026, month: 11, day: 11 } });
  const daily = describe('calendar_rules', { dailyCalendarRule: { period: 2, localCalendarDay: day() } });
  assert.deepEqual(daily.days, { kind: 'daily' });
  assert.deepEqual(daily.period, { kind: 'days', value: 2 });
  const repeating = describe('calendar_rules', weekly({ period: 2 }));
  assert.deepEqual(repeating.period, { kind: 'declared', value: 2 });
  assert.ok(repeating.warnings.includes('period'));
  assert.equal(describe('calendar_rules', weekly({ period: 2 ** 54 })).period.kind, 'unknown');
});

test('clock times are validated and solar times are never converted to guessed clock hours', () => {
  const result = describe('calendar_days', day([time({ hours: 0, minutes: 5 }), time({ triggerType: 'DAWN', hours: 99, dawnOffset: -15 }), time({ triggerType: 'DUSK', duskOffset: 0 })]));
  assert.deepEqual(result.triggers[0].time, { kind: 'time', hours: 0, minutes: 5 });
  assert.deepEqual(result.triggers[1].time, { kind: 'dawn', offsetNative: -15 });
  assert.ok(result.triggers[1].warnings.includes('solar_offset'));
  assert.deepEqual(result.triggers[2].time, { kind: 'dusk', offsetNative: 0 });
  for (const fields of [{ hours: 24 }, { minutes: 60 }, { hours: '7' }, { hours: 2 ** 54 }]) {
    const trigger = describe('calendar_days', day([time(fields)])).triggers[0];
    assert.equal(trigger.time.kind, 'unknown');
    assert.ok(trigger.warnings.includes('invalid_time'));
  }
  const invalidOffset = describe('calendar_days', day([time({ triggerType: 'DAWN', dawnOffset: 2 ** 54 })])).triggers[0];
  assert.equal(invalidOffset.time.offsetNative, undefined);
  assert.ok(invalidOffset.warnings.includes('unsafe_number'));
});

test('native activation flags stay declared flags and finalRule never activates anything', () => {
  const result = describe('calendar_rules', weekly({ finalRule: true, localCalendarDay: day([time({ enabled: false })]) }));
  assert.equal(result.enabled, null);
  assert.equal(result.finalRule, true);
  assert.equal(result.triggers[0].enabled, false);
  assert.equal(describe('calendar_rules', weekly({ enabled: true })).enabled, true);
  assert.equal(describe('calendar_rules', weekly({ enabled: 'true' })).enabled, null);
});

test('local calendar days win over simultaneous references while the conflict remains visible', () => {
  const raw = weekly({ localCalendarDay: day([time({ hours: 19 })]), calendarDayOID: 'day-example' });
  const result = describe('calendar_rules', raw);
  assert.equal(result.triggers[0].time.hours, 19);
  assert.ok(result.warnings.includes('conflicting_reference'));
  delete raw.weeklyCalendarRule.localCalendarDay;
  assert.equal(describe('calendar_rules', raw).triggers[0].time.hours, 7);
  raw.weeklyCalendarRule.calendarDayOID = 'absent';
  assert.ok(describe('calendar_rules', raw).warnings.includes('missing_reference'));
});

test('inline action groups and referenced scenes share the same ordered presentation', () => {
  const result = describe('calendar_days', day([
    { startActionGroupTimeTrigger: { triggerType: 'TIME', hours: 8, minutes: 0, actionGroupOID: 'scene-example' } },
    time({ localActionGroup: { label: 'Inline group', actions: [action('up'), action('setClosure', [75]), action('down')] } }),
  ]));
  assert.equal(result.triggers[0].groupLabel, 'Example scene');
  assert.equal(result.triggers[0].actions[0].commands[0].kind, 'close');
  assert.equal(result.triggers[1].groupLabel, 'Inline group');
  assert.deepEqual(result.triggers[1].actions.map(a => a.commands[0].kind), ['up', 'set_closure', 'down']);
  assert.equal(result.triggers[1].actions[1].commands[0].value, 75);
});

test('duplicate ids cannot silently pick a scenario or calendar day', () => {
  const data = catalog({ scenarios: { data: { definitions: [definition('same', { actions: [action()] }), definition('same', { actions: [action('close')] })] } } });
  const result = describe('calendar_days', day([time({ localActionGroup: undefined, actionGroupOID: 'same' })]), data);
  assert.equal(result.triggers[0].actions.length, 0);
  assert.ok(result.triggers[0].warnings.includes('ambiguous_reference'));
  const ambiguousDay = catalog({ calendar_days: { data: { definitions: [definition('same', day()), definition('same', day())] } } });
  assert.ok(describe('calendar_rules', weekly({ localCalendarDay: undefined, calendarDayOID: 'same' }), ambiguousDay).warnings.includes('ambiguous_reference'));
});

test('historical referenced collections remain qualified even when the rule itself is current', () => {
  const ref = weekly({ localCalendarDay: undefined, calendarDayOID: 'day-example' });
  const result = describe('calendar_rules', ref, catalog(), { ...fresh, calendar_days: false });
  assert.equal(result.triggers.length, 1);
  assert.ok(result.warnings.includes('referenced_history'));
  const scene = describe('calendar_days', day([time({ localActionGroup: undefined, actionGroupOID: 'scene-example' })]), catalog(), { ...fresh, scenarios: false });
  assert.ok(scene.triggers[0].warnings.includes('referenced_history'));
});

test('target labels require exact URLs and unsafe commands are kept uninterpreted', () => {
  const data = catalog();
  data.target_labels[0].label = '<img src=x onerror=evil()>';
  const result = describe('scenarios', { actions: [
    action('open'), action('open', ['unexpected']), action('setClosure', [101]), action('setClosure', [2 ** 54]), action('custom', [7, 'value']),
    action('close', [], 'io://example.invalid/cover-other'),
  ] }, data);
  assert.equal(result.actions[0].target.label, '<img src=x onerror=evil()>');
  assert.equal(result.actions[0].target.known, true);
  assert.deepEqual(result.actions.slice(1, 5).map(a => a.commands[0].kind), ['unknown', 'unknown', 'unknown', 'unknown']);
  assert.equal(result.actions[3].commands[0].parametersPreview, null);
  assert.equal(result.actions[4].commands[0].parametersPreview, '[7,"value"]');
  assert.equal(result.actions[5].target.known, false);
  assert.equal(result.actions[5].target.label, null);
});

test('conflicting target names do not invent an authoritative device name', () => {
  const data = catalog();
  data.target_labels.push({ device_url: 'io://example.invalid/cover', label: 'Different name' });
  const result = describe('scenarios', { actions: [action()] }, data);
  assert.equal(result.actions[0].target.known, false);
  assert.ok(result.warnings.includes('unknown_target'));
});

test('unknown variants and malformed lists do not become successful empty programmes', () => {
  const unknown = describe('calendar_rules', { futureCalendarRule: { localCalendarDay: day() } });
  assert.ok(unknown.warnings.includes('unsupported'));
  assert.equal(unknown.triggers.length, 0);
  assert.ok(describe('scenarios', { actions: {} }).warnings.includes('invalid_actions'));
  const malformed = describe('scenarios', { actions: [{ deviceURL: 'io://example.invalid/cover', commands: {} }] });
  assert.ok(malformed.warnings.includes('invalid_commands'));
});

test('presentation is bounded and reports omitted triggers, actions and commands', () => {
  const commands = Array.from({ length: 25 }, () => ({ name: 'open', parameters: [] }));
  const actions = Array.from({ length: 150 }, () => ({ deviceURL: 'io://example.invalid/cover', commands }));
  const result = describe('calendar_days', day(Array.from({ length: 90 }, () => time({ localActionGroup: { actions } }))));
  assert.equal(result.totalTriggers, 90);
  assert.equal(result.triggers.length, 64);
  assert.equal(result.truncated, true);
  assert.equal(result.triggers[0].totalActions, 150);
  assert.equal(result.triggers[0].actions.length, 100);
  assert.equal(result.triggers[0].truncated, true);
  assert.equal(result.triggers[0].actions[0].totalCommands, 25);
  assert.equal(result.triggers[0].actions[0].commands.length, 16);
  assert.ok(result.warnings.includes('truncated'));
});

test('presentation does not mutate its source or follow reference cycles', () => {
  const raw = weekly({ localCalendarDay: undefined, calendarDayOID: 'loop' });
  const data = catalog({ calendar_days: { data: { definitions: [definition('loop', { calendarDayOID: 'loop' })] } } });
  const before = JSON.stringify({ raw, data });
  const result = describe('calendar_rules', raw, data);
  assert.ok(result.warnings.includes('unsupported'));
  assert.equal(JSON.stringify({ raw, data }), before);
});

test('uninterpreted conditions and notification settings remain visible in the qualification', () => {
  const rule = describe('calendar_rules', weekly({ additionalRestriction: { futureOption: true } }));
  assert.ok(rule.warnings.includes('conditions'));
  const trigger = describe('calendar_days', day([time({ timeWindow: { min: 8 } })]));
  assert.ok(trigger.triggers[0].warnings.includes('conditions'));
  const group = describe('scenarios', { actions: [{ ...action(), executionOptions: { delay: 10 } }], notificationTypeMask: 1 });
  assert.ok(group.warnings.includes('conditions'));
});

test('malformed inline alternatives never silently resolve to a different referenced programme', () => {
  const rule = describe('calendar_rules', weekly({ localCalendarDay: 'invalid', calendarDayOID: 'day-example' }));
  assert.equal(rule.triggers.length, 0);
  assert.ok(rule.warnings.includes('unsupported'));
  const trigger = describe('calendar_days', day([time({ localActionGroup: 'invalid', actionGroupOID: 'scene-example' })])).triggers[0];
  assert.equal(trigger.actions.length, 0);
  assert.ok(trigger.warnings.includes('unsupported'));
});

test('daily cadence must be explicit before it can be presented as every day', () => {
  for (const period of [undefined, null, 0, -1, '1', 1.5, 2 ** 54]) {
    const result = describe('calendar_rules', { dailyCalendarRule: { period, localCalendarDay: day() } });
    assert.deepEqual(result.period, { kind: 'unknown' });
    assert.ok(result.warnings.includes('period'));
  }
  const daily = describe('calendar_rules', { dailyCalendarRule: { period: 1, localCalendarDay: day() } });
  assert.deepEqual(daily.period, { kind: 'days', value: 1 });
  assert.ok(!daily.warnings.includes('period'));
});

test('an unspecified specific day cannot be treated as an unbounded date', () => {
  for (const fields of [{ year: -1, month: -1, day: -1 }, {}]) {
    const result = describe('calendar_rules', { specificDayCalendarRule: { ...fields, localCalendarDay: day() } });
    assert.deepEqual(result.days, { kind: 'specific', date: { kind: 'unknown' } });
    assert.ok(result.warnings.includes('invalid_date'));
  }
  assert.deepEqual(describe('calendar_rules', weekly({ startYear: -1, startMonth: -1, startDay: -1 })).dates.start, { kind: 'unbounded' });
});

test('day and action-group flags survive independently of rule and trigger flags', () => {
  const localDay = { ...day([time({ enabled: true, localActionGroup: { enabled: false, actions: [action()] } })]), enabled: false };
  const local = describe('calendar_rules', weekly({ enabled: true, localCalendarDay: localDay }));
  assert.equal(local.enabled, true);
  assert.equal(local.dayEnabled, false);
  assert.equal(local.triggers[0].enabled, true);
  assert.equal(local.triggers[0].groupEnabled, false);
  const data = catalog({
    calendar_days: { data: { definitions: [definition('day-example', { enabled: false, dayTimeTriggers: [time({ localActionGroup: undefined, actionGroupOID: 'scene-example' })] })] } },
    scenarios: { data: { definitions: [definition('scene-example', { enabled: false, actions: [action()] })] } },
  });
  const referenced = describe('calendar_rules', weekly({ localCalendarDay: undefined, calendarDayOID: 'day-example' }), data);
  assert.equal(referenced.dayEnabled, false);
  assert.equal(referenced.triggers[0].groupEnabled, false);
  const unknown = describe('calendar_days', day());
  assert.equal(unknown.dayEnabled, null);
  assert.equal(unknown.triggers[0].groupEnabled, null);
});

test('every declared solar clock parameter remains qualified, including zero', () => {
  for (const triggerType of ['DAWN', 'DUSK']) {
    for (const value of [0, 1, -1, '0', null, 1.5, 2 ** 54]) {
      for (const field of ['hours', 'minutes']) {
        const trigger = describe('calendar_days', day([time({ triggerType, hours: 0, minutes: 0, [field]: value })])).triggers[0];
        assert.equal(trigger.time.kind, triggerType === 'DAWN' ? 'dawn' : 'dusk');
        assert.ok(trigger.warnings.includes('solar_time'));
        assert.equal(trigger.time.hours, undefined);
        assert.equal(trigger.time.minutes, undefined);
      }
    }
    const trigger = describe('calendar_days', day([{ startActionGroupTimeTrigger: { triggerType, localActionGroup: { actions: [action()] } } }])).triggers[0];
    assert.ok(!trigger.warnings.includes('solar_time'));
  }
});
