// Apply the complete driver status carried by the event stream.
export function applyDriverStatus(drivers, event) {
  const driver = drivers.find((item) => item.instance === event.instance);
  const status = { status: event.status, warning: event.warning ?? null };
  if (driver) Object.assign(driver, status);
  else drivers.push({ instance: event.instance, kind: '?', ...status });
}
