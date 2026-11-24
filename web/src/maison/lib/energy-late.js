// A house whose meters report a day late (a Linky through Enedis, a Gazpar
// through GRDF) and that measures nothing live: « today » and « right now »
// stay empty, so the pages show the last day known instead, and say which
// days never came.

const LATE_ROLES = ['grid', 'total', 'gas'];

/** Nothing measured live: no supplier's meter read now, no power point. */
export function noLiveMeasure(summary) {
  if (!summary) return false;
  return !summary.live && !(summary.meters ?? []).some((m) => m.power);
}

/** A house shown by its last day known: the server says its electricity is
 *  read a day late (`last_days`), and nothing is measured live. */
export function lateHouse(summary) {
  return noLiveMeasure(summary) && lastDay(summary, 'electricity') != null;
}

/** The last day known of `energy` ('electricity' or 'gas'), if read late. */
export function lastDay(summary, energy) {
  return (summary?.last_days ?? []).find((d) => d.energy === energy) ?? null;
}

/** That day against its usual day, in %, when there is one. */
export function vsUsual(day) {
  const usual = day?.usual?.kwh;
  if (!usual || day.amount?.kwh == null) return null;
  return Math.round(((day.amount.kwh - usual) / usual) * 100);
}

/** The arc of a gauge: the day against its usual day (the usual day fills
 *  two thirds, a day half again as big fills it). */
export function usualFrac(day) {
  const kwh = day?.amount?.kwh;
  const usual = day?.usual?.kwh;
  if (kwh == null) return 0;
  if (!usual) return kwh > 0 ? 2 / 3 : 0;
  return Math.min(1, Math.max(0, kwh / (usual * 1.5)));
}

/**
 * The days a late meter never told, in a report by day: each meter with its
 * runs of missing days between its first and last day told (the days after
 * the last one are not missing, only not published yet).
 * @returns {{ meters: {id: string, name: string, runs: {from: number, to: number, days: number}[]}[], missing: Set<number> }}
 */
export function gaps(report) {
  const out = { meters: [], missing: new Set() };
  if (report?.step !== 'day') return out;
  const buckets = report.buckets ?? [];
  for (const m of (report.meters ?? []).filter((m) => m.history && LATE_ROLES.includes(m.role))) {
    const told = buckets.map((b) => b.meters?.[m.id] != null);
    const first = told.indexOf(true);
    const last = told.lastIndexOf(true);
    if (first < 0) continue;
    const runs = [];
    for (let i = first; i <= last; i++) {
      if (told[i]) continue;
      out.missing.add(i);
      const run = runs.at(-1);
      if (run && run.end === i - 1) {
        run.end = i;
        run.to = buckets[i].start;
        run.days++;
      } else runs.push({ end: i, from: buckets[i].start, to: buckets[i].start, days: 1 });
    }
    if (runs.length) out.meters.push({ id: m.id, name: m.name, runs: runs.map(({ from, to, days }) => ({ from, to, days })) });
  }
  return out;
}
