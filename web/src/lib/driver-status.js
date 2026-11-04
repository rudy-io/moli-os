// Apply the complete driver status carried by the event stream.
export function applyDriverStatus(drivers, event) {
  const driver = drivers.find((item) => item.instance === event.instance);
  const status = { status: event.status, warning: event.warning ?? null };
  if (driver) Object.assign(driver, status);
  else drivers.push({ instance: event.instance, kind: '?', ...status });
}

// A hot-added integration announces its identity before its first readings.
// Keep a status already received if events are replayed across a reconnect.
export function applyDriverRegistration(drivers, event) {
  const driver = drivers.find((item) => item.instance === event.instance);
  const identity = {kind:event.kind, integration:event.integration ?? null};
  if (driver) Object.assign(driver, identity);
  else drivers.push({instance:event.instance, ...identity, status:{state:'starting'}, warning:null});
}

// Withdrawal removes the live task; persisted configuration/history is separate.
export function applyDriverUnregistration(drivers, event) {
  const index = drivers.findIndex((item) => item.instance === event.instance);
  if (index !== -1) drivers.splice(index, 1);
}
