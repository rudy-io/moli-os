import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { createServer } from 'vite';

// Reuse the app's name/locale helpers without introducing a second formatter.
let vite;
let model;
before(async () => {
  vite = await createServer({ root: fileURLToPath(new URL('../../../', import.meta.url)), server: { middlewareMode: true, watch: null }, logLevel: 'error' });
  model = await vite.ssrLoadModule('/src/maison/lib/airzone.js');
});
after(async () => { await vite?.close(); });

const drivers = [
  { instance: 'native-a', kind: 'airzone', status: { state: 'running' } },
  { instance: 'native-b', kind: 'airzone', status: { state: 'running' } },
  { instance: 'bridge', kind: 'home-assistant', status: { state: 'running' } },
];
function makeDevice(id, values = {}, options = {}) {
  const points = Object.entries(values).map(([key, value]) => ({
    key, label: key, access: { read: true, write: false }, semantic: 'other',
    kind: { type: typeof value === 'number' ? 'numeric' : typeof value === 'boolean' ? 'binary' : 'text' },
  }));
  return {
    id, instance: id.split(':')[0], native_name: id, label: {}, model: 'Local API',
    online: true, members: [], points,
    state: Object.fromEntries(Object.entries(values).map(([key, value]) => [key, { value, ts: 1000 }])),
    ...options,
  };
}
const keys = (devices) => devices.map((d) => d.id);

test('native inclusion joins the driver instance, not names, brands or native-looking IDs', () => {
  const a = makeDevice('native-a:zone', { systemID: 1 });
  const bridge = makeDevice('bridge:gateway_s1_z1', { systemID: 1 }, { manufacturer: 'Airzone', native_name: 'Airzone' });
  const unregistered = makeDevice('airzone-local:zone', { systemID: 1 });
  const inventory = model.airzoneInventory([a, bridge, unregistered], drivers);
  assert.deepEqual(keys(inventory.zones), [a.id]);
  assert.deepEqual([...inventory.byId.keys()], [a.id]);
  assert.equal(model.airzonePresent(drivers), true);
  assert.equal(model.airzonePresent([drivers[2]]), false);
});

test('instances and systems remain separate with deterministic sorting and label precedence', () => {
  const a2 = makeDevice('native-a:zone2', { systemID: 2 });
  const a1 = makeDevice('native-a:zone1', { systemID: 1 }, { native_name: 'Zulu', label: { name: 'Alpha' } });
  const a3 = makeDevice('native-a:zone3', { systemID: 1 }, { native_name: 'Bravo' });
  const b1 = makeDevice('native-b:zone1', { systemID: 1 });
  const inventory = model.airzoneInventory(Object.fromEntries([b1, a2, a3, a1].map((d) => [d.id, d])), drivers);
  assert.deepEqual(inventory.instances.map((i) => i.id), ['native-a', 'native-b']);
  assert.deepEqual(inventory.instances[0].systems.map((s) => s.systemId), [1, 2]);
  assert.deepEqual(keys(inventory.instances[0].systems[0].zones), [a1.id, a3.id]);
  assert.notEqual(inventory.instances[0].systems[0].id, inventory.instances[1].systems[0].id);
});

test('unknown, string, fractional and broadcast system IDs never become system one', () => {
  const zones = [undefined, null, '1', 0, 33, 127, 1.5].map((systemID, i) => makeDevice(`native-a:zone${i}`, { systemID }));
  zones.push(makeDevice('native-a:valid', { systemID: 1 }));
  const systems = model.airzoneInventory(zones, drivers).instances[0].systems;
  assert.deepEqual(systems.map((s) => s.systemId), [1, null]);
  assert.equal(systems[1].zones.length, 7);
});

test('hidden zones disappear from inventory and cannot associate a shared control', () => {
  const visible = makeDevice('native-a:visible', { systemID: 1 });
  const hidden = makeDevice('native-a:hidden', { systemID: 2 });
  const control = makeDevice('native-a:control', { systemID: 2 }, { model: 'Airzone system', members: [hidden.id] });
  const inventory = model.airzoneInventory([visible, hidden, control], drivers, [hidden.id]);
  assert.deepEqual(keys(inventory.zones), [visible.id]);
  assert.equal(inventory.byId.has(hidden.id), false);
  const unresolved = inventory.instances[0].systems.find((s) => s.systemId === null);
  assert.deepEqual(keys(unresolved.controls), [control.id]);
});

