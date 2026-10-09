// Energy formatting and shared helpers.
import { locale } from './i18n.svelte.js';

// Formatted when asked: the house's language arrives after the first render.
const ONE = { maximumFractionDigits: 1, minimumFractionDigits: 1 };
const SMALL = { maximumFractionDigits: 2 };
const WHOLE = { maximumFractionDigits: 0 };

export function kwh(v) {
  if (v == null || !Number.isFinite(v)) return '—';
  return v.toLocaleString(locale(), Math.abs(v) < 1 ? SMALL : ONE) + ' kWh';
}

export function money(v, currency = '€') {
  if (v == null || !Number.isFinite(v)) return '';
  return v.toLocaleString(locale(), { minimumFractionDigits: 2, maximumFractionDigits: 2 }) + ' ' + currency;
}

export function watts(v) {
  if (v == null || !Number.isFinite(v)) return '—';
  return v >= 10000 ? (v / 1000).toLocaleString(locale(), ONE) + ' kW' : v.toLocaleString(locale(), WHOLE) + ' W';
}

/** Live value of a point from the SSE-fed model (fresher than the summary). */
export function livePoint(devices, point) {
  if (!point) return null;
  const i = point.indexOf('/');
  const v = devices[point.slice(0, i)]?.state?.[point.slice(i + 1)]?.value;
  return typeof v === 'number' ? v : null;
}

/** Stable colours per meter, by role then order. */
export function meterColor(meter, index) {
  if (/hc|creuse/i.test(meter.id + meter.name)) return 'var(--ink-2)';
  if (/hp|pleine/i.test(meter.id + meter.name)) return 'var(--sun)';
  if (meter.role === 'total') return 'var(--copper)';
  const palette = ['#3f7d4e', '#5b6fb5', '#b5653a', '#8a5bb5', '#2f8f8f', '#a8892a'];
  return palette[index % palette.length];
}
