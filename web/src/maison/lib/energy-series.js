// What each part of the house drew, bucket by bucket (an hour, a day…),
// from an energy report (/api/energy/series): the circuits (net of the
// appliances measured inside them), the appliances, the lights (estimated,
// together), and the rest of the house. The parts add up to what the
// house used: its total (its main clamp, else the Linky), plus what it
// made and kept, less what the battery stored, plus what it gave back
// (`ownUse`). Nothing counted twice.

import { t } from '../../lib/i18n.svelte.js';
import { ownUse } from './energy-own.js';

const PALETTE = ['#e9a23b', '#5b8def', '#3fae7c', '#c46ad8', '#e0705a', '#2fb3b8', '#8c7ae6', '#d4a373'];
const LIGHTS = '#f2c94c';

/** The parts, each with `kwh(bucket)` and `cost(bucket)` (that hour's price). */
export function deviceSeries(meters) {
  const whole = meters.find((m) => m.role === 'total');
  const grid = meters.filter((m) => m.role === 'grid');
  const circuits = meters.filter((m) => m.role === 'circuit');
  const appliances = meters.filter((m) => m.role === 'appliance');
  const lights = appliances.filter((a) => a.estimated);
  const measured = appliances.filter((a) => !a.estimated);
  const k = (b, id) => b.meters[id]?.kwh ?? 0;
  const top = whole ? (b) => k(b, whole.id) : grid.length ? (b) => grid.reduce((t, g) => t + k(b, g.id), 0) : null;
  const paid = whole ? (b) => b.meters[whole.id]?.cost : grid.length ? (b) => grid.reduce((t, g) => t + (b.meters[g.id]?.cost ?? 0), 0) : null;

  const own = ownUse(meters);
  // What the house used: what was bought, plus what it used of its own.
  const used = top ? (b) => top(b) + (own?.(b) ?? 0) : null;

  const parts = [];
  let n = 0;
  for (const m of circuits) {
    const inner = appliances.filter((a) => a.within === m.id);
    parts.push({ id: m.id, name: m.name, color: PALETTE[n++ % PALETTE.length], kwh: (b) => Math.max(0, k(b, m.id) - inner.reduce((t, a) => t + k(b, a.id), 0)) });
  }
  for (const a of measured) parts.push({ id: a.id, name: a.name, color: PALETTE[n++ % PALETTE.length], kwh: (b) => k(b, a.id) });
  if (lights.length) parts.push({ id: '__lights', name: t('energie.lumieres'), estimated: true, color: LIGHTS, kwh: (b) => lights.reduce((t, a) => t + k(b, a.id), 0) });
  if (top) {
    const others = [...parts];
    parts.push({ id: '__rest', name: t('energie.reste'), rest: true, color: 'var(--surface-3)', kwh: (b) => Math.max(0, used(b) - others.reduce((t, p) => t + p.kwh(b), 0)) });
  }
  // Each part at that bucket's price: what the house paid then per kWh it
  // used (with panels or a battery, not every kWh used was bought), so the
  // parts add up to what was paid.
  const price = (b) => {
    const u = used?.(b);
    const c = paid?.(b);
    return u > 0 && c != null ? c / u : null;
  };
  for (const p of parts) {
    p.cost = (b) => {
      const pr = price(b);
      return pr == null ? null : p.kwh(b) * pr;
    };
  }
  return parts;
}

/** The share of an hour starting at `start` (ms) spent off-peak, from the
 *  windows learnt off the meter (minutes of the day, `{from, to}`). */
export function offPeakShare(start, windows) {
  if (!windows?.length) return 0;
  const d = new Date(start);
  const m0 = d.getHours() * 60 + d.getMinutes();
  let inside = 0;
  for (let m = m0; m < m0 + 60; m++) {
    const t = m % 1440;
    if (windows.some((w) => (w.from < w.to ? t >= w.from && t < w.to : t >= w.from || t < w.to))) inside++;
  }
  return inside / 60;
}