test('shared controls keep distinct full-ID member scopes and never cross instances', () => {
  const a = makeDevice('native-a:zone1', { systemID: 1, mode: 'heat' });
  const a2 = makeDevice('native-a:zone2', { systemID: 1, mode: 'cool' });
  const b = makeDevice('native-b:zone1', { systemID: 1 });
  const c1 = makeDevice('native-a:control1', { systemID: 1 }, { model: 'Airzone system', members: [a.id] });
  const c2 = makeDevice('native-a:control2', { systemID: 1 }, { model: 'Airzone system', members: [a2.id] });
  const wrong = makeDevice('native-a:wrong', { systemID: 1 }, { model: 'Airzone system', members: [b.id] });
  const inventory = model.airzoneInventory([a, a2, b, c1, c2, wrong], drivers);
  const instance = inventory.instances[0];
  assert.deepEqual(keys(instance.systems[0].controls), [c1.id, c2.id]);
  assert.deepEqual(keys(instance.systems[1].controls), [wrong.id]);
  assert.equal(model.zoneSummary(a).mode, 'heat');
  assert.equal(model.zoneSummary(a2).mode, 'cool');
  assert.deepEqual(c1.members, [a.id]);
});

test('resource roles use native model hints without dropping unknown resources', () => {
  const make = (suffix, values = {}, options = {}) => makeDevice(`native-a:gateway${suffix}`, values, { model: 'Airzone Local API', ...options });
  const gateway = make('', {}, { model: 'Airzone Webserver' });
  const system = make('_s1_system', { systemID: 1 });
  const iaq = make('_s1_iaq1', { systemID: 1, iaqsensorID: 1 });
  const dhw = make('_dhw', { systemID: 0 });
  const unknown = make('_extra', { temperature: 20, on: true });
  const wrongModel = make('_s1_system_copy', { systemID: 1 }, { model: 'Unrecognized' });
  const inventory = model.airzoneInventory([gateway, system, iaq, dhw, unknown, wrongModel], drivers);
  const instance = inventory.instances[0];
  assert.equal(inventory.zones.length, 0);
  assert.deepEqual(keys(instance.gateways), [gateway.id]);
  assert.deepEqual(keys(instance.systems[0].metadata), [system.id]);
  assert.deepEqual(keys(instance.systems[0].iaq), [iaq.id]);
  assert.deepEqual(keys(instance.dhw), [dhw.id]);
  assert.deepEqual(new Set(keys(instance.other)), new Set([unknown.id, wrongModel.id]));
  assert.equal(inventory.byId.size, 6);
});

test('metadata identities may identify a role but never invent a missing system reading', () => {
  const metadata = makeDevice('native-a:gateway_s2_system', {}, { model: 'Airzone Local API' });
  const impostor = makeDevice('native-a:gateway_s1_system', {}, { model: 'Unrecognized' });
  const inventory = model.airzoneInventory([metadata, impostor], drivers);
  assert.equal(inventory.instances[0].systems[0].systemId, null);
  assert.deepEqual(keys(inventory.instances[0].systems[0].metadata), [metadata.id]);
  assert.deepEqual(keys(inventory.instances[0].other), [impostor.id]);
});

test('offline and unknown availability never present old enabled or demand states as live', () => {
  const d = makeDevice('native-a:zone', { on: true, heat_demand: true, temperature: 19 });
  for (const [online, availability, status] of [[false, 'offline', 'offline'], [null, 'unknown', 'unknown']]) {
    const summary = model.zoneSummary({ ...d, online });
    assert.equal(summary.availability, availability);
    assert.equal(summary.online, false);
    assert.equal(summary.status, status);
    assert.equal(summary.activity, 'unknown');
    assert.equal(summary.temperature.value, 19); // Still available as a clearly old sample.
  }
});

test('enabled, actual demand and unknown activity are independent concepts', () => {
  const summary = (values) => model.zoneSummary(makeDevice('native-a:zone', values));
  assert.equal(summary({ on: true, temperature: 16, target_temperature: 24 }).status, 'on');
  assert.equal(summary({ on: true, temperature: 16, target_temperature: 24 }).activity, 'unknown');
  assert.equal(summary({ on: true, heat_demand: false }).status, 'idle');
  assert.equal(summary({ on: true, heat_demand: false }).activity, 'no_reported_demand');
  assert.equal(summary({ on: true, air_demand: true }).status, 'demand');
  const off = summary({ on: false, floor_demand: true });
  assert.equal(off.status, 'off');
  assert.equal(off.activity, 'demand');
  assert.deepEqual(off.demands, [{ key: 'floor_demand', value: true }]);
  assert.equal(summary({ on: true, invented_demand: true }).activity, 'unknown');
});

