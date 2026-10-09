import { setLanguage, t } from './i18n.svelte.js';

// Live model of the hub, fed by /api/events (SSE) and /api/health.

export const hub = $state({
  connected: false,
  /** @type {Record<string, any>} device id → DeviceView */
  devices: {},
  /** @type {any[]} */
  drivers: [],
  /** @type {any[]} most recent first */
  journal: [],
  /** @type {any} */
  stats: null,
  /** point id → timestamp of last change seen live (for the flash) */
  changed: {},
  /** point id → pending value awaiting device confirmation */
  pending: {},
  /** @type {{ id: number, text: string }[]} */
  toasts: [],
  /** @type {any[]} orders held by the guard, waiting for a human */
  approvals: [],
  /** @type {any} protected rooms and quiet hours */
  guard: null,
  /** point whose history is open, or null */
  chart: null,
  /** comparison with Home Assistant open */
  bench: false,
  /** Home Assistant's memory, as last measured on the host */
  haMemory: null,
  /** messages for the people at home (automations), newest first */
  notices: [],
  /** human session (PIN) — what lets this dashboard release guarded orders */
  session: { human: false, pin_configured: false, locked: false, setup: false },
});

const JOURNAL_MAX = 60;

function applySnapshot(snapshot) {
  const devices = {};
  for (const d of snapshot.devices) devices[d.id] = d;
  hub.devices = devices;
  hub.drivers = snapshot.drivers;
  hub.approvals = snapshot.approvals ?? [];
  hub.guard = snapshot.guard ?? null;
}

function splitPoint(point) {
  const i = point.indexOf('/');
  return [point.slice(0, i), point.slice(i + 1)];
}

function applyEvent(e) {
  switch (e.type) {
    case 'device_upserted': {
      const prev = hub.devices[e.device.id];
      hub.devices[e.device.id] = {
        ...e.device,
        label: prev?.label ?? {},
        online: prev?.online ?? null,
        state: prev?.state ?? {},
      };
      break;
    }
    case 'device_removed':
      delete hub.devices[e.device];
      break;
    case 'state': {
      const [id, key] = splitPoint(e.point);
      const device = hub.devices[id];
      if (!device) break;
      device.state[key] = { value: e.value, ts: e.ts };
      hub.changed[e.point] = e.ts;
      // Only the requested value confirms a command; anything else may be
      // an older report still in flight. The timeout in command() is the net.
      if (e.point in hub.pending && hub.pending[e.point] === e.value) delete hub.pending[e.point];
      break;
    }
    case 'availability':
      if (hub.devices[e.device]) hub.devices[e.device].online = e.online;
      break;
    case 'label_changed':
      if (hub.devices[e.device]) hub.devices[e.device].label = e.label;
      break;
    case 'driver_status': {
      const d = hub.drivers.find((x) => x.instance === e.instance);
      if (d) d.status = e.status;
      else hub.drivers.push({ instance: e.instance, kind: '?', status: e.status });
      break;
    }
    case 'journal':
      mergeJournal([e.entry]);
      break;
    case 'approval_requested':
      hub.approvals = [...hub.approvals.filter((a) => a.id !== e.request.id), e.request];
      break;
    case 'approval_resolved':
      hub.approvals = hub.approvals.filter((a) => a.id !== e.id);
      break;
    case 'notice': {
      const id = `${e.notice.ts}-${Math.random()}`;
      hub.notices = [{ ...e.notice, id }, ...hub.notices].slice(0, 4);
      setTimeout(() => (hub.notices = hub.notices.filter((n) => n.id !== id)), 15000);
      break;
    }
  }
}

/** Merge by id, newest first: SSE entries and fetched pages never clobber each other. */
function mergeJournal(entries) {
  const byId = new Map(hub.journal.map((e) => [e.id, e]));
  for (const e of entries) byId.set(e.id, e);
  hub.journal = [...byId.values()].sort((a, b) => b.id - a.id).slice(0, JOURNAL_MAX);
}

function loadJournal() {
  fetch('/api/journal?limit=' + JOURNAL_MAX)
    .then((r) => r.json())
    .then(mergeJournal)
    .catch(() => {});
}

let linked = false;

/** Opens the live link (once: both faces of the dashboard share it). */
export function connect() {
  if (linked) return;
  linked = true;
  const source = new EventSource('/api/events');
  // Every snapshot (first connection, reconnection, catch-up after lag)
  // may follow a gap: reload the journal too.
  source.addEventListener('snapshot', (m) => {
    hub.connected = true;
    applySnapshot(JSON.parse(m.data));
    loadJournal();
  });
  source.addEventListener('event', (m) => applyEvent(JSON.parse(m.data)));
  source.onerror = () => (hub.connected = false); // EventSource reconnects on its own

  const refresh = async () => {
    try {
      const res = await fetch('/api/health');
      if (res.ok) {
        const health = await res.json();
        hub.stats = health.stats;
        hub.guard = health.guard; // quiet hours start and end on their own
      }
    } catch {
      /* next tick */
    }
  };
  refresh();
  setInterval(refresh, 5000);
  refreshSession();

  const refreshBench = () =>
    fetch('/api/bench')
      .then((r) => (r.ok ? r.json() : null))
      .then((b) => (hub.haMemory = b?.ha?.memory_bytes ?? null))
      .catch(() => {});
  refreshBench();
  setInterval(refreshBench, 5 * 60 * 1000);
  watchBuild();
}

