/** Point contracts shared by controls; these are UI hints, not authorization. */
export const pointOf = (device, key) => device?.points?.find((point) => point.key === key) ?? null;
export const canWrite = (device, key) => device?.online !== false && pointOf(device, key)?.access?.write === true;

/** Controls may opt into their legacy limits, but never replace invalid metadata. */
export function numericRange(device, key, defaultStep = 0.5, fallback = null) {
  const kind = pointOf(device, key)?.kind;
  if (kind?.type !== 'numeric') return null;
  const min = kind.min ?? fallback?.min;
  const max = kind.max ?? fallback?.max;
  const step = kind.step ?? defaultStep;
  if (![min, max, step, max - min].every(Number.isFinite) || max <= min || step <= 0) return null;
  // A visual fallback is not a declared step origin. The core validates limits,
  // not steps; UI controls use the declared minimum, or zero when it is absent.
  const range = { min, max, step, stepBase: kind.min ?? 0 };
  const first = quantize(min, range);
  const last = quantize(max, range);
  return first != null && last != null && first < last ? { ...range, first, last } : null;
}

/** Snap to the UI's step grid while retaining the contract's validity limits. */
export function quantize(value, range) {
  if (!Number.isFinite(value) || !range) return null;
  const { min, max, step, stepBase = min } = range;
  const first = Math.ceil((min - stepBase) / step - 1e-9);
  const last = Math.floor((max - stepBase) / step + 1e-9);
  if (!Number.isFinite(first) || !Number.isFinite(last) || first > last) return null;
  const index = Math.min(last, Math.max(first, Math.round((value - stepBase) / step)));
  const result = Math.round((stepBase + index * step) * 1e10) / 1e10;
  return Number.isFinite(result) ? result : null;
}

export function writableChoices(device, key) {
  const kind = pointOf(device, key)?.kind;
  return canWrite(device, key) && kind?.type === 'enum' && Array.isArray(kind.values)
    ? [...new Set(kind.values.filter((value) => typeof value === 'string' && value.length))]
    : [];
}

/** Reading power and commanding it are separate capabilities. */
export function powerPoint(device, writable = true) {
  const points = device?.points?.filter((point) => !writable || point.access?.write) ?? [];
  const point = points.find((point) => point.semantic === 'on_off' && point.kind.type === 'binary')
    ?? points.find((point) => ['on', 'switch_1', 'switch', 'state'].includes(point.key)
      && (point.kind.type === 'binary' || point.kind.type === 'enum'));
  return point?.key ?? null;
}

const BRIDGE_KINDS = new Map([
  ['Airzone system', 'climate'],
  ['Overkiz RollerShutter', 'cover'],
  ['Overkiz Gate', 'cover'],
  ['Overkiz Window', 'cover'],
  ['Home Assistant light', 'light'],
  ['Home Assistant switch', 'plug'],
  ['Home Assistant media_player', 'speaker'],
  ['Home Assistant climate', 'climate'],
  ['Home Assistant cover', 'cover'],
]);

/** Stable adapter hints help an on/off-only light; never used by the guard. */
export const knownDeviceKind = (device) => BRIDGE_KINDS.get(device?.model) ?? null;
