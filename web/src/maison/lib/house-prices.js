// The house's list of prices as the dashboard edits it: one row per price,
// a day (YYYY-MM-DD, local) and the price as typed (« 0,2016 »).

/** A moment's local day, as a date field writes it. */
export function isoDay(ts) {
  const d = new Date(ts);
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
}

/** A typed price as a number (a comma or a dot). */
export const typedNumber = (text) => Number(String(text ?? '').trim().replace(',', '.'));

/** A price per kWh: above zero, never typed in cents (5 at most). */
export function priceOk(text) {
  const v = typedNumber(text);
  return String(text ?? '').trim() !== '' && Number.isFinite(v) && v > 0 && v <= 5;
}

/** The rows to edit, from the prices (oldest first), each price shown with `show`. */
export const rowsFrom = (prices, show) =>
  (prices ?? []).map((p) => ({ day: isoDay(p.since), since: p.since, typed: show(p.price) }));

/** A row's start: its old moment while its day is unchanged (a price set at
 *  6:47 stays at 6:47), else that day's local midnight. */
export function startOf(row) {
  if (row.since != null && isoDay(row.since) === row.day) return row.since;
  const [y, m, d] = row.day.split('-').map(Number);
  return new Date(y, m - 1, d).getTime();
}

/** Every row has a day and a price, and no two begin the same day. */
// At least one price: the house refuses an empty list (every cost would go).
export const rowsOk = (rows) =>
  Array.isArray(rows) &&
  rows.length > 0 &&
  rows.every((r) => /^\d{4}-\d{2}-\d{2}$/.test(r.day) && priceOk(r.typed)) &&
  new Set(rows.map((r) => r.day)).size === rows.length;

/** What `PUT /api/energy/prices` takes. */
export const pricesFrom = (rows) => rows.map((r) => ({ since: startOf(r), price: typedNumber(r.typed) }));
