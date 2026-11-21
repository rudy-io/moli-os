// The day hour by hour, from the hourly energy series (/api/energy/series
// ?step=hour, a week back): what the house used, the part the sun covered,
// what was bought, and a usual day (the last 7 days) to set it against.
// Pure: the page gives the series and the time.
//
// What the house used is not what was bought: with panels, the sun's share
// is used without going through the meter. An hour's consumption is
//   bought + produced − sent out − stored + given back by the battery,
// never below zero. Without panels nor battery, it is what was bought.

const HOUR = 3_600_000;

function ids(series, role) {
  return (series?.meters ?? []).filter((m) => m.role === role).map((m) => m.id);
}

/** The meters of what was bought: the finest (`total`), else `grid`. */
function boughtIds(series) {
  const total = ids(series, 'total');
  return total.length ? total : ids(series, 'grid');
}

/** One hourly bucket: `{ used, bought, solar }` (kWh), or null when the
 *  bought meter has no reading that hour. */
export function hourOf(series, bucket) {
  const m = bucket?.meters ?? {};
  const sum = (list) => list.reduce((t, id) => t + (m[id]?.kwh ?? 0), 0);
  const bought = boughtIds(series);
  if (!bought.length || !bought.some((id) => typeof m[id]?.kwh === 'number')) return null;
  const b = sum(bought);
  const solar = sum(ids(series, 'solar'));
  const used = Math.max(0, b + solar - sum(ids(series, 'export')) - sum(ids(series, 'charge')) + sum(ids(series, 'discharge')));
  return { used, bought: b, solar, sun: Math.min(used, Math.max(0, used - b)) };
}

/** Today's 24 hours (null for those not reached yet), and the usual day:
 *  each hour's mean consumption over the days before today. */
export function dayHours(series, now) {
  const midnight = new Date(now);
  midnight.setHours(0, 0, 0, 0);
  const start = midnight.getTime();
  const today = new Array(24).fill(null);
  const sum = new Array(24).fill(0);
  const n = new Array(24).fill(0);
  for (const b of series?.buckets ?? []) {
    const h = hourOf(series, b);
    if (!h) continue;
    const hour = new Date(b.start).getHours();
    if (b.start >= start) {
      if (b.start <= now) today[hour] = h;
    } else if (b.start >= start - 7 * 24 * HOUR) {
      sum[hour] += h.used;
      n[hour] += 1;
    }
  }
  const usual = n.some((c) => c) ? sum.map((s, h) => (n[h] ? s / n[h] : 0)) : null;
  return { today: today.some(Boolean) ? today : null, usual, start };
}

/** Today so far: used, bought, from the sun, and the usual day up to the
 *  same minute (the current hour pro rata). */
export function totals(today, usual, now) {
  if (!today) return null;
  const d = new Date(now);
  const hour = d.getHours();
  const frac = (d.getMinutes() + d.getSeconds() / 60) / 60;
  let used = 0;
  let bought = 0;
  let sun = 0;
  for (const h of today) {
    if (!h) continue;
    used += h.used;
    bought += h.bought;
    sun += h.sun;
  }
  const wont = usual ? usual.slice(0, hour).reduce((t, v) => t + v, 0) + usual[hour] * frac : null;
  return {
    used,
    bought,
    sun,
    share: used > 0.05 ? Math.round((sun / used) * 100) : null,
    usual: wont,
    gap: wont > 0.05 ? Math.round(((used - wont) / wont) * 100) : null,
  };
}

/** The hour that used the most today (a past hour), or null. */
export function peak(today) {
  let best = null;
  (today ?? []).forEach((h, i) => {
    if (h && (best == null || h.used > today[best].used)) best = i;
  });
  return best;
}
