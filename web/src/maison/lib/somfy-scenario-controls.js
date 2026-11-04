import { canWrite, writableChoices } from './capabilities.js';
// Execution data is issued by the server for one exact catalogue revision.
// A visible label, local device match, or saved preference never grants control.
export function scenarioControl(observation, id, devices = {}, connected = false) {
  const execution = observation?.execution;
  const rows = execution?.scenarios?.filter(row => row.id === id) ?? [];
  if (!id || rows.length !== 1) return { permitted: false, executable: false, reason: 'not_supported' };
  const row = rows[0];
  const separator = typeof row.point === 'string' ? row.point.lastIndexOf('/') : -1;
  const device = separator > 0 ? devices[row.point.slice(0, separator)] : null;
  const key = separator > 0 ? row.point.slice(separator + 1) : '';
  const live = connected && device?.online === true && canWrite(device, key) && writableChoices(device, key).includes(row.value);
  const fresh = observation.freshness?.current === true && observation.freshness?.collections?.scenarios === true;
  const executable = live && execution.read_only === false && fresh
    && row.executable === true && row.permitted === true
    && typeof row.point === 'string' && row.point.length > 0
    && typeof row.value === 'string' && row.value.length > 0;
  return { ...row, executable, reason: execution.read_only !== false ? 'global_read_only'
    : !connected ? 'offline' : !fresh ? 'stale' : row.executable && !live ? (device?.online === true ? 'stale' : 'offline') : row.reason ?? (executable ? null : 'not_supported') };
}

export async function launchScenario(observation, id, devices, connected, command, label) {
  const control = scenarioControl(observation, id, devices, connected);
  if (!control.executable) return 'unavailable';
  return command(control.point, control.value, label);
}
