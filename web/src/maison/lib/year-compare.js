// This month against the same days a year ago, from the daily energy series
// (/api/energy/series?step=day&count=400). Only whole days count: from the
// 1st to yesterday, both years, so a day half gone is never set against a
// whole one. Pure: the page gives the series and the time.

const DAY = 86_400_000;

/** Local midnight of a date (month 0-based), clamped to the month's length. */
function midnight(year, month, day) {
  const last = new Date(year, month + 1, 0).getDate();
  return new Date(year, month, Math.min(day, last)).getTime();
}

/** The two spans to compare at `now` (ms): this month from the 1st to
 *  yesterday, and the same days a year earlier; null on the 1st. */
export function spans(now) {
  const d = new Date(now);
  const y = d.getFullYear();
  const m = d.getMonth();
  const today = d.getDate();
  if (today <= 1) return null;
  return {
    now: [midnight(y, m, 1), midnight(y, m, today)],
    before: [midnight(y - 1, m, 1), midnight(y - 1, m, today)],
    days: today - 1,
    month: new Date(y - 1, m, 1),
  };
}

/** The meters of a role: `solar`, or for what was bought, the `total`
 *  meters (finest) and else the `grid` ones. */
function metersOf(series, role) {
  const meters = series?.meters ?? [];
  if (role === 'bought') {
    const total = meters.filter((m) => m.role === 'total');
    return new Set((total.length ? total : meters.filter((m) => m.role === 'grid')).map((m) => m.id));
  }
  return new Set(meters.filter((m) => m.role === role).map((m) => m.id));
}

/** kWh of the role's meters in the days starting within [from, to), and
 *  whether every one of those days has a reading. */
export function sum(series, role, [from, to]) {
  const ids = metersOf(series, role);
  let kwh = 0;
  let days = 0;
  for (const b of series?.buckets ?? []) {
    if (b.start < from || b.start >= to) continue;
    let any = false;
    for (const [id, m] of Object.entries(b.meters ?? {})) {
      if (ids.has(id) && typeof m?.kwh === 'number') {
        kwh += m.kwh;
        any = true;
      }
    }
    if (any) days++;
  }
  return { kwh, complete: ids.size > 0 && days >= Math.round((to - from) / DAY) };
}

/** This month so far against a year ago, for `role` (`bought`, `solar`):
 *  `{ now, before, pct, month }`, or null when a year ago is not fully
 *  known (no history then, a meter added since) or too small to compare. */
export function yearOverYear(series, now, role) {
  const s = spans(now);
  if (!s) return null;
  const cur = sum(series, role, s.now);
  const old = sum(series, role, s.before);
  if (!old.complete || !cur.complete || old.kwh < 1) return null;
  return { now: cur.kwh, before: old.kwh, pct: Math.round((cur.kwh / old.kwh - 1) * 100), month: s.month, days: s.days };
}

/** The colour of a change in consumption, the same for every comparison:
 *  green when less (5 % or more), neutral within ±5 %, orange a little more,
 *  red from 25 % more. For production, `more` is better: the scale turns. */
export function trendTone(pct, more = 'worse') {
  if (pct == null || !Number.isFinite(pct)) return '';
  const p = more === 'better' ? -pct : pct;
  if (p <= -5) return 'good';
  if (p < 5) return '';
  return p < 25 ? 'warm' : 'alert';
}
