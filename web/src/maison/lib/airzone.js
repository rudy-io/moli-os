import { deviceName } from '../../lib/format.js';
import { locale } from '../../lib/i18n.svelte.js';
import { pointOf } from './capabilities.js';

const DEMANDS = ['heat_demand', 'cold_demand', 'air_demand', 'floor_demand'];
const validId = (id) => Number.isInteger(id) && id >= 1 && id <= 32;
const timestamp = (ts) => Number.isFinite(ts) && ts >= 0 ? ts : null;
const availabilityOf = (device) => device?.online === true ? 'online' : device?.online === false ? 'offline' : 'unknown';
const compare = (a, b) => String(deviceName(a) ?? a.id).localeCompare(String(deviceName(b) ?? b.id), locale())
  || a.id.localeCompare(b.id);

/** Read an advertised point; zero, false and an unset text label are real values. */
export function readingOf(device, key) {
  const point = pointOf(device, key);
  const sample = device?.state?.[key];
  if (point?.access?.read !== true || !sample) return null;
  const value = sample.value;
  const type = point.kind?.type;
  const valid = type === 'numeric' ? Number.isFinite(value)
    : type === 'binary' ? typeof value === 'boolean'
      : ['text', 'enum'].includes(type) && typeof value === 'string';
  if (!valid) return null;
  return { key, value, unit: typeof point.unit === 'string' ? point.unit : null, ts: timestamp(sample.ts), point };
}

function numeric(device, key) {
  const reading = readingOf(device, key);
  return typeof reading?.value === 'number' ? reading : null;
}
function binary(device, key) {
  const value = readingOf(device, key)?.value;
  return typeof value === 'boolean' ? value : null;
}
function systemOf(device) {
  const value = numeric(device, 'systemID')?.value;
  return validId(value) ? value : null;
}

export const airzonePresent = (drivers) => (drivers ?? []).some((d) => d.kind === 'airzone');

// These hints are emitted by the native driver, never accepted from a bridge
// just because its device manufacturer or displayed name happens to be Airzone.
function resourceRole(device) {
  if (device.model === 'Local API') return 'zone';
  if (device.model === 'Airzone system') return 'control';
  if (device.model === 'Airzone Webserver') return 'gateway';
  if (device.model !== 'Airzone Local API') return 'other';
  if (validId(numeric(device, 'iaqsensorID')?.value)) return 'iaq';
  if (numeric(device, 'systemID')?.value === 0) return 'dhw';
  // Metadata may arrive before its first sample. Its stable native role is
  // still useful, but an absent system reading remains in the unknown group.
  if (/_s(?:[1-9]|[12]\d|3[0-2])_system$/.test(device.id)) return 'metadata';
  if (/_s(?:[1-9]|[12]\d|3[0-2])_iaq(?:[1-9]|[12]\d|3[0-2])$/.test(device.id)) return 'iaq';
  if (/_dhw$/.test(device.id)) return 'dhw';
  return 'other';
}

/** Observation-only inventory. Grouping never changes labels, rooms or guard scope. */
export function airzoneInventory(devices, drivers, hidden = []) {
  const native = new Map((drivers ?? []).filter((d) => d.kind === 'airzone').map((d) => [d.instance, d]));
  const omitted = new Set(hidden);
  const included = (Array.isArray(devices) ? devices : Object.values(devices ?? {}))
    .filter((d) => typeof d?.id === 'string' && native.has(d.instance) && !omitted.has(d.id))
    .sort(compare);
  const byId = new Map(included.map((d) => [d.id, d]));
  const zones = included.filter((d) => resourceRole(d) === 'zone');
  const zoneById = new Map(zones.map((d) => [d.id, d]));
  const instances = [...native.entries()].sort(([a], [b]) => a.localeCompare(b)).map(([id, driver]) => {
    const groups = new Map();
    const instance = { id, driver, gateways: [], systems: [], dhw: [], other: [] };
    const group = (systemId) => {
      if (!groups.has(systemId)) groups.set(systemId, {
        id: `${id}/system/${systemId ?? 'unknown'}`, systemId, zones: [], controls: [], metadata: [], iaq: [],
      });
      return groups.get(systemId);
    };
    for (const device of included.filter((d) => d.instance === id)) {
      const role = resourceRole(device);
      if (role === 'gateway') instance.gateways.push(device);
      else if (role === 'dhw') instance.dhw.push(device);
      else if (role === 'other') instance.other.push(device);
      else if (role === 'control') {
        const systemId = systemOf(device);
        const members = (device.members ?? []).map((member) => zoneById.get(member))
          .filter((zone) => zone?.instance === id);
        // A global system number alone cannot make this a universal control.
        // Missing, conflicting or entirely hidden member scopes stay unresolved.
        const belongs = systemId !== null && members.length > 0 && members.every((zone) => systemOf(zone) === systemId);
        group(belongs ? systemId : null).controls.push(device);
      } else {
        group(systemOf(device))[role === 'zone' ? 'zones' : role].push(device);
      }
    }
    instance.systems = [...groups.values()].sort((a, b) => (a.systemId ?? Infinity) - (b.systemId ?? Infinity));
    return instance;
  });
  return { instances, zones, byId };
}