test('readings require a matching readable specification and the right value type', () => {
  const d = makeDevice('native-a:zone', { temperature: 0, on: false, mode: 'heat', driver: '' });
  for (const [key, value] of [['temperature', 0], ['on', false], ['mode', 'heat'], ['driver', '']]) {
    assert.equal(model.readingOf(d, key).value, value);
  }
  assert.equal(model.readingOf(d, 'missing'), null);
  assert.equal(model.readingOf({ ...d, points: [] }, 'temperature'), null);
  for (const value of [null, undefined, '20', NaN, Infinity, true]) {
    assert.equal(model.readingOf({ ...d, state: { temperature: { value } } }, 'temperature'), null);
  }
  assert.equal(model.readingOf({ ...d, points: [{ ...d.points[0], access: { read: false, write: true } }] }, 'temperature'), null);
  assert.equal(model.readingOf({ ...d, state: { on: { value: 1 } } }, 'on'), null);
  const enumeration = { ...d, points: [{ key: 'mode', access: { read: true }, kind: { type: 'enum', values: ['heat'] } }] };
  assert.equal(model.readingOf(enumeration, 'mode').value, 'heat');
  assert.equal(model.readingOf({ ...enumeration, state: { mode: { value: 3 } } }, 'mode'), null);
});

test('declared units and individual timestamps are preserved without Celsius assumptions', () => {
  const d = makeDevice('native-a:zone', { temperature: 68, target_temperature: 72, humidity: 0, heatsetpoint: 70, coolsetpoint: 74 });
  d.points.find((p) => p.key === 'temperature').unit = '°F';
  d.points.find((p) => p.key === 'target_temperature').unit = null;
  d.state.temperature.ts = 1200;
  d.state.humidity.ts = 1500;
  d.state.unpublished = { value: 999, ts: 9000 };
  const summary = model.zoneSummary(d);
  assert.equal(summary.temperature.unit, '°F');
  assert.equal(summary.target.unit, null);
  assert.equal(summary.humidity.unit, null);
  assert.equal(summary.humidity.value, 0);
  assert.equal(summary.temperature.ts, 1200);
  assert.equal(summary.heatingTarget.value, 70);
  assert.equal(summary.coolingTarget.value, 74);
  assert.equal(summary.lastSampleAt, 1500);
});

test('invalid booleans and missing diagnostics do not become a healthy device', () => {
  const d = makeDevice('native-a:zone', { on: 'yes', problem: 0, heat_demand: 1 });
  const summary = model.zoneSummary(d);
  assert.equal(summary.on, null);
  assert.equal(summary.problem, null);
  assert.equal(summary.activity, 'unknown');
  assert.equal(model.zoneSummary(makeDevice('native-a:zone', { problem: false })).problem, false);
});

test('every published point belongs to exactly one deterministic section', () => {
  const d = makeDevice('native-a:zone', { temperature: 21, target_temperature: 22, heat_demand: true, minTemp: 15, thermos_firmware: '1.0', problem: false, arbitrary_future_field: 'kept' });
  const sections = model.pointSections(d);
  assert.deepEqual(sections.map((s) => s.id), ['comfort', 'activity', 'limits', 'equipment', 'diagnostics', 'other']);
  assert.deepEqual(sections.flatMap((s) => s.points).map((p) => p.key).sort(), d.points.map((p) => p.key).sort());
  assert.deepEqual(sections.at(-1).points.map((p) => p.key), ['arbitrary_future_field']);
  assert.deepEqual(model.pointSections({ points: [] }), []);
});

test('projection leaves device identities, members, point contracts and source order untouched', () => {
  const zone = makeDevice('native-a:zone', { systemID: 1, temperature: 20, on: true });
  const control = makeDevice('native-a:control', { systemID: 1 }, { model: 'Airzone system', members: [zone.id] });
  const source = [zone, control];
  const before = structuredClone(source);
  const inventory = model.airzoneInventory(source, drivers);
  model.zoneSummary(zone);
  model.pointSections(zone);
  assert.deepEqual(source, before);
  assert.equal(inventory.byId.get(zone.id), zone);
  assert.equal(model.readingOf(zone, 'temperature').point, zone.points.find((p) => p.key === 'temperature'));
});

