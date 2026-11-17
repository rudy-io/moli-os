// What a refused order says on screen. The server's words are English for
// what the hub itself refuses (`driver error: …`, `mirror house: …`, a bare
// `Forbidden`): said in the house's words instead. A driver's own refusal
// is already in them (its catalogue), shown without the hub's prefix.

/** `{ key }` for a sentence of the screen's catalogue, else `{ text }`. */
export function orderError(message) {
  const m = String(message ?? '').trim();
  if (/unreachable|injoignable|did not answer|no answer/i.test(m)) return { key: 'commun.appareil_injoignable' };
  if (/read-only|lecture seule|cannot be set/i.test(m)) return { key: 'commun.non_pilotable' };
  if (/^driver (unavailable|busy)$/i.test(m)) return { key: 'commun.pilote_indisponible' };
  if (/^mirror house\b/i.test(m)) return { key: 'commun.maison_miroir' };
  if (/^forbidden$/i.test(m)) return { key: 'commun.refuse' };
  const driver = m.match(/^driver error:\s*([\s\S]+)$/i);
  return { text: driver ? driver[1] : m };
}
