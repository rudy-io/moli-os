// Automations in the dashboard: the catalog of nodes, the API, the layout
// and the words on each node. The server has the last word (checks, the
// French sentence); this file keeps the editor quick.

import { hub } from '../../lib/hub.svelte.js';
import { t, locale } from '../../lib/i18n.svelte.js';
import { home, nameOf, note } from './home.svelte.js';

/** What a cancelled code sheet rejects with: a signal, never shown. */
export const CANCELLED = 'cancelled';

const headers = { 'content-type': 'application/json', 'x-moli-origin': 'ui' };

// ---- catalog -------------------------------------------------------------------

/** One kind of node. Its words are read when asked, never kept: the house's
 *  language arrives after the first render. */
function kind(type, family, icon, defaults) {
  return {
    family,
    icon,
    defaults,
    get label() {
      return t(`automatismes.noeud.${type}.label`);
    },
    get short() {
      return t(`automatismes.noeud.${type}.court`);
    },
    get hint() {
      return t(`automatismes.noeud.${type}.indice`);
    },
  };
}

/** Every kind of node: family (colour), words, icon, outputs, defaults. */
export const CATALOG = {
  when_state: kind('when_state', 'trigger', 'flash', { point: '', to: true, for_s: 0 }),
  when_threshold: kind('when_threshold', 'trigger', 'thermometer', { point: '', above: 0, for_s: 0 }),
  at_time: kind('at_time', 'trigger', 'clock', { at: '07:00', days: [] }),
  at_sun: kind('at_sun', 'trigger', 'sunset', { event: 'set', offset_min: 0, days: [] }),
  every: kind('every', 'trigger', 'repeat', { minutes: 15 }),
  on_start: kind('on_start', 'trigger', 'power', {}),
  manual: kind('manual', 'trigger', 'hand', {}),
  if: kind('if', 'logic', 'branch', { rules: [{ kind: 'sun', is: 'night' }], all: true }),
  set: kind('set', 'action', 'tune', { point: '', value: true }),
  toggle: kind('toggle', 'action', 'swap', { point: '' }),
  wait: kind('wait', 'wait', 'timer', { seconds: 300 }),
  wait_for: kind('wait_for', 'wait', 'timer', { rule: { kind: 'state', point: '', op: 'eq', value: false }, timeout_s: 600 }),
  notify: kind('notify', 'notify', 'bell', { title: null, message: '', channels: ['maison'] }),
  write: kind('write', 'moli', 'mic', { prompt: '' }),
};

export const FAMILIES = ['trigger', 'logic', 'action', 'wait', 'notify', 'moli'].map((id) => ({
  id,
  get label() {
    return t(`automatismes.famille.${id}.label`);
  },
  get hint() {
    return t(`automatismes.famille.${id}.indice`);
  },
}));

export const PORTS = { if: ['yes', 'no'], wait_for: ['ok', 'timeout'] };
export const PORT_LABEL = {
  out: '',
  get yes() {
    return t('automatismes.sortie.yes');
  },
  get no() {
    return t('automatismes.sortie.no');
  },
  get ok() {
    return t('automatismes.sortie.ok');
  },
  get timeout() {
    return t('automatismes.sortie.timeout');
  },
};
export const ports = (type) => PORTS[type] ?? ['out'];
export const isTrigger = (type) => CATALOG[type]?.family === 'trigger';

// ---- words -----------------------------------------------------------------------

// Monday first, as the server counts them (1 = Monday).
const DAY_IDS = ['lundi', 'mardi', 'mercredi', 'jeudi', 'vendredi', 'samedi', 'dimanche'];

/** The one-letter names of the days, Monday first. */
export const dayInitials = () => DAY_IDS.map((id) => t(`automatismes.initiale.${id}`));

export function daysText(days) {
  const d = [...new Set(days ?? [])].sort();
  if (!d.length || d.length === 7) return t('automatismes.texte.chaque_jour');
  if (d.join() === '1,2,3,4,5') return t('automatismes.texte.en_semaine');
  if (d.join() === '6,7') return t('automatismes.texte.week_end');
  return d.map((n) => (DAY_IDS[n - 1] ? t(`automatismes.jour.${DAY_IDS[n - 1]}`) : '')).join(', ');
}