const withPercent = (d, key = 'battery') => {
  d.points.find((p) => p.key === key).unit = '%';
  return d;
};

test('battery is a reported percentage, including zero, not a binary low-battery flag', () => {
  for (const percent of [0, 1, 20, 53, 100]) {
    const d = withPercent(makeDevice('native-a:zone', { battery: percent, thermos_radio: true }));
    const battery = model.batterySummary(d);
    assert.equal(battery.status, 'level');
    assert.equal(battery.percent, percent);
    assert.equal(battery.reading.unit, '%');
    assert.equal(battery.radio, true);
    assert.equal(model.airzoneAlerts([d], drivers).items.length, 0);
  }
});

test('battery absence distinguishes wired, unreported radio and unknown thermostat metadata', () => {
  assert.equal(model.batterySummary(makeDevice('native-a:zone', { thermos_radio: false })).status, 'wired');
  assert.equal(model.batterySummary(makeDevice('native-a:zone', { thermos_radio: true })).status, 'not_reported');
  const unknown = model.batterySummary(makeDevice('native-a:zone'));
  assert.equal(unknown.status, 'unknown');
  assert.equal(unknown.percent, null);
  for (const value of [-1, 101, 1.5, '25', true]) {
    const d = withPercent(makeDevice('native-a:zone', { battery: value, thermos_radio: true }));
    assert.equal(model.batterySummary(d).percent, null);
  }
  assert.equal(model.batterySummary(makeDevice('native-a:zone', { battery: 50 })).percent, null);
  const old = withPercent(makeDevice('native-a:zone', { battery: 0 }, { online: false }));
  assert.equal(model.batterySummary(old).percent, 0);
  assert.equal(model.batterySummary(old).availability, 'offline');
});

test('explicit low-battery errors are deduplicated without interpreting arbitrary battery words', () => {
  const zone = makeDevice('native-a:gateway_s1_z8', { systemID: 1, problem: true, errors: JSON.stringify([{ Zone: 'Low battery' }, { zone: ' LOW BATTERY ' }, 'Low battery', { warning: 'Low battery' }]), detail: 'Low battery' });
  const control = makeDevice('native-a:gateway_s1_z8_control', { systemID: 1, errors: zone.state.errors.value }, { model: 'Airzone system', members: [zone.id] });
  const result = model.airzoneAlerts([zone, control], drivers);
  assert.equal(result.items.length, 1);
  assert.equal(result.items[0].kind, 'low_battery');
  assert.equal(result.items[0].code, 'low_battery');
  assert.equal(result.items[0].deviceId, zone.id);
  assert.deepEqual(result.diagnostics, { total: 1, reported: 1, unknown: 0 });
  const vague = makeDevice('native-a:zone', { errors: '[{"zone":"battery"}]' });
  assert.equal(model.airzoneAlerts([vague], drivers).items[0].kind, 'unknown_alarm');
});

test('system errors consolidate within their installation and system, while zone errors stay distinct', () => {
  const a1 = makeDevice('native-a:zone1', { systemID: 1, errors: '[{"System":"Error C09"},{"Zone":"Error 3"}]' });
  const a2 = makeDevice('native-a:zone2', { systemID: 1, errors: a1.state.errors.value });
  const a3 = makeDevice('native-a:zone3', { systemID: 2, errors: '[{"system":"Error C09"}]' });
  const b1 = makeDevice('native-b:zone1', { systemID: 1, errors: '[{"system":"Error C09"}]' });
  const result = model.airzoneAlerts([a1, a2, a3, b1], drivers);
  assert.equal(result.items.length, 5);
  const combined = result.items.find((i) => i.scope === 'system' && i.instance === 'native-a' && i.affectedIds.length === 2);
  assert.equal(combined.code, 'C09');
  assert.deepEqual(combined.affectedIds, [a1.id, a2.id]);
  assert.equal(result.items.filter((i) => i.code === '3').length, 2);
});

test('connection and driver failures suppress stale per-device cascades', () => {
  const zones = [1, 2].map((i) => makeDevice(`native-a:zone${i}`, { errors: '[{"Zone":"Low battery"}]' }, { online: false }));
  const disconnected = model.airzoneAlerts(zones, drivers, { connected: false });
  assert.equal(disconnected.current, false);
  assert.deepEqual(disconnected.items.map((i) => i.kind), ['connection']);
  const failed = model.airzoneAlerts(zones, [{ ...drivers[0], status: { state: 'backoff', error: 'private transport detail' } }]);
  assert.deepEqual(failed.items.map((i) => i.kind), ['driver']);
  assert.equal(failed.items[0].code, 'backoff');
  assert.doesNotMatch(JSON.stringify(failed), /private transport detail/);
  const offline = model.airzoneAlerts(zones, drivers);
  assert.deepEqual(offline.items.map((i) => i.kind), ['offline', 'offline']);
});

