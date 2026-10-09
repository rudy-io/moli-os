// Presentation rules: meaning first, brand never.

import { t, locale } from './i18n.svelte.js';

// One formatter per language, built when first needed (the language arrives
// with the session, after the first render).
const formatters = new Map();
function cached(name, make) {
  const key = `${name}:${locale()}`;
  if (!formatters.has(key)) formatters.set(key, make(locale()));
  return formatters.get(key);
}
const nf = () => cached('number', (l) => new Intl.NumberFormat(l, { maximumFractionDigits: 2 }));

/** Binary sensors read as words, not booleans: semantic → alarm-when-true.
 *  The words are in the catalog: systeme.format.binaire.<semantic>.on / .off */
const BINARY_ALARM = {
  contact: false,
  occupancy: false,
  water_leak: true,
  smoke: true,
  battery_low: true,
  tamper: true,
};

/** Semantics shown on the card face; everything else goes under « détails ». */
const PRIMARY = new Set([
  'on_off', 'brightness', 'power', 'apparent_power', 'energy', 'temperature',
  'humidity', 'pressure', 'illuminance', 'contact', 'occupancy', 'water_leak',
  'smoke', 'current',
]);

export function isPrimary(point) {
  return PRIMARY.has(point.semantic);
}

export function isAlarm(point, value) {
  return Boolean(Object.hasOwn(BINARY_ALARM, point.semantic) && BINARY_ALARM[point.semantic] && value === true);
}

export function formatValue(point, value) {
  if (value === undefined || value === null) return '—';
  if (point.kind.type === 'binary') {
    if (Object.hasOwn(BINARY_ALARM, point.semantic)) return t(`systeme.format.binaire.${point.semantic}.${value ? 'on' : 'off'}`);
    return value ? t('systeme.format.oui') : t('systeme.format.non');
  }
  if (typeof value === 'number') return nf().format(value);
  return String(value);
}

export function unitOf(point) {
  return point.unit && point.unit !== 'lqi' ? point.unit : '';
}

/** Names that are really addresses (Zigbee IEEE `0x00158d…`) say nothing to a human. */
const looksLikeAddress = (name) => !name || /^0x[0-9a-f]{6,}$/i.test(name.trim());

/** The user's label, else the name given in the source system, else the product. */
export function deviceName(d) {
  if (d.label?.name) return d.label.name;
  if (!looksLikeAddress(d.native_name)) return d.native_name;
  return d.description || d.model || d.native_name;
}

export function formatBytes(n) {
  if (n == null) return '—';
  const mb = n / (1024 * 1024);
  return mb >= 1024 ? t('systeme.format.go', { value: nf().format(mb / 1024) }) : t('systeme.format.mo', { value: nf().format(mb) });
}

export function formatDuration(ms) {
  if (ms == null) return '—';
  const s = Math.floor(ms / 1000);
  const d = Math.floor(s / 86400);
  const h = Math.floor((s % 86400) / 3600);
  const m = Math.floor((s % 3600) / 60);
  if (d) return t('systeme.format.duree_jours', { d, h });
  if (h) return t('systeme.format.duree_heures', { h, m });
  return m ? t('systeme.format.duree_minutes', { m }) : t('systeme.format.duree_secondes', { s });
}

const rtf = () => cached('relative', (l) => new Intl.RelativeTimeFormat(l, { numeric: 'auto' }));
export function ago(ts, now) {
  const s = Math.round((ts - now) / 1000);
  if (Math.abs(s) < 60) return rtf().format(s, 'second');
  if (Math.abs(s) < 3600) return rtf().format(Math.round(s / 60), 'minute');
  if (Math.abs(s) < 86400) return rtf().format(Math.round(s / 3600), 'hour');
  return rtf().format(Math.round(s / 86400), 'day');
}
