// Reading the house's electricity for advice: when the off-peak hours are
// (learned from the Linky's own tariff changes, no setting), the night-time
// floor, a typical day, and what to do right now. Pure functions: the page
// feeds them what it has, in the browser's (the house's) time zone.

import { t, locale } from '../../lib/i18n.svelte.js';

const DAY = 24 * 60;
const minuteOf = (ts) => {
  const d = new Date(ts);
  return d.getHours() * 60 + d.getMinutes();
};

/** Off-peak windows `[{ from, to }]` in minutes of the day (`to` may be
 *  past midnight, then smaller than `from`), from the tariff point's
 *  history (`/api/history` series: `before` and `raw`). */
export function offPeak(series, now = Date.now()) {
  const raw = series?.raw ?? [];
  if (raw.length < 2) return [];
  const hc = (v) => /hc|creuse/i.test(String(v));
  const start = raw[0][0];
  const hits = new Array(DAY).fill(0);
  const seen = new Array(DAY).fill(0);
  let i = 0;
  let state = series.before ? hc(series.before[1]) : hc(raw[0][1]);
  for (let t = start; t < now; t += 60_000) {
    while (i < raw.length && raw[i][0] <= t) state = hc(raw[i++][1]);
    const m = minuteOf(t);
    seen[m] += 1;
    if (state) hits[m] += 1;
  }
  // A minute is off-peak when it was on most of the days seen.
  const off = hits.map((h, m) => seen[m] > 0 && h / seen[m] > 0.5);
  if (off.every(Boolean) || !off.some(Boolean)) return [];
  const windows = [];
  // Start the walk on a peak minute so that a window crossing midnight stays whole.
  const first = off.findIndex((o) => !o);
  for (let k = 0; k < DAY; k++) {
    const m = (first + k) % DAY;
    if (off[m] && !off[(m + DAY - 1) % DAY]) windows.push({ from: m, to: m });
    if (off[m]) windows[windows.length - 1].to = (m + 1) % DAY;
  }
  return windows;
}

const inside = (w, m) => (w.from < w.to ? m >= w.from && m < w.to : m >= w.from || m < w.to);

/** Whether `now` is off-peak by the learned windows, and when that changes. */
export function nextChange(windows, now = Date.now()) {
  if (!windows.length) return null;
  const m = minuteOf(now);
  const off = windows.some((w) => inside(w, m));
  // The nearest boundary ahead: a start when on peak, an end when off.
  const marks = windows.map((w) => (off ? w.to : w.from));
  const ahead = Math.min(...marks.map((b) => (b - m + DAY) % DAY || DAY));
  const at = new Date(now + ahead * 60_000);
  at.setSeconds(0, 0);
  return { off, at, minutes: ahead };
}

export const clockText = (date) => {
  const h = date.getHours();
  const m = date.getMinutes();
  return m ? t('energie.heure.minutes', { h, m: String(m).padStart(2, '0') }) : t('energie.heure.pile', { h });
};

export const minutesText = (n) => {
  if (n < 60) return t('energie.duree.minutes', { n });
  const h = Math.floor(n / 60);
  const m = n % 60;
  return m ? t('energie.duree.heures_minutes', { h, m: String(m).padStart(2, '0') }) : t('energie.duree.heures', { h });
};

export const windowText = (w) => {
  const at = (m) => clockText(new Date(2000, 0, 1, Math.floor(m / 60), m % 60));
  return `${at(w.from)} → ${at(w.to)}`;
};

/** The floor (W): what never stops (fridge, box, servers, standbys).
 *  Median over the days of each day's quietest hour (days with at least
 *  12 hours measured); null below three days. `report`:
 *  `/api/energy/series?step=hour`. */
export function baseload(report) {
  const meter = report?.meters.find((m) => m.role === 'total') ?? null;
  if (!meter) return null;
  const days = new Map();
  for (const b of report.buckets) {
    const kwh = b.meters[meter.id]?.kwh;
    if (kwh == null) continue;
    const day = new Date(b.start).toDateString();
    const [low, n] = days.get(day) ?? [Infinity, 0];
    days.set(day, [Math.min(low, kwh), n + 1]);
  }
  const lows = [...days.values()].filter(([, n]) => n >= 12).map(([low]) => low).sort((a, b) => a - b);
  if (lows.length < 3) return null;
  return Math.round(lows[Math.floor(lows.length / 2)] * 1000);
}

/** A typical day: average kWh for each hour (0 → 23), whole house. */
export function typicalDay(report) {
  const meter = report?.meters.find((m) => m.role === 'total');
  const grid = report?.meters.filter((m) => m.role === 'grid') ?? [];
  if (!meter && !grid.length) return null;
  const sum = new Array(24).fill(0);
  const n = new Array(24).fill(0);
  for (const b of report.buckets) {
    const kwh = meter ? b.meters[meter.id]?.kwh : grid.reduce((t, g) => t + (b.meters[g.id]?.kwh ?? 0), 0);
    if (kwh == null) continue;
    const h = new Date(b.start).getHours();
    sum[h] += kwh;
    n[h] += 1;
  }
  if (n.every((c) => c === 0)) return null;
  return sum.map((s, h) => (n[h] ? s / n[h] : 0));
}

/** Today, hour by hour (kWh; null for the hours still to come), from the
 *  same hourly report as the typical day. */
export function todayByHour(report, now = Date.now()) {
  const meter = report?.meters.find((m) => m.role === 'total');
  const grid = report?.meters.filter((m) => m.role === 'grid') ?? [];
  if (!meter && !grid.length) return null;
  const midnight = new Date(now);
  midnight.setHours(0, 0, 0, 0);
  const out = new Array(24).fill(null);
  for (const b of report.buckets) {
    if (b.start < midnight.getTime() || b.start > now) continue;
    const kwh = meter ? b.meters[meter.id]?.kwh : grid.reduce((t, g) => t + (b.meters[g.id]?.kwh ?? 0), 0);
    if (kwh != null) out[new Date(b.start).getHours()] = kwh;
  }
  return out.some((v) => v != null) ? out : null;
}

