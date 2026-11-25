// The internet box on the Infra page, from points the house already reads
// (the `freebox` driver, a box seen through Home Assistant, any driver),
// named in home.json. Nothing is guessed from names: only what is named is
// shown, and a rate is converted only when its unit is known.
//
//   "infra": { "box": {
//     "name": "Freebox",
//     "down": "<point>", "up": "<point>",  // rates, in the point's own unit
//     "rate_unit": "kB/s",                 // only for points that declare none
//     "free": "<point>",                   // free space, in percent
//     "degraded": "<point>",               // true: the disk array is degraded
//     "wifi": "<point>",                   // true: Wi-Fi on
//     "temperatures": [{ "label": "Disque", "point": "<point>", "warn": 50, "alert": 55 }],
//     "fans": [{ "label": "Ventilateur", "point": "<point>", "warn": 1800 }]
//   } }

/** `<device>/<key>` split in two, or null. */
function split(point) {
  if (typeof point !== 'string') return null;
  const i = point.lastIndexOf('/');
  return i > 0 ? [point.slice(0, i), point.slice(i + 1)] : null;
}

/** `<device>/<key>` → its value, through `value(device, key)`. */
export function reader(value) {
  return (point) => {
    const parts = split(point);
    return parts ? value(parts[0], parts[1]) : null;
  };
}

/** `<device>/<key>` → the unit the point declares, through `device(id)`. */
export function unitReader(device) {
  return (point) => {
    const parts = split(point);
    if (!parts) return null;
    const unit = device(parts[0])?.points?.find((p) => p.key === parts[1])?.unit;
    return typeof unit === 'string' && unit ? unit : null;
  };
}

/** Mbit/s in one unit of each rate. */
const TO_MBITS = { 'bit/s': 1e-6, 'B/s': 8e-6, 'kbit/s': 0.001, 'kB/s': 0.008, 'Mbit/s': 1, 'MB/s': 8, 'Gbit/s': 1000 };

/** Mbit/s in one `unit`, or null when it is not a rate's unit. */
export function mbitsFactor(unit) {
  return typeof unit === 'string' && Object.hasOwn(TO_MBITS, unit) ? TO_MBITS[unit] : null;
}

/** A rate → Mbit/s (one decimal); null when the unit is not one of a rate. */
export function mbits(rate, unit) {
  const factor = mbitsFactor(unit);
  if (typeof rate !== 'number' || !Number.isFinite(rate) || factor === null) return null;
  return Math.round(rate * factor * 10) / 10;
}

/** What needs a look on the box: a hot temperature, a fast fan, a degraded
 *  array, a nearly full disk. `read(point)` gives a point's value. */
export function boxIssues(box, read, t) {
  if (!box) return [];
  const name = box.name ?? t('systeme.infra.box.titre');
  const out = [];
  for (const s of box.temperatures ?? []) {
    const x = read(s.point);
    if (typeof x === 'number' && typeof s.alert === 'number' && x >= s.alert) {
      out.push(t('systeme.infra.box.alerte_chaud', { name, label: s.label, temp: Math.round(x) }));
    }
  }
  for (const f of box.fans ?? []) {
    const x = read(f.point);
    if (typeof x === 'number' && typeof f.warn === 'number' && x >= f.warn) {
      out.push(t('systeme.infra.box.alerte_ventilateur', { name, label: f.label, rpm: Math.round(x) }));
    }
  }
  if (read(box.degraded) === true) out.push(t('systeme.infra.box.alerte_raid', { name }));
  const free = read(box.free);
  if (typeof free === 'number' && free <= 10) out.push(t('systeme.infra.box.alerte_plein', { name, pct: Math.round(100 - free) }));
  return out;
}
