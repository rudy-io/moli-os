// The home battery right now (`summary.battery` of /api/energy), pure.

/** Below this, an inverter's standby reading: the battery is at rest (W). */
export const IDLE_W = 20;

/** The battery's state from the summary and the points' live values
 *  (`current(point)` → number or null): charge (%), power (W, positive
 *  while charging, whatever the point's own sign), what it does
 *  (`charge`, `decharge`, `repos`), what the charge holds (kWh) and a tone
 *  for the gauge. `null` without a battery or anything to show. */
export function batteryNow(summary, current = () => null) {
  const b = summary?.battery;
  if (!b) return null;
  const valid = (v) => (typeof v === 'number' && Number.isFinite(v) ? v : null);
  const live = (point) => (point ? valid(current(point)) : null);
  const socRaw = live(b.soc_point) ?? valid(b.soc);
  const soc = socRaw != null && socRaw >= 0 && socRaw <= 100 ? socRaw : null;
  const raw = live(b.power_point);
  const power = raw == null ? valid(b.power_w) : b.discharge_positive ? -raw : raw;
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
  const [inGrid, sun, outGrid] = [pos(bought), pos(made), pos(sent)];
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