/** Zone enabled and actual demand are independent reported states, not an estimate. */
export function zoneSummary(device) {
  const availability = availabilityOf(device);
  const on = binary(device, 'on');
  const rawMode = readingOf(device, 'mode')?.value;
  const demands = DEMANDS.map((key) => ({ key, value: binary(device, key) })).filter((d) => d.value !== null);
  const activity = availability !== 'online' || !demands.length ? 'unknown'
    : demands.some((d) => d.value) ? 'demand' : 'no_reported_demand';
  const status = availability !== 'online' ? availability
    : on === false ? 'off'
      : activity === 'demand' ? 'demand'
        : on === true ? activity === 'no_reported_demand' ? 'idle' : 'on' : 'unknown';
  const timestamps = (device?.points ?? []).map((p) => readingOf(device, p.key)?.ts).filter((ts) => ts != null);
  return {
    on, mode: typeof rawMode === 'string' ? rawMode : null, online: availability === 'online', availability,
    status, activity, demands,
    temperature: numeric(device, 'temperature'), target: numeric(device, 'target_temperature'),
    heatingTarget: numeric(device, 'heatsetpoint'), coolingTarget: numeric(device, 'coolsetpoint'),
    humidity: numeric(device, 'humidity'), problem: binary(device, 'problem'),
    lastSampleAt: timestamps.length ? Math.max(...timestamps) : null,
  };
}

/** Local API 1.78 defines a battery percentage, not a binary low-battery flag.
 * Absence is expected for wired thermostats. No manufacturer threshold is
 * documented: only an explicit alarm below can assert a low battery. */
export function batterySummary(device) {
  const candidate = numeric(device, 'battery');
  const reading = candidate?.unit === '%' && Number.isInteger(candidate.value)
    && candidate.value >= 0 && candidate.value <= 100 ? candidate : null;
  const radio = binary(device, 'thermos_radio');
  return {
    reading, percent: reading?.value ?? null, radio, availability: availabilityOf(device),
    status: reading ? 'level' : radio === false ? 'wired' : radio === true ? 'not_reported' : 'unknown',
  };
}

const ZONE_ERRORS = new Set(['3', '4', '5', '6', '7', '8', 'IAQ2', 'IAQ3']);
const SYSTEM_ERRORS = new Set(['9', '11', '13', '14', '15', '16', 'C02', 'C09', 'C11', 'IU', 'IAQ1', 'IAQ4']);
// The specification's descriptions and examples use both short names and
// their "alarm" spelling. These are exact aliases, never substring guesses.
const WARNINGS = new Map([
  ['occupancy', 'occupancy'], ['presence alarm', 'occupancy'],
  ['window', 'window'], ['window alarm', 'window'],
  ['anti-freezing', 'anti_freezing'], ['anti-freezing alarm', 'anti_freezing'],
  ['active dew protection', 'active_dew_protection'], ['active dew', 'active_dew'],
  ['zone without thermostat', 'zone_without_thermostat'], ['low battery', 'low_battery'],
]);
const unknownAlarm = (scope = 'device') => ({ kind: 'unknown_alarm', severity: 'warning', scope, code: 'unknown' });