export function duration(s) {
  s = Number(s) || 0;
  if (s >= 3600 && s % 3600 === 0) return `${s / 3600} h`;
  if (s >= 3600) return `${Math.floor(s / 3600)} h ${String(Math.floor((s % 3600) / 60)).padStart(2, '0')}`;
  if (s >= 60 && s % 60 === 0) return `${s / 60} min`;
  if (s > 60) return `${Math.floor(s / 60)} min ${s % 60} s`;
  return `${s} s`;
}

export function splitPoint(point) {
  const i = (point ?? '').indexOf('/');
  return i < 0 ? [point ?? '', ''] : [point.slice(0, i), point.slice(i + 1)];
}

export function pointSpec(point) {
  const [id, key] = splitPoint(point);
  return hub.devices[id]?.points.find((p) => p.key === key) ?? null;
}

export function pointName(point) {
  const [id, key] = splitPoint(point);
  if (!id) return t('automatismes.point.choisir');
  const d = hub.devices[id];
  if (!d) return t('automatismes.point.inconnu');
  const spec = d.points.find((p) => p.key === key);
  const name = nameOf(id);
  // The value says it (« ouverte », « détecté »): the device's name is enough.
  const generic = ['on', 'on_off', 'state', 'switch', 'switch_1', 'power', 'contact', 'doorcontact_state', 'occupancy', 'motion', 'person', 'doorbell', 'vehicle', 'animal', 'package'].includes(key);
  return spec && !generic ? `${name} · ${spec.label}` : name;
}

/** Every point a graph touches (steps and rules alike). */
export function graphPoints(graph) {
  const found = new Set();
  const walk = (v) => {
    if (Array.isArray(v)) v.forEach(walk);
    else if (v && typeof v === 'object') {
      for (const [k, x] of Object.entries(v)) {
        if (k === 'point' && typeof x === 'string') found.add(x);
        else walk(x);
      }
    }
  };
  walk(graph?.nodes);
  return [...found];
}

/** Devices of a graph known in Moli under another name than their own:
 *  shown before approving (an agent may have named them). */
export function renamedIn(graph) {
  const ids = new Set(graphPoints(graph).map((p) => splitPoint(p)[0]).filter(Boolean));
  return [...ids]
    .map((id) => hub.devices[id])
    .filter((d) => d?.label?.name && d.label.name !== d.native_name)
    .map((d) => ({ id: d.id, name: d.label.name, native: d.native_name }));
}

const POWER_KEYS = ['on', 'on_off', 'state', 'switch', 'switch_1', 'switch_2', 'power'];

/** How a value reads for this point (« allumé », « ouverte », « 21 °C »). */
export function valueText(point, v) {
  const spec = pointSpec(point);
  const key = splitPoint(point)[1];
  const flag = v === true || v === 'ON' || v === 'on' ? true : v === false || v === 'OFF' || v === 'off' ? false : null;
  if (flag != null) {
    if (key === 'contact') return t(flag ? 'automatismes.valeur.fermee' : 'automatismes.valeur.ouverte');
    if (key === 'doorcontact_state') return t(flag ? 'automatismes.valeur.ouverte' : 'automatismes.valeur.fermee');
    if (['person', 'vehicle', 'animal', 'package', 'motion', 'occupancy', 'presence', 'doorbell', 'smoke', 'water_leak'].includes(key))
      return t(flag ? 'automatismes.valeur.detecte' : 'automatismes.valeur.rien');
    if (POWER_KEYS.includes(key) || spec?.semantic === 'on_off') return t(flag ? 'automatismes.valeur.allume' : 'automatismes.valeur.eteint');
    return t(flag ? 'automatismes.valeur.oui' : 'automatismes.valeur.non');
  }
  if (v == null || v === '') return '…';
  if (typeof v === 'number') return `${v.toLocaleString(locale(), { useGrouping: false, maximumFractionDigits: 20 })}${spec?.unit ? ` ${spec.unit}` : ''}`;
  return String(v);
}

const SPOKEN_CHANNELS = ['telegram', 'voix', 'telephone'];

