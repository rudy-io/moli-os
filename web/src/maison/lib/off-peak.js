/** This month, the share of the grid billed off-peak (%), or `null`.
 *  Off-peak is read from the grid meters named for it (« HC », « creuses »):
 *  without one, the house has a single price, or its meter does not split
 *  the hours, and « 0 % » would only be noise. */
export function offPeakShare(summary) {
  const grid = (summary?.meters ?? []).filter((m) => m.role === 'grid');
  const offPeak = grid.filter((m) => /hc|creuse/i.test(m.id + m.name));
  if (!offPeak.length) return null;
  const kwh = (list) => list.reduce((t, m) => t + (summary.month?.meters?.[m.id]?.kwh ?? 0), 0);
  const all = kwh(grid);
  return all > 1 ? Math.round((kwh(offPeak) / all) * 100) : null;
}
