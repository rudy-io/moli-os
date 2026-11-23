// A room thermostat of Moli: a boost added to its setpoint for a while, and a
// setpoint held for a while in `auto` (crates/moli-heating/src/temporary.rs).

/** What is left until `text` (RFC 3339), in whole minutes rounded up; null when over or empty. */
export function minutesLeft(text, now) {
  const at = Date.parse(text ?? '');
  if (!Number.isFinite(at) || at <= now) return null;
  return Math.ceil((at - now) / 60_000);
}

/** Minutes as hours and minutes, the minutes on two digits once there is an hour. */
export function hoursMinutes(minutes) {
  const h = Math.floor(minutes / 60);
  const m = minutes % 60;
  return { h, m: h ? String(m).padStart(2, '0') : String(m) };
}

/** The boosts offered (+1, +2), within the room's maximum. */
export const boostChoices = (max) => [1, 2].filter((b) => Number.isFinite(max) && b <= max);

/** The room's boost and setpoint held, as `read(key)` gives its points; null when none. */
export function temporaryOf(read, now) {
  const by = read('boost');
  const boostLeft = minutesLeft(read('boost_until'), now);
  const holdLeft = minutesLeft(read('hold_until'), now);
  return {
    boost: by > 0 && boostLeft != null ? { by, until: Date.parse(read('boost_until')), left: boostLeft } : null,
    hold: holdLeft != null ? { until: Date.parse(read('hold_until')), left: holdLeft } : null,
  };
}