function alarmToken(scope, value) {
  if (typeof value !== 'string' && !Number.isInteger(value)) return unknownAlarm(scope);
  const token = String(value).trim();
  if (!token || token.length > 240) return unknownAlarm(scope);
  const code = token.replace(/^error\s+/i, '').toUpperCase();
  const knownCode = scope === 'system' ? SYSTEM_ERRORS.has(code)
    : scope === 'zone' ? ZONE_ERRORS.has(code) : ZONE_ERRORS.has(code) || SYSTEM_ERRORS.has(code);
  if (knownCode) return {
    kind: 'device_error', severity: 'error',
    scope: scope === 'device' ? ZONE_ERRORS.has(code) ? 'zone' : 'system' : scope, code,
  };
  const warning = scope !== 'system' ? WARNINGS.get(token.toLowerCase()) : null;
  if (!warning) return unknownAlarm(scope);
  return {
    kind: warning === 'low_battery' ? 'low_battery' : 'device_warning',
    scope: scope === 'device' ? 'zone' : scope, code: warning,
    severity: ['low_battery', 'active_dew', 'zone_without_thermostat'].includes(warning) ? 'warning' : 'info',
  };
}

function alarmEntry(entry) {
  if (typeof entry !== 'object' || entry === null) return [alarmToken('device', entry)];
  if (Array.isArray(entry)) return [unknownAlarm()];
  const pairs = Object.entries(entry);
  if (pairs.length === 0 || pairs.length > 8) return [unknownAlarm()];
  const scopes = pairs.filter(([key]) => ['zone', 'system'].includes(key.toLowerCase()));
  if (scopes.length) return scopes.map(([scope, value]) => alarmToken(scope.toLowerCase(), value));
  // An envelope's code and message describe one event, not two warnings.
  const key = ['code', 'error', 'warning', 'message'].find((name) => Object.hasOwn(entry, name));
  return [key ? alarmToken('device', entry[key]) : unknownAlarm()];
}

function reportedAlarms(device) {
  const reading = readingOf(device, 'errors');
  if (!reading) return { reported: false, items: [] };
  const text = reading.value;
  if (typeof text !== 'string' || text.length > 8192) return { reported: false, items: [unknownAlarm()] };
  try {
    const entries = JSON.parse(text);
    if (!Array.isArray(entries) || entries.length > 32) return { reported: false, items: [unknownAlarm()] };
    const items = entries.flatMap(alarmEntry);
    return { reported: items.every((item) => item.kind !== 'unknown_alarm'), items };
  } catch {
    // The native diagnostic text is bounded and may have been truncated.
    // Never parse a partial prefix into a reassuringly incomplete alarm list.
    return { reported: false, items: [unknownAlarm()] };
  }
}

/** Deterministic current alert projection. No clocks, thresholds, commands or
 * notifications. SSE does not refresh timestamps for unchanged readings. */
