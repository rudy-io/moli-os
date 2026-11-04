export function pointCanWrite(device, point) {
  return device?.online === true && point?.access?.write === true;
}

// Match the core's Unicode-character bound, not JavaScript's UTF-16 length.
// Device-specific restrictions (for example an Airzone name) remain in its driver.
export function textPointCommand(device, point, draft, pending) {
  if (!pointCanWrite(device, point) || pending || point.kind.type !== 'text'
    || typeof draft !== 'string' || [...draft].length > 4000
    || draft === device.state?.[point.key]?.value) return null;
  return { point: `${device.id}/${point.key}`, value: draft };
}
