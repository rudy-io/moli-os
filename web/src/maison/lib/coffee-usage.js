// Coffee drunk day by day or hour by hour, from the machine's own counters
// (`brews.<drink>`, one per drink, kept by the machine for ever): each rise
// of a counter is cups brewed, counted when Moli saw it (the counters are
// read as soon as a drink ends).
import { seriesColor } from './palette.js';

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
  const { buckets, since } = counted(series, starts);
  return { days: buckets, since };
}

/** Today hour by hour, from `from` o'clock (6, or the hour of an earlier
 *  cup) to 23 h: each hour's start, its cups and its cups by drink. */
export function hourly(series, now = Date.now(), from = 6) {
  const today = midnight(now);
  const hour = (h) => new Date(today).setHours(h, 0, 0, 0);
  const all = counted(
    series,
    Array.from({ length: 24 }, (_, h) => hour(h)),
  );
  const first = all.buckets.findIndex((b) => b.total > 0);
  const start = first < 0 ? from : Math.min(from, first);
  return { hours: all.buckets.slice(start), since: all.since };
}

/** Cups in buckets starting at `starts` (oldest first, each up to the next,
 *  the last up to now). */
function counted(series, starts) {
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
      const at = out.findLastIndex((d) => d.start <= ts);
      if (at < 0) continue;
      out[at].total += cups;
      out[at].by[drink] = (out[at].by[drink] ?? 0) + cups;
    }
  }
  return { buckets: out, since };
}

/** Each drink's colour, as the electricity chart gives its parts theirs:
 *  the palette's slots in order of what the machine has brewed of each over
 *  its life (`counts`: drink → count), the most brewed first. The same
 *  wherever the drinks are drawn, whatever the period. */
export function drinkColors(counts) {
  const ranked = Object.entries(counts ?? {})
    .filter(([drink, n]) => !NO_COFFEE.includes(drink) && n > 0)
    .sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]));
  return new Map(ranked.map(([drink], i) => [drink, seriesColor(i)]));
}

/** The counters' history over the last `hours` hours (`keys`: the
 *  machine's `brews.<drink>` points): drink → `{before, raw}`. Throws an
 *  error with the HTTP `status` when the history cannot give it. */
export async function cupHistory(id, keys, hours) {
  const read = await Promise.all(
    keys.map(async (key) => {
      const r = await fetch(`/api/history?point=${encodeURIComponent(`${id}/${key}`)}&hours=${hours}&points=2000`);
      if (!r.ok) throw Object.assign(new Error(`HTTP ${r.status}`), { status: r.status });
      return [key.slice(6), await r.json()];
    }),
  );
  return Object.fromEntries(read);
}

/** The cups of several days (or hours), by drink, most drunk first. */
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
