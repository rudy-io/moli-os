// The home battery right now (`summary.battery` of /api/energy), pure.

/** Below this, an inverter's standby reading: the battery is at rest (W). */
export const IDLE_W = 20;

const valid = (v) => (typeof v === 'number' && Number.isFinite(v) ? v : null);

/** The battery's power from the `charge` / `discharge` meters' own power
 *  points (`power` in `summary.meters`), like `solar` gives the production:
 *  what goes in less what comes out (W, positive while charging). For a
 *  battery that says both as positive powers (a Jackery's `charge_power`
 *  and `discharge_power`). Each point once, live first, else the summary's
 *  value; `null` when none reads. */
export function metersPower(meters = [], current = () => null) {
  const seen = new Set();
  let out = null;
  for (const m of meters) {
    const sign = m.role === 'charge' ? 1 : m.role === 'discharge' ? -1 : 0;
    if (!sign || !m.power || seen.has(m.power)) continue;
    seen.add(m.power);
    const w = valid(current(m.power)) ?? valid(m.power_w);
    // A battery at rest may read a few negative watts: nothing moves.
    if (w != null) out = (out ?? 0) + sign * Math.max(0, w);
  }
  return out;
}

/** The battery's state from the summary and the points' live values
 *  (`current(point)` → number or null): charge (%), power (W, positive
 *  while charging, whatever the point's own sign; without a `power` point,
 *  from the `charge` / `discharge` meters), what it does (`charge`,
 *  `decharge`, `repos`), what the charge holds (kWh) and a tone for the
 *  gauge. `null` without a battery or anything to show. */
export function batteryNow(summary, current = () => null) {
  const b = summary?.battery;
  if (!b) return null;
  const live = (point) => (point ? valid(current(point)) : null);
  const socRaw = live(b.soc_point) ?? valid(b.soc);
  const soc = socRaw != null && socRaw >= 0 && socRaw <= 100 ? socRaw : null;
  const raw = live(b.power_point);
  const power = !b.power_point
    ? (metersPower(summary.meters, current) ?? valid(b.power_w))
    : raw == null
      ? valid(b.power_w)
      : b.discharge_positive
        ? -raw
        : raw;
  if (soc == null && power == null) return null;
  const state = power == null ? null : power > IDLE_W ? 'charge' : power < -IDLE_W ? 'decharge' : 'repos';
  const stored = soc != null && b.capacity_kwh ? (b.capacity_kwh * soc) / 100 : null;
  const tone = soc == null ? null : soc < 20 ? 'low' : soc < 40 ? 'mid' : 'ok';
  return {
    soc,
    power,
    state,
    stored,
    capacity: b.capacity_kwh ?? null,
    tone,
    charged: summary.today?.battery_charge?.kwh ?? null,
    discharged: summary.today?.battery_discharge?.kwh ?? null,
  };
}

/** Where the power goes right now, with a battery (W, all ≥ 0): the live
 *  meters say how much the grid gives and takes, the panels make and the
 *  battery takes (`batteryW`, positive while charging), not who feeds whom.
 *  Like most energy dashboards: the battery charges from the sun first,
 *  then the grid; the sun then feeds the house, its surplus goes out; a
 *  discharging battery feeds the house, and the grid only for what the sun
 *  did not send out. */
export function splitFlows({ bought = 0, made = 0, sent = 0, batteryW = 0 }) {
  const pos = (v) => (v != null && Number.isFinite(v) ? Math.max(0, v) : 0);
  const [inGrid, outGrid] = [pos(bought), pos(sent)];
  // Production unknown (a solar meter without power, a silent inverter):
  // what goes out came from the sun, as before there was a battery.
  const sun = made == null || !Number.isFinite(made) ? outGrid : pos(made);
  const charge = pos(batteryW);
  const discharge = pos(-(batteryW ?? 0));
  const solarToBattery = Math.min(charge, sun);
  const gridToBattery = Math.min(charge - solarToBattery, inGrid);
  const solarLeft = sun - solarToBattery;
  const solarToGrid = Math.min(outGrid, solarLeft);
  const batteryToGrid = Math.min(discharge, outGrid - solarToGrid);
  return {
    solarToBattery,
    gridToBattery,
    solarToGrid,
    solarToHouse: solarLeft - solarToGrid,
    gridToHouse: inGrid - gridToBattery,
    batteryToGrid,
    batteryToHouse: discharge - batteryToGrid,
  };
}