export function airzoneAlerts(devices, drivers, { connected = true } = {}) {
  const inventory = airzoneInventory(devices, drivers);
  const included = [...inventory.byId.values()];
  const diagnosticDevices = included.filter((d) => resourceRole(d) !== 'control'
    && (['zone', 'metadata', 'dhw', 'iaq'].includes(resourceRole(d)) || pointOf(d, 'errors') || pointOf(d, 'problem')));
  const diagnostics = { total: diagnosticDevices.length, reported: 0, unknown: diagnosticDevices.length };
  const result = new Map();
  const add = (item, affectedIds = []) => {
    const id = JSON.stringify([item.instance, item.scope, item.target, item.kind, item.code]);
    const existing = result.get(id);
    if (existing) existing.affectedIds = [...new Set([...existing.affectedIds, ...affectedIds])].sort();
    else {
      const { target, ...publicItem } = item;
      result.set(id, { id, ...publicItem, affectedIds: [...new Set(affectedIds)].sort() });
    }
  };
  if (connected !== true) {
    if (inventory.instances.length) add({ kind: 'connection', severity: 'warning', instance: null, deviceId: null, scope: 'connection', target: null, code: 'disconnected' }, included.map((d) => d.id));
    return { items: [...result.values()], diagnostics, current: false };
  }
  const unavailable = new Set();
  for (const instance of inventory.instances) {
    const state = instance.driver.status?.state;
    if (['waiting', 'backoff', 'stopped'].includes(state)) {
      unavailable.add(instance.id);
      add({ kind: 'driver', severity: state === 'waiting' ? 'info' : 'warning', instance: instance.id, deviceId: null, scope: 'instance', target: instance.id, code: state }, included.filter((d) => d.instance === instance.id).map((d) => d.id));
    }
  }
  for (const device of included) {
    // A shared-control entity repeats its master zone's alarms and can go
    // offline with a missing member. The physical zones already report both.
    if (unavailable.has(device.instance) || resourceRole(device) === 'control') continue;
    if (device.online === false) {
      add({ kind: 'offline', severity: 'warning', instance: device.instance, deviceId: device.id, scope: 'device', target: device.id, code: 'offline' }, [device.id]);
      continue;
    }
    if (device.online !== true || !diagnosticDevices.includes(device)) continue;
    const alarms = reportedAlarms(device);
    const problem = binary(device, 'problem') === true;
    if (problem && alarms.items.length === 0) {
      alarms.reported = false;
      alarms.items.push({ kind: 'problem', severity: 'warning', scope: 'device', code: 'reported' });
    }
    if (alarms.reported) diagnostics.reported++;
    for (const alarm of alarms.items) {
      const systemId = systemOf(device);
      const scope = alarm.kind === 'unknown_alarm' ? 'device' : alarm.scope;
      // Only a known numeric system identity permits consolidation across
      // devices. An unknown system never absorbs another device's warning.
      const target = scope === 'system' && systemId !== null ? systemId : device.id;
      add({ ...alarm, scope, instance: device.instance, deviceId: device.id, target }, [device.id]);
    }
  }
  diagnostics.unknown -= diagnostics.reported;
  const severity = { error: 0, warning: 1, info: 2 };
  const items = [...result.values()].sort((a, b) => severity[a.severity] - severity[b.severity] || a.id.localeCompare(b.id));
  return { items, diagnostics, current: true };
}

const SECTION_ORDER = ['comfort', 'activity', 'limits', 'equipment', 'diagnostics', 'other'];
const COMFORT = new Set(['temperature', 'target_temperature', 'humidity', 'heatsetpoint', 'coolsetpoint', 'mode', 'fan_speed', 'sleep', 'eco_adapt', 'acs_temp', 'acs_setpoint']);
const ACTIVITY = new Set(['on', ...DEMANDS, 'heatStage', 'coldStage', 'antifreeze', 'aq_mode', 'aq_control', 'aq_value', 'aq_ventilation']);
const DIAGNOSTICS = new Set(['problem', 'detail', 'errors', 'failures_count', 'read_only_status', 'driver', 'interface', 'cloud_connected']);
const EQUIPMENT = new Set(['systemID', 'zoneID', 'iaqsensorID', 'master_zoneID', 'name', 'units', 'type', 'technology', 'mc_connected', 'modes', 'heatStages', 'coldStages', 'ws_type', 'mac']);
function sectionOf(point) {
  const key = point.key;
  if (COMFORT.has(key)) return 'comfort';
  if (ACTIVITY.has(key)) return 'activity';
  if (/(?:min|max)temp/i.test(key) || ['temp_step', 'coldangle', 'heatangle', 'double_sp'].includes(key)) return 'limits';
  if (DIAGNOSTICS.has(key) || key.startsWith('sync_') || /_available$/.test(key)) return 'diagnostics';
  if (EQUIPMENT.has(key) || /(?:firmware|version|thermos_)/.test(key)) return 'equipment';
  return 'other';
}

/** All published fields remain reachable, including future fields we do not classify. */
export function pointSections(device) {
  const sections = new Map(SECTION_ORDER.map((id) => [id, []]));
  for (const point of device?.points ?? []) sections.get(sectionOf(point)).push(point);
  return [...sections.entries()].filter(([, points]) => points.length)
    .map(([id, points]) => ({ id, points: points.sort((a, b) => a.key.localeCompare(b.key)) }));
}
