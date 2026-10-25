// The house's energy split between off-peak and peak hours, part by part
// (a radiator, a circuit), from an energy report (/api/energy/series).

/** A contract's period is off-peak (« HC », « Heures creuses »). */
export const offPeakPeriod = (name) => /hc|creuse/i.test(name);

/** Each part's off-peak and peak over `buckets`: `{ id, name, estimated,
 *  hc, hp, hcCost, hpCost, priced }`. An hour the contract split (its
 *  `by_period`, for the house) is split the same way for every part, at
 *  that hour's off-peak and peak prices; another hour by `share(bucket)`
 *  (the off-peak windows), at its average price. */
export function tariffSplit(parts, buckets, share) {
  const acc = parts.map((p) => ({ id: p.id, name: p.name, estimated: p.estimated, hc: 0, hp: 0, hcCost: 0, hpCost: 0, priced: false }));
  for (const b of buckets) {
    const by = Object.entries(b.by_period ?? {});
    const sum = (off, key) => by.filter(([name]) => offPeakPeriod(name) === off).reduce((t, [, a]) => t + (a[key] ?? 0), 0);
    const hcKwh = sum(true, 'kwh');
    const hpKwh = sum(false, 'kwh');
    const split = hcKwh + hpKwh > 0;
    const part = split ? hcKwh / (hcKwh + hpKwh) : share(b);
    const priced = split && by.some(([, a]) => a.cost != null);
    const hcPrice = hcKwh > 0 ? sum(true, 'cost') / hcKwh : 0;
    const hpPrice = hpKwh > 0 ? sum(false, 'cost') / hpKwh : 0;
    parts.forEach((p, i) => {
      const a = acc[i];
      const k = p.kwh(b);
      if (!k) return;
      a.hc += k * part;
      a.hp += k * (1 - part);
      if (priced) {
        a.priced = true;
        a.hcCost += k * part * hcPrice;
        a.hpCost += k * (1 - part) * hpPrice;
      } else {
        const c = p.cost(b);
        if (c == null) return;
        a.priced = true;
        a.hcCost += c * part;
        a.hpCost += c * (1 - part);
      }
    });
  }
  return acc.filter((a) => a.hc + a.hp > 0.0005).sort((x, y) => y.hc + y.hp - (x.hc + x.hp));
}
