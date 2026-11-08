import { deviceName } from '../../lib/format.js';
import { locale } from '../../lib/i18n.svelte.js';
import { pointOf } from './capabilities.js';

const DEMANDS = ['heat_demand', 'cold_demand', 'air_demand', 'floor_demand'];
const validId = (id) => Number.isInteger(id) && id >= 1 && id <= 32;
const timestamp = (ts) => Number.isFinite(ts) && ts >= 0 ? ts : null;
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
  const availability = device?.online === true ? 'online' : device?.online === false ? 'offline' : 'unknown';
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
  if (DIAGNOSTICS.has(key) || /_available$/.test(key)) return 'diagnostics';
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
