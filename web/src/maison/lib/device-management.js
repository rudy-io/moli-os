// Metadata only: these routes never dispatch a device command. Authentication
// is deliberately left to the caller so a 403 cannot silently replay a write.
const headers = { 'content-type': 'application/json', 'x-moli-origin': 'ui' };
const object = (value) => value !== null && typeof value === 'object' && !Array.isArray(value);
const validId = (id) => typeof id === 'string' && id.trim().length > 0;
const validLabel = (label) => object(label) && Object.keys(label).every((key) =>
  (key === 'name' || key === 'room') && typeof label[key] === 'string' && [...label[key]].length <= 60);

async function put(url, payload, accepts, fetcher) {
  let response;
  try {
    response = await fetcher(url, { method: 'PUT', headers, body: JSON.stringify(payload) });
  } catch {
    return { ok: false, error: 'network' };
  }
  const { status } = response;
  if (!response.ok) return { ok: false, error: status === 403 ? 'auth' : 'server', status };
  try {
    const data = await response.json();
    if (status === 200 && accepts(data)) return { ok: true, data };
  } catch {
    // A successful status without the expected JSON is not a confirmed update.
  }
  return { ok: false, error: 'response', status };
}

export async function saveDeviceLabels(id, label, fetcher = globalThis.fetch) {
  if (!validId(id) || !validLabel(label) || !Object.keys(label).length) {
    return { ok: false, error: 'invalid' };
  }
  return put(`/api/labels/${encodeURIComponent(id)}`, label, validLabel, fetcher);
}

export async function setDeviceHidden(id, hidden, fetcher = globalThis.fetch) {
  if (!validId(id) || typeof hidden !== 'boolean') return { ok: false, error: 'invalid' };
  return put(`/api/devices/${encodeURIComponent(id)}/visibility`, { hidden },
    (data) => object(data) && data.hidden === hidden, fetcher);
}