/** Price per kWh of each tariff period, from what the month cost so far. */
export function prices(summary) {
  const out = {};
  for (const m of summary?.meters ?? []) {
    if (m.role !== 'grid') continue;
    const a = summary.month.meters[m.id];
    if (!a?.kwh || a.cost == null) continue;
    const key = /hc|creuse/i.test(m.id + m.name) ? 'hc' : /hp|pleine/i.test(m.id + m.name) ? 'hp' : null;
    if (key) out[key] = a.cost / a.kwh;
  }
  return out;
}

const euro = (v) => `${v.toLocaleString(locale(), { maximumFractionDigits: 0 })} €`;
const w = (v) => (v >= 1000 ? `${(v / 1000).toLocaleString(locale(), { maximumFractionDigits: 1 })} kW` : `${Math.round(v)} W`);

/**
 * What to do now, most pressing first. Each: `{ tone, icon, title, text }`.
 *
 * `ctx`: `{ off (bool|null: off-peak now), change (nextChange), live, warn,
 * unit, draws ([{name, w}] by circuit), climates ([{name, on, mode, temp,
 * target, outdoor}]), openDoors ([name]), pool ({ on }|null), price
 * ({hp, hc}), base (W), avgPrice }`.
 */
export function advise(ctx) {
  const out = [];
  const peak = ctx.off === false;
  const saving = ctx.price?.hp && ctx.price?.hc ? Math.round((1 - ctx.price.hc / ctx.price.hp) * 100) : null;
  const running = (ctx.climates ?? []).filter((c) => c.on);

  if (running.length && ctx.openDoors?.length) {
    out.push({
      tone: 'alert',
      icon: 'door-open',
      title: t('energie.conseil.porte_titre', { count: ctx.openDoors.length, doors: ctx.openDoors.join(', ') }),
      text: t('energie.conseil.porte_texte', { names: running.map((c) => c.name).join(', ') }),
    });
  }

  if (peak && ctx.live != null && ctx.warn && ctx.live >= ctx.warn) {
    const top = (ctx.draws ?? []).filter((d) => !d.rest && d.w >= 300).slice(0, 2);
    const soon = ctx.change && !ctx.change.off && ctx.change.minutes <= 180;
    out.push({
      tone: 'warm',
      icon: 'flash',
      title: t('energie.conseil.gros_titre'),
      text: [
        top.length ? t('energie.conseil.gros_gourmand', { list: top.map((d) => `${d.name} (${w(d.w)})`).join(', ') }) : '',
        soon ? t('energie.conseil.gros_bientot', { at: clockText(ctx.change.at) }) : t('energie.conseil.gros_decaler'),
      ]
        .filter(Boolean)
        .join(' '),
    });
  }

  for (const c of running) {
    const temp = typeof c.temp === 'number' ? c.temp : null;
    const o = typeof c.outdoor === 'number' ? c.outdoor : null;
    if (c.mode === 'froid' && temp != null && o != null && o <= temp - 3) {
      out.push({
        tone: 'warm',
        icon: 'snowflake',
        title: t('energie.conseil.frais_titre', { name: c.name }),
        text: t('energie.conseil.frais_texte', { outdoor: Math.round(o), indoor: Math.round(temp) }),
      });
    } else if (c.mode === 'froid' && typeof c.target === 'number' && c.target < 24) {
      out.push({
        tone: 'info',
        icon: 'thermometer',
        title: t('energie.conseil.consigne_titre', { name: c.name, target: c.target }),
        text: t('energie.conseil.consigne_froid'),
      });
    } else if (c.mode === 'chaud' && typeof c.target === 'number' && c.target > 21) {
      out.push({
        tone: 'info',
        icon: 'thermometer',
        title: t('energie.conseil.consigne_titre', { name: c.name, target: c.target }),
        text: t('energie.conseil.consigne_chaud'),
      });
    }
  }

  if (peak && ctx.pool?.on) {
    out.push({
      tone: 'info',
      icon: 'pool',
      title: t('energie.conseil.piscine_titre'),
      text: saving != null ? t('energie.conseil.piscine_texte_prix', { saving }) : t('energie.conseil.piscine_texte'),
    });
  }

  if (ctx.change) {
    if (ctx.change.off) {
      out.push({
        tone: 'good',
        icon: 'moon',
        title: t('energie.conseil.creuses_titre', { at: clockText(ctx.change.at) }),
        text: saving != null ? t('energie.conseil.creuses_texte_prix', { saving }) : t('energie.conseil.creuses_texte'),
      });
    } else if (ctx.change.minutes <= 240) {
      out.push({
        tone: 'info',
        icon: 'clock',
        title: t('energie.conseil.bientot_titre', { at: clockText(ctx.change.at), delay: minutesText(ctx.change.minutes) }),
        text: saving != null ? t('energie.conseil.bientot_texte_prix', { saving }) : t('energie.conseil.bientot_texte'),
      });
    }
  }

  if (ctx.base != null && ctx.base >= 120) {
    const year = ctx.avgPrice ? (ctx.base / 1000) * 24 * 365 * ctx.avgPrice : null;
    out.push({
      tone: 'info',
      icon: 'power',
      title: t('energie.conseil.talon_titre', { power: w(ctx.base) }),
      text: year ? t('energie.conseil.talon_texte_an', { year: euro(year) }) : t('energie.conseil.talon_texte'),
    });
  }
  return out;
}
