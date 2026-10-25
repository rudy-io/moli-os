// The house's energy split between off-peak and peak hours, part by part
// (a radiator, a circuit), from an energy report (/api/energy/series).

/** A contract's period is off-peak (« HC », « Heures creuses »). */
export const offPeakPeriod = (name) => /hc|creuse/i.test(name);

const NONE = { hc: 0, hp: 0, hcCost: 0, hpCost: 0 };

/** A meter's off-peak and peak in a bucket, as the contract split each of
 *  its hours (`meter_periods`): `{ hc, hp, hcCost, hpCost }`, or null when
 *  the bucket does not say its periods (no dated contract then). */
export function meterSplit(bucket, id) {
  const periods = bucket.meter_periods?.[id];
  if (!periods) return (bucket.meters?.[id]?.kwh ?? 0) > 0 ? null : { ...NONE };
  const by = Object.entries(periods);
  const sum = (off, key) => by.filter(([name]) => offPeakPeriod(name) === off).reduce((t, [, a]) => t + (a[key] ?? 0), 0);
  return { hc: sum(true, 'kwh'), hp: sum(false, 'kwh'), hcCost: sum(true, 'cost'), hpCost: sum(false, 'cost') };
}

/** Splits added (`sign` -1 takes away, never below zero); null stays null. */
export function addSplits(splits, sign = 1) {
  if (splits.some((s) => s == null)) return null;
  const out = { ...NONE };
  for (const [i, s] of splits.entries()) {
    for (const k of Object.keys(out)) out[k] += i && sign < 0 ? -s[k] : s[k];
  }
  for (const k of Object.keys(out)) out[k] = Math.max(0, out[k]);
  return out;
}

/** Each part's off-peak and peak over `buckets`: `{ id, name, estimated,
 *  hc, hp, hcCost, hpCost, priced }`. A part that knows its own split
 *  (`split(bucket)`, the contract hour by hour for that meter) takes it;
 *  otherwise an hour the contract split (its `by_period`, for the house)
 *  is split the same way for every part, at that hour's off-peak and peak
 *  prices; another hour by `share(bucket)` (the off-peak windows), at its
 *  average price. */
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
      const own = p.split?.(b);
      if (own) {
        a.hc += own.hc;
        a.hp += own.hp;
        a.hcCost += own.hcCost;
        a.hpCost += own.hpCost;
        a.priced ||= own.hcCost + own.hpCost > 0;
        return;
      }
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