test('missing and malformed alarm data cannot become an all-clear or crash the view', () => {
  const absent = makeDevice('native-a:zone', { problem: false });
  assert.deepEqual(model.airzoneAlerts([absent], drivers).diagnostics, { total: 1, reported: 0, unknown: 1 });
  const healthy = makeDevice('native-a:zone', { errors: '[]', problem: false });
  assert.deepEqual(model.airzoneAlerts([healthy], drivers).diagnostics, { total: 1, reported: 1, unknown: 0 });
  for (const errors of ['[{"Zone":"Error 3"}', '{}', '[null]', '[{}]', '[' + ' '.repeat(9000) + ']', JSON.stringify(Array(33).fill({ Zone: 'Low battery' }))]) {
    const d = makeDevice('native-a:zone', { errors, problem: true });
    const result = model.airzoneAlerts([d], drivers);
    assert.equal(result.items.length, 1);
    assert.equal(result.items[0].kind, 'unknown_alarm');
    assert.equal(result.diagnostics.unknown, 1);
  }
  const fallback = makeDevice('native-a:zone', { problem: true });
  assert.equal(model.airzoneAlerts([fallback], drivers).items[0].kind, 'problem');
  const contradictory = makeDevice('native-a:zone', { errors: '[]', problem: true });
  const result = model.airzoneAlerts([contradictory], drivers);
  assert.equal(result.items[0].kind, 'problem');
  assert.equal(result.diagnostics.unknown, 1);
});

test('old SSE timestamps do not create stale-reading alerts and cleared warnings disappear', () => {
  const d = makeDevice('native-a:zone', { errors: '[{"Zone":"Window alarm"}]' });
  d.state.errors.ts = 1;
  const active = model.airzoneAlerts([d], drivers);
  assert.deepEqual(active.items.map((i) => [i.kind, i.code]), [['device_warning', 'window']]);
  d.state.errors.value = '[]';
  assert.deepEqual(model.airzoneAlerts([d], drivers).items, []);
  assert.deepEqual(model.airzoneAlerts([d], drivers).diagnostics, { total: 1, reported: 1, unknown: 0 });
});

test('alert source inclusion is native-only and unknown availability is not a reported outage', () => {
  const bridge = makeDevice('bridge:airzone', { errors: '[{"Zone":"Low battery"}]' }, { manufacturer: 'Airzone' });
  const unknown = makeDevice('native-a:zone', { errors: '[]' }, { online: null });
  const gateway = makeDevice('native-a:gateway', {}, { model: 'Airzone Webserver' });
  const result = model.airzoneAlerts([bridge, unknown, gateway], drivers);
  assert.equal(result.items.length, 0);
  assert.deepEqual(result.diagnostics, { total: 1, reported: 0, unknown: 1 });
});

test('alarm projection is stable across device order and never mutates its source', () => {
  const a = makeDevice('native-a:zone1', { systemID: 1, errors: '[{"system":"Error 9"},{"Zone":"Error 3"}]' });
  const b = makeDevice('native-a:zone2', { systemID: 1, errors: '["Error 9",{"code":"Error 9","message":"same event"}]' });
  const before = structuredClone([a, b]);
  const first = model.airzoneAlerts([a, b], drivers);
  const second = model.airzoneAlerts([b, a], drivers);
  assert.deepEqual(first, second);
  assert.deepEqual([a, b], before);
  assert.equal(first.items.length, 2);
  assert.equal(first.items.find((i) => i.code === '9').affectedIds.length, 2);
});

test('unrecognized entries are one diagnostic warning per device, not a duplicate problem cascade', () => {
  const d = makeDevice('native-a:zone', { errors: '[{"Zone":"unknown A"},{"System":"unknown B"},null]', problem: true, detail: 'unknown A; unknown B' });
  const result = model.airzoneAlerts([d], drivers);
  assert.equal(result.items.length, 1);
  assert.equal(result.items[0].kind, 'unknown_alarm');
  assert.equal(result.diagnostics.unknown, 1);
});
