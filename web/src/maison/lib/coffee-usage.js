// Coffee drunk day by day, from the machine's own counters (`brews.<drink>`,
// one per drink, kept by the machine for ever): each rise of a counter is
// cups brewed, counted on the day Moli saw it.

/** Drinks without coffee: not counted as cups. */
export const NO_COFFEE = ['hot_water', 'milk_foam'];

/** The counters that count coffee, among a machine's points. */
export const cupCounters = (points) =>
  (points ?? []).map((p) => p.key).filter((k) => k.startsWith('brews.') && !NO_COFFEE.includes(k.slice(6)));

const DAY = 86_400_000;
/** Local midnight of the day `ts` falls in. */
const midnight = (ts) => new Date(new Date(ts).setHours(0, 0, 0, 0)).getTime();

/** `days` days up to today's (oldest first): each day's start, its cups and
 *  its cups by drink; `series` maps a drink to its counter's history
 *  (`{before, raw}`, as `/api/history` gives it). A counter seen for the
 *  first time is where counting starts, not cups; a counter that went back
 *  (a machine reset) starts again from there. `since`: when the first count
 *  was seen (counting cannot go further back). */
export function daily(series, days, now = Date.now()) {
  const today = midnight(now);
  // Midnights step back day by day (a 23- or 25-hour day near a clock change).
  const starts = Array.from({ length: days }, (_, i) => midnight(today - (days - 1 - i) * DAY + DAY / 2));
  const out = starts.map((start) => ({ start, total: 0, by: {} }));
  let since = null;
  for (const [drink, s] of Object.entries(series ?? {})) {
    let last = s?.before?.[1] ?? null;
    if (s?.before) since = Math.min(since ?? Infinity, s.before[0]);
    for (const [ts, v] of s?.raw ?? []) {
      if (typeof v !== 'number') continue;
      since = Math.min(since ?? Infinity, ts);
      const cups = last == null ? 0 : v - last;
      last = v;
      if (cups <= 0) continue;
      const day = out.findLastIndex((d) => d.start <= ts);
      if (day < 0) continue;
      out[day].total += cups;
      out[day].by[drink] = (out[day].by[drink] ?? 0) + cups;
    }
  }
  return { days: out, since };
}

/** The cups of several days, by drink, most drunk first. */
export function byDrink(days) {
  const all = {};
  for (const d of days) for (const [drink, n] of Object.entries(d.by)) all[drink] = (all[drink] ?? 0) + n;
  return Object.entries(all)
    .map(([drink, n]) => ({ drink, n }))
    .sort((a, b) => b.n - a.n || a.drink.localeCompare(b.drink));
}

/** Cups a day on average over the days counted (those since counting began). */
export function average(days, since) {
  const counted = days.filter((d) => since != null && d.start + DAY > since);
  return counted.length ? counted.reduce((sum, d) => sum + d.total, 0) / counted.length : null;
}
