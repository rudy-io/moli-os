import { deviceName } from '../../lib/format.js';
import { zoneSummary } from './airzone.js';

// Filters describe observed data only: activation and a temperature gap are
// not proof of demand, and retained offline readings are never live demand.
function matchesActivity(device, activity, connected, isCurrent) {
  if (activity === 'unavailable') return device.online !== true||!isCurrent(device);
  if (activity === 'demand') return connected && isCurrent(device) && zoneSummary(device).activity === 'demand';
  return true;
}

export function zoneActivityCounts(zones, connected = true, isCurrent = () => true) {
  return {
    all: zones.length,
    demand: zones.filter((d) => matchesActivity(d, 'demand', connected, isCurrent)).length,
    unavailable: zones.filter((d) => matchesActivity(d, 'unavailable', connected, isCurrent)).length,
  };
}

export function filterZoneSystems(systems, { search = '', system = '', activity = 'all', language = 'fr', connected = true, isCurrent = () => true } = {}) {
  const norm = (value) => String(value ?? '').normalize('NFD').replace(/\p{M}/gu, '').trim().toLocaleLowerCase(language);
  const query = norm(search);
  return systems.filter((s) => !system || s.id === system).map((s) => ({
    ...s,
    zones: s.zones.filter((d) => matchesActivity(d, activity, connected, isCurrent) && (!query || norm(
      [deviceName(d), d.label?.room, d.native_room].filter(Boolean).join(' '),
    ).includes(query))),
  })).filter((s) => s.zones.length);
}
