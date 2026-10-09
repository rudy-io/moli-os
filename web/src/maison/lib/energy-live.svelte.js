// Energy summary (/api/energy), shared by every page that shows it.
import { t, locale } from '../../lib/i18n.svelte.js';

export const energy = $state({ summary: null, missing: false });

let timer = null;
let users = 0;

async function refresh() {
  try {
    const res = await fetch('/api/energy');
    if (res.status === 404) {
      energy.missing = true;
      return;
    }
    if (res.ok) energy.summary = await res.json();
  } catch {
    /* next round */
  }
}

/** Starts polling while at least one view needs it. Returns a stop function. */
export function useEnergy() {
  users += 1;
  if (users === 1) {
    refresh();
    timer = setInterval(refresh, 60_000);
  }
  return () => {
    users -= 1;
    if (users === 0) clearInterval(timer);
  };
}

/** The month's bill at this pace, subscription included (null on its first day). */
export function billOf(s) {
  const cost = s?.month_projection?.cost;
  return cost == null ? null : cost + (s.monthly_fee ?? 0);
}

export function euros(v, currency = '€') {
  if (v == null || !Number.isFinite(v)) return '—';
  return `${v.toLocaleString(locale(), { minimumFractionDigits: 2, maximumFractionDigits: 2 })} ${currency}`;
}

export function kwh(v) {
  if (v == null || !Number.isFinite(v)) return '—';
  return v.toLocaleString(locale(), { maximumFractionDigits: v < 10 ? 1 : 0, minimumFractionDigits: v < 10 ? 1 : 0 });
}

export function watts(v) {
  if (v == null || !Number.isFinite(v)) return '—';
  return v >= 1000
    ? `${(v / 1000).toLocaleString(locale(), { maximumFractionDigits: 1 })} kW`
    : `${Math.round(v).toLocaleString(locale())} W`;
}

/** How much the house draws, as a colour and a word: green while calm,
 *  orange from `warn`, red at twice that. */
export function liveLook(v, warn) {
  if (v == null || !warn) return { hue: null, word: '' };
  const r = v / warn;
  const hue = r <= 0.4 ? 145 : r <= 1 ? 145 - ((r - 0.4) / 0.6) * 115 : Math.max(2, 30 - (r - 1) * 28);
  const level = r < 0.4 ? 'calme' : r < 1 ? 'moderee' : r < 2 ? 'soutenue' : 'forte';
  return { hue, word: t('energie.niveau.' + level) };
}

/** Power in the point's unit: watts, or volt-amperes for a Linky (« 2,4 kVA »). */
export function power(v, unit = 'W') {
  if (unit !== 'VA') return watts(v);
  if (v == null || !Number.isFinite(v)) return '—';
  return v >= 1000
    ? `${(v / 1000).toLocaleString(locale(), { maximumFractionDigits: 1 })} kVA`
    : `${Math.round(v).toLocaleString(locale())} VA`;
}