// ---- new build ---------------------------------------------------------------------

/** The bundle this page runs (absent under the dev server). */
const BUILD = document.querySelector('script[type="module"][src*="/assets/index-"]')?.getAttribute('src') ?? null;
/** No touch for this long: nobody is using the screen, it can reload. */
const IDLE_RELOAD = 3 * 60 * 1000;

/** A dashboard left open (a wall tablet, a forgotten tab) keeps the code it
 *  loaded, bugs included: when a new build is served, it reloads at the first
 *  quiet moment (hidden, or untouched for a while and nothing being typed). */
function watchBuild() {
  if (!BUILD) return;
  let stale = false;
  let touched = Date.now();
  for (const ev of ['pointerdown', 'keydown', 'wheel', 'touchstart']) {
    addEventListener(ev, () => (touched = Date.now()), { passive: true, capture: true });
  }
  const typing = () => document.activeElement?.matches?.('input, textarea, select, [contenteditable]') ?? false;
  // Words left in a field (a message to the assistant, a name being changed).
  const drafted = () =>
    [...document.querySelectorAll('textarea, input[type="text"], input[type="search"], input:not([type])')].some((e) => e.value.trim());
  const reloadIfQuiet = () => {
    // An edit not saved yet (the plan, an automation, pages being sent) or
    // a draft waits: hidden or not, nobody loses what they were doing.
    if (!stale || document.querySelector('[data-unsaved]') || drafted()) return;
    if (document.visibilityState === 'hidden' || (Date.now() - touched > IDLE_RELOAD && !typing())) location.reload();
  };
  const check = async () => {
    try {
      const res = await fetch('/', { cache: 'no-store' });
      if (!res.ok) return;
      const served = (await res.text()).match(/src="(\/assets\/index-[^"]+\.js)"/)?.[1];
      if (served && served !== BUILD) stale = true;
    } catch {
      /* next round */
    }
    reloadIfQuiet();
  };
  setInterval(check, 5 * 60 * 1000);
  setInterval(reloadIfQuiet, 30 * 1000);
  document.addEventListener('visibilitychange', reloadIfQuiet);
}

function toast(text) {
  const id = Date.now() + Math.random();
  hub.toasts.push({ id, text });
  setTimeout(() => (hub.toasts = hub.toasts.filter((x) => x.id !== id)), 5000);
}

const headers = { 'content-type': 'application/json', 'x-moli-origin': 'ui' };

export async function command(point, value) {
  hub.pending[point] = value;
  try {
    const res = await fetch('/api/command', {
      method: 'POST',
      headers,
      body: JSON.stringify({ point, value }),
    });
    if (!res.ok) throw new Error((await res.json()).error ?? res.statusText);
    if (res.status === 202) {
      // Held by the guard: this dashboard is not (yet) recognized as human.
      delete hub.pending[point];
      const { reason } = await res.json();
      toast(t('systeme.hub.retenu', { reason }));
      return;
    }
    // Confirmation arrives as a state event; give up waiting after a while.
    setTimeout(() => {
      if (hub.pending[point] === value) delete hub.pending[point];
    }, 8000);
  } catch (err) {
    delete hub.pending[point];
    toast(t('systeme.hub.commande_refusee', { message: err.message }));
  }
}

/** A human approves or denies an order the guard held (naming the point
 *  shown on the card, so a decision never lands on another request). */
export async function decide(request, approve) {
  try {
    const res = await fetch(`/api/approvals/${request.id}`, {
      method: 'POST',
      headers,
      body: JSON.stringify({ approve, point: request.point }),
    });
    // Gone: already decided or expired — the card can go.
    if (res.ok || res.status === 410) {
      hub.approvals = hub.approvals.filter((a) => a.id !== request.id);
    }
    if (!res.ok) throw new Error((await res.json()).error ?? res.statusText);
  } catch (err) {
    toast(t('systeme.hub.decision_non_appliquee', { message: err.message }));
  }
}

export async function refreshSession() {
  try {
    const res = await fetch('/api/session');
    if (res.ok) {
      hub.session = await res.json();
      // The house speaks one language: the dashboard follows it.
      await setLanguage(hub.session.language);
    }
  } catch {
    /* keep the last known state */
  }
}

/** Opens a human session with the PIN. Returns an error message or null. */
export async function login(pin) {
  try {
    const res = await fetch('/api/session', { method: 'POST', headers, body: JSON.stringify({ pin }) });
    await refreshSession();
    return res.ok ? null : ((await res.json()).error ?? res.statusText);
  } catch (err) {
    return err.message;
  }
}

export async function logout() {
  await fetch('/api/session', { method: 'DELETE', headers }).catch(() => {});
  await refreshSession();
}

export async function setLabel(id, label) {
  try {
    const res = await fetch(`/api/labels/${encodeURIComponent(id)}`, {
      method: 'PUT',
      headers,
      body: JSON.stringify(label),
    });
    if (!res.ok) throw new Error((await res.json()).error ?? res.statusText);
  } catch (err) {
    toast(t('systeme.hub.etiquette_non_enregistree', { message: err.message }));
  }
}
