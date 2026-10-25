// The VELUX gateway's automations, as its driver shows them (the point
// `automations`, JSON text): cards of `{ id, name, days (0 = Monday), ref
// ('time' | 'sunrise' | 'sunset'), min (minutes since midnight, or around
// the sun), end (a window airing until then, then closed), set (device →
// opening, 0 closed to 100 open), off (paused) }`.

export const POINT = 'automations';
export const WEEK = [0, 1, 2, 3, 4, 5, 6];
export const WEEKDAYS = [0, 1, 2, 3, 4];
export const WEEKEND = [5, 6];
/** Minutes around the sun the gateway takes. */
export const SUN_SPAN = 90;

/** The cards of the gateway's point, or null while unread. */
export function parseCards(text) {
  if (typeof text !== 'string') return null;
  try {
    const cards = JSON.parse(text);
    return Array.isArray(cards) ? cards : null;
  } catch {
    return null;
  }
}

/** Roughly when in the day a card acts (the sun's own time is unknown
 *  here: about 8 h and 19 h), to list the cards in the day's order. */
export const roughly = (card) => (card.ref === 'sunrise' ? 8 * 60 : card.ref === 'sunset' ? 19 * 60 : 0) + card.min;

export const sorted = (cards) => [...cards].sort((a, b) => roughly(a) - roughly(b) || String(a.name).localeCompare(String(b.name)));

/** A card acts on `day`. */
export const on = (card, day) => card.days.includes(day);

/** The cards as Moli lists them: VELUX keeps one card per room of its app,
 *  so the cards with one name, days, moment, end and pause are one line.
 *  `at`: their places in the list; `ids`: the cloud's cards it stands for;
 *  `set`: all their devices (a device on two of them: the first one says,
 *  as the gateway runs it). */
export function grouped(cards) {
  const lines = new Map();
  cards.forEach((c, i) => {
    const key = JSON.stringify([String(c.name).trim(), [...c.days].sort((x, y) => x - y), c.ref, c.min, c.end ?? null, !!c.off]);
    const ids = [c.id, ...(c.ids ?? [])].filter((id) => id != null);
    const line = lines.get(key);
    if (!line) lines.set(key, { ...c, at: [i], ids, set: { ...c.set } });
    else {
      line.at.push(i);
      line.ids.push(...ids.filter((id) => !line.ids.includes(id)));
      for (const [device, pct] of Object.entries(c.set)) if (!(device in line.set)) line.set[device] = pct;
    }
  });
  return [...lines.values()];
}

/** A line as the driver takes it back: its first card's `id`, the others'
 *  `ids` (each room gets its own card again, never one twice). */
export function toCloud(line) {
  const { at, ids, id, ...card } = line;
  const all = [...new Set([id, ...(ids ?? [])].filter((x) => x != null))];
  if (all.length) card.id = all[0];
  if (all.length > 1) card.ids = all.slice(1);
  return card;
}

/** Two lists the same, whatever their ids, order and split by room (a
 *  card the driver numbered or split, its devices sorted). */
export function same(a, b) {
  const norm = (cards) =>
    grouped(cards ?? [])
      .map((c) =>
        JSON.stringify([c.name, [...c.days].sort(), c.ref, c.min, c.end ?? null, !!c.off, Object.entries(c.set).sort(([x], [y]) => x.localeCompare(y))]),
      )
      .sort();
  return JSON.stringify(norm(a)) === JSON.stringify(norm(b));
}

/** `to` days get exactly what `from` has: the cards of `from` act on them
 *  too, the others no longer do (a card left with no day goes). */
export function copyDay(cards, from, to) {
  const targets = to.filter((d) => d !== from);
  return cards
    .map((c) => {
      const days = new Set(c.days);
      for (const d of targets) {
        if (days.has(from)) days.add(d);
        else days.delete(d);
      }
      return { ...c, days: [...days].sort((x, y) => x - y) };
    })
    .filter((c) => c.days.length);
}

/** « 07:15 » for a time of day. */
export const hhmm = (min) => `${String(Math.floor(min / 60)).padStart(2, '0')}:${String(min % 60).padStart(2, '0')}`;
export const fromHhmm = (text) => {
  const [h, m] = String(text).split(':').map(Number);
  return Number.isFinite(h) && Number.isFinite(m) ? h * 60 + m : null;
};
