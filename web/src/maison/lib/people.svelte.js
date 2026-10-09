// The household's people, their phones and zones (server: moli-api people
// and whereabouts). Each person's whereabouts are devices `personnes:<id>`,
// live through the hub like any other device.
import { hub, refreshSession } from '../../lib/hub.svelte.js';
import { t } from '../../lib/i18n.svelte.js';

export const people = $state({
  loaded: false,
  me: null,
  owner: false,
  list: [],
  zones: [],
  phones: [],
  error: '',
});

const headers = { 'content-type': 'application/json', 'x-moli-origin': 'ui' };

async function call(method, path, body) {
  const res = await fetch(path, { method, headers, body: body === undefined ? undefined : JSON.stringify(body) });
  const json = await res.json().catch(() => ({}));
  if (!res.ok) throw new Error(json.error ?? t('personnes.erreur', { status: res.status }));
  return json;
}

export async function loadPeople() {
  try {
    const [book, phones] = await Promise.all([call('GET', '/api/people'), call('GET', '/api/mobile/phones').catch(() => ({ phones: [] }))]);
    people.me = book.me;
    people.owner = book.owner;
    people.list = book.people;
    people.zones = book.zones;
    people.phones = phones.phones ?? [];
    people.error = '';
  } catch (e) {
    people.error = e.message;
  } finally {
    people.loaded = true;
  }
}

export async function savePerson(id, draft) {
  const saved = id ? await call('PUT', `/api/people/${encodeURIComponent(id)}`, draft) : await call('POST', '/api/people', draft);
  await loadPeople();
  return saved;
}

export async function removePerson(id) {
  await call('DELETE', `/api/people/${encodeURIComponent(id)}`);
  await loadPeople();
}

/** `{code, expires}`: shown once, to give to the person. */
export const invite = (id) => call('POST', `/api/people/${encodeURIComponent(id)}/invite`);

export async function setPassword(id, password) {
  await call('PUT', `/api/people/${encodeURIComponent(id)}/password`, { password });
}

export async function saveZones(zones) {
  people.zones = await call('PUT', '/api/zones', zones);
}

export async function assignPhone(phone, person) {
  await call('PUT', `/api/mobile/phones/${encodeURIComponent(phone)}`, { person: person || null });
  await loadPeople();
}

/** Signs in with a password (from anywhere). */
export async function signIn(login, password) {
  await call('POST', '/api/session', { login, password });
  await refreshSession();
}

/** An invitation: the person chooses a password and is signed in. */
export async function redeem(code, password) {
  await call('POST', '/api/invitation', { code, password });
  await refreshSession();
}

export async function signOut() {
  await fetch('/api/session', { method: 'DELETE', headers });
  location.reload();
}

/** Where a person is now (their device `personnes:<id>`). */
export function whereabouts(id) {
  const state = hub.devices[`personnes:${id}`]?.state ?? {};
  const v = (k) => state[k]?.value ?? null;
  return {
    home: v('home'),
    zone: v('zone'),
    latitude: v('latitude'),
    longitude: v('longitude'),
    accuracy: v('accuracy'),
    battery: v('battery'),
    since: v('since'),
  };
}

/** A colour for a person: theirs, else one from their id. */
export function colorOf(person) {
  if (person?.color) return person.color;
  const palette = ['#E8B931', '#4F8DF7', '#E5677B', '#3DBE8B', '#A57CF0', '#F08A3E', '#2FB5C7'];
  let h = 0;
  for (const c of person?.id ?? '') h = (h * 31 + c.charCodeAt(0)) >>> 0;
  return palette[h % palette.length];
}

export const ROLES = ['owner', 'member', 'guest'];