/** The one line under a node's title. */
export function nodeText(node) {
  switch (node.type) {
    case 'when_state': {
      const base = `${pointName(node.point)} → ${node.to != null ? valueText(node.point, node.to) : t('automatismes.texte.change')}`;
      return node.for_s ? t('automatismes.texte.depuis', { texte: base, duree: duration(node.for_s) }) : base;
    }
    case 'when_threshold': {
      const spec = pointSpec(node.point);
      const u = spec?.unit ? ` ${spec.unit}` : '';
      const what =
        node.above != null && node.below != null
          ? t('automatismes.texte.entre', { min: node.above, max: node.below, unite: u })
          : node.above != null
            ? `> ${node.above}${u}`
            : `< ${node.below}${u}`;
      const base = `${pointName(node.point)} ${what}`;
      return node.for_s ? t('automatismes.texte.pendant', { texte: base, duree: duration(node.for_s) }) : base;
    }
    case 'at_time':
      return `${node.at ?? '—'} · ${daysText(node.days)}`;
    case 'at_sun': {
      const rise = node.event === 'rise';
      const off = Number(node.offset_min) || 0;
      if (!off) return t(rise ? 'automatismes.texte.au_lever' : 'automatismes.texte.au_coucher');
      const key = off < 0 ? (rise ? 'avant_lever' : 'avant_coucher') : rise ? 'apres_lever' : 'apres_coucher';
      return t(`automatismes.texte.${key}`, { duree: duration(Math.abs(off) * 60) });
    }
    case 'every':
      return t('automatismes.texte.toutes_les', { duree: duration((node.minutes ?? 0) * 60) });
    case 'on_start':
      return t('automatismes.texte.demarrage');
    case 'manual':
      return t('automatismes.texte.manuel');
    case 'if':
      return (
        (node.rules ?? []).map(ruleText).join(` ${t(node.all === false ? 'automatismes.texte.ou' : 'automatismes.texte.et')} `) ||
        t('automatismes.texte.aucune_condition')
      );
    case 'set': {
      const key = splitPoint(node.point)[1];
      if (key === 'key') return t('automatismes.texte.touche', { nom: pointName(node.point), valeur: node.value });
      return `${pointName(node.point)} → ${valueText(node.point, node.value)}`;
    }
    case 'toggle':
      return pointName(node.point);
    case 'wait':
      return duration(node.seconds);
    case 'wait_for':
      return t('automatismes.texte.attendre_max', { regle: ruleText(node.rule ?? {}), duree: duration(node.timeout_s) });
    case 'notify': {
      const where = (node.channels ?? []).map((c) => t(`automatismes.texte.canal.${SPOKEN_CHANNELS.includes(c) ? c : 'maison'}`)).join(' + ');
      return t('automatismes.texte.notifier', { canaux: where, message: node.message || '…' });
    }
    case 'write':
      return node.prompt || '…';
    default:
      return '';
  }
}

const OPS = { eq: '=', ne: '≠', gt: '>', ge: '≥', lt: '<', le: '≤' };
export function ruleText(r) {
  if (r.kind === 'sun') return t(r.is === 'day' ? 'automatismes.regle.jour' : 'automatismes.regle.nuit');
  if (r.kind === 'time') {
    const h =
      r.after && r.before
        ? t('automatismes.regle.entre', { debut: r.after, fin: r.before })
        : r.after
          ? t('automatismes.regle.apres', { heure: r.after })
          : r.before
            ? t('automatismes.regle.avant', { heure: r.before })
            : '';
    return [h, r.days?.length ? daysText(r.days) : ''].filter(Boolean).join(', ') || t('automatismes.regle.toute_heure');
  }
  if (typeof r.value === 'boolean' && (r.op ?? 'eq') === 'eq') return `${pointName(r.point)} ${valueText(r.point, r.value)}`;
  return `${pointName(r.point)} ${OPS[r.op ?? 'eq']} ${valueText(r.point, r.value)}`;
}

// ---- graph helpers -----------------------------------------------------------

/** A node as the server stores it: `type` + its fields + position. */
export function newNode(type, graph, x = 0, y = 0) {
  const used = new Set(graph.nodes.map((n) => n.id));
  const prefix = isTrigger(type) ? 't' : type === 'if' ? 'c' : 'a';
  let i = 1;
  while (used.has(`${prefix}${i}`)) i += 1;
  return { id: `${prefix}${i}`, type, ...structuredClone(CATALOG[type].defaults), x, y };
}

export const NODE_W = 236;
export const NODE_H = 78;

/** Height of an output port on a node of this type. */
export function portY(type, port) {
  const list = ports(type);
  if (list.length === 1) return NODE_H / 2;
  return NODE_H * (list.indexOf(port) === 0 ? 0.32 : 0.72);
}
const GAP_X = 280;
const GAP_Y = 120;

