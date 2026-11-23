// A device added by its address: Moli asks it who it is, a person names it,
// places it and adds it. A typed key goes to Moli's vault and never comes back.
// Authentication is left to the caller (a 403 is `auth`), so a refused write
// is never replayed silently.

const ERRORS = new Set([
  'invalid_host', 'invalid_key', 'invalid_name', 'invalid_room', 'needs_key', 'silent', 'not_recognized',
  'identity_changed', 'duplicate_device', 'busy', 'limit_reached', 'persistence_failed', 'probe_failed',
  'unavailable', 'shutting_down',
]);
const STATUSES = new Set(['recognized', 'needs_key', 'unknown', 'silent']);
const KEYS = new Set(['none', 'empty', 'known', 'given']);
const MAX_KEY = 128;
const MAX_LABEL = 60;
const text = (value, max = 128) => typeof value === 'string' && value.length <= max && !/[\u0000-\u001f\u007f]/.test(value);
const optional = (value, max) => value === undefined || text(value, max);

/** A private IPv4 written plainly (`192.168.1.20`), or null. */
export function privateHost(input) {
  const host = String(input ?? '').trim();
  const pieces = host.split('.');
  if (pieces.length !== 4 || pieces.some((p) => !/^(?:0|[1-9]\d{0,2})$/.test(p))) return null;
  const [a, b] = pieces.map(Number);
  if (pieces.map(Number).some((p) => p > 255)) return null;
  const isPrivate = a === 10 || (a === 172 && b >= 16 && b <= 31) || (a === 192 && b === 168);
  return isPrivate ? host : null;
}

function keyOf(input) {
  const key = String(input ?? '');
  if (!key) return { key: undefined };
  if (key.length > MAX_KEY || /[\u0000-\u001f\u007f]/.test(key)) return { error: 'invalid_key' };
  return { key };
}

export function previewIsValid(data) {
  if (!data || !STATUSES.has(data.status)) return false;
  if (!optional(data.manufacturer, 64)) return false;
  if (data.status !== 'recognized') return true;
  return text(data.profile, 64) && text(data.name) && text(data.model, 32) && text(data.identity)
    && data.identity.length > 0 && KEYS.has(data.key) && (data.mac === undefined || /^[0-9a-f]{12}$/.test(data.mac))
    && optional(data.existing_instance, 64) && optional(data.existing_device, 200);
}

const installedIsValid = (data) => text(data?.instance, 64) && text(data?.device, 200) && typeof data.labelled === 'boolean';

async function post(path, body, validate, fetcher, signal) {
  let response;
  try {
    response = await fetcher(path, {
      method: 'POST',
      headers: { 'content-type': 'application/json', 'x-moli-origin': 'ui' },
      body: JSON.stringify(body),
      ...(signal ? { signal } : {}),
    });
  } catch {
    return { ok: false, error: signal?.aborted ? 'cancelled' : 'network' };
  }
  let data = null;
  try { data = await response.json(); } catch { /* not JSON */ }
  if (response.status === 403) return { ok: false, error: 'auth', status: 403 };
  if (!response.ok) {
    const error = ERRORS.has(data?.error) ? data.error : response.status === 429 ? 'busy' : 'server';
    const result = { ok: false, error, status: response.status };
    if (error === 'duplicate_device' && text(data?.existing_instance, 64)) result.existingInstance = data.existing_instance;
    return result;
  }
  if (response.status !== 200 || !validate(data)) return { ok: false, error: 'response', status: response.status };
  return { ok: true, data };
}

/** Who answers at `host` (with the key typed, when it asked for one). */
export async function probeDevice({ host, key } = {}, fetcher = globalThis.fetch, { signal } = {}) {
  const address = privateHost(host);
  if (!address) return { ok: false, error: 'invalid_host' };
  const typed = keyOf(key);
  if (typed.error) return { ok: false, error: typed.error };
  return post('/api/integrations/devices/probe', { host: address, ...(typed.key ? { key: typed.key } : {}) }, previewIsValid, fetcher, signal);
}

/** Adds the device the person saw (`identity`), named and placed. */
export async function addDevice({ host, identity, name, room, key } = {}, fetcher = globalThis.fetch) {
  const address = privateHost(host);
  if (!address) return { ok: false, error: 'invalid_host' };
  const label = String(name ?? '').trim();
  if (!label || [...label].length > MAX_LABEL) return { ok: false, error: 'invalid_name' };
  const place = String(room ?? '').trim();
  if ([...place].length > MAX_LABEL) return { ok: false, error: 'invalid_room' };
  if (!text(identity) || !identity) return { ok: false, error: 'identity_changed' };
  const typed = keyOf(key);
  if (typed.error) return { ok: false, error: typed.error };
  return post('/api/integrations/devices', {
    host: address, identity, name: label, ...(place ? { room: place } : {}), ...(typed.key ? { key: typed.key } : {}),
  }, installedIsValid, fetcher);
}