/** Left to right by depth, branches stacked: for drafts without positions. */
export function layout(graph) {
  const depth = new Map();
  const triggers = graph.nodes.filter((n) => isTrigger(n.type));
  triggers.forEach((t) => depth.set(t.id, 0));
  // Longest path from a trigger (the graph has no cycles; cap anyway).
  for (let round = 0; round < graph.nodes.length + 1; round += 1) {
    let moved = false;
    for (const e of graph.edges) {
      const d = depth.get(e.from);
      if (d == null) continue;
      if ((depth.get(e.to) ?? -1) < d + 1) {
        depth.set(e.to, d + 1);
        moved = true;
      }
    }
    if (!moved) break;
  }
  const columns = new Map();
  for (const n of graph.nodes) {
    const d = depth.get(n.id) ?? 0;
    if (!columns.has(d)) columns.set(d, []);
    columns.get(d).push(n);
  }
  // Order within a column: follow the parents' order, « oui » above « non ».
  const portRank = { out: 0, yes: 0, ok: 0, no: 1, timeout: 1 };
  const order = new Map();
  for (const [d, list] of [...columns.entries()].sort((a, b) => a[0] - b[0])) {
    list.sort((a, b) => key(a) - key(b));
    list.forEach((n, i) => order.set(n.id, i));
    const height = (list.length - 1) * GAP_Y;
    list.forEach((n, i) => {
      n.x = 60 + d * GAP_X;
      n.y = 200 - height / 2 + i * GAP_Y;
    });
  }
  function key(n) {
    const parent = graph.edges.find((e) => e.to === n.id);
    if (!parent) return graph.nodes.indexOf(n);
    return (order.get(parent.from) ?? 0) * 10 + (portRank[parent.port] ?? 0);
  }
  return graph;
}

export const needsLayout = (graph) => graph.nodes.length > 0 && graph.nodes.every((n) => !n.x && !n.y);

// ---- API -------------------------------------------------------------------------

async function call(method, url, body) {
  const res = await fetch(url, { method, headers, body: body ? JSON.stringify(body) : undefined });
  const data = await res.json().catch(() => ({}));
  if (!res.ok) {
    const err = new Error(data.error ?? res.statusText);
    err.status = res.status;
    throw err;
  }
  return data;
}

export const api = {
  list: () => call('GET', '/api/automations'),
  get: (id) => call('GET', `/api/automations/${encodeURIComponent(id)}`),
  create: (a) => call('POST', '/api/automations', a),
  update: (a) => call('PUT', `/api/automations/${encodeURIComponent(a.id)}`, a),
  remove: (id) => call('DELETE', `/api/automations/${encodeURIComponent(id)}`),
  // Approving, switching on and running name the version the person saw:
  // another version in between is refused (409) and must be looked at again.
  approve: (id, fingerprint) => call('POST', `/api/automations/${encodeURIComponent(id)}/approve`, { fingerprint }),
  enabled: (id, on, fingerprint) => call('POST', `/api/automations/${encodeURIComponent(id)}/enabled`, { on, fingerprint }),
  test: (id) => call('POST', `/api/automations/${encodeURIComponent(id)}/test`),
  run: (id, fingerprint) => call('POST', `/api/automations/${encodeURIComponent(id)}/run`, { fingerprint }),
  restore: (id) => call('POST', `/api/automations/${encodeURIComponent(id)}/restore`),
  check: (graph) => call('POST', '/api/automations/check', { graph }),
  draft: (request, current) => call('POST', '/api/automations/draft', { request, current }),
  runs: (limit = 20) => call('GET', `/api/automations/runs?limit=${limit}`),
};

/** What a person does (approve, run, delete) needs « c'est moi »: asks the
 *  code once, then does it. */
export async function asHuman(label, action) {
  if (hub.session?.human) {
    try {
      return await action();
    } catch (err) {
      if (err.status !== 403) throw err;
    }
  }
  return new Promise((resolve, reject) => {
    home.held = {
      label,
      reason: 'automatisme',
      custom: async () => {
        try {
          resolve(await action());
        } catch (err) {
          note(err.message, 'error');
          reject(err);
        }
      },
      onCancel: () => reject(new Error(CANCELLED)),
    };
  });
}

/** A draft (from Moli) waiting for the editor to open it. */
export const pending = $state({ draft: null });

/** The run of an automation, step by step, as it happens. */
export function liveRuns(onEvent) {
  const source = new EventSource('/api/automations/live');
  source.addEventListener('live', (m) => onEvent(JSON.parse(m.data)));
  return () => source.close();
}
