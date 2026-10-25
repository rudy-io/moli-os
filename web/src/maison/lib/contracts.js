// The electricity contracts as the Énergie page edits them
// (/api/energy/contracts): each from its own day, its off-peak windows
// (« 01:38-07:08 », an end before its start runs past midnight), its prices
// (« HC » and « HP », or « Base » without off-peak hours) and its monthly
// subscription. Pure functions: the editor keeps drafts, these check them
// and turn them back into what the server stores.

const DAY = 1440;

/** « 01:38 » → 98 (« 24:00 » → 1440); null when it is no time. */
export function toMin(text) {
  const m = /^(\d{1,2}):(\d{2})$/.exec(String(text ?? '').trim());
  if (!m) return null;
  const [h, min] = [+m[1], +m[2]];
  return h <= 24 && min < 60 && h * 60 + min <= DAY ? h * 60 + min : null;
}

/** 98 → « 01:38 ». */
export const hhmm = (m) => `${String(Math.floor(m / 60)).padStart(2, '0')}:${String(m % 60).padStart(2, '0')}`;

/** Minutes of the day a window covers (past midnight on both sides). */
function minutes(w) {
  const out = [];
  if (w.from < w.to) for (let m = w.from; m < w.to; m++) out.push(m);
  else {
    for (let m = w.from; m < DAY; m++) out.push(m);
    for (let m = 0; m < w.to; m++) out.push(m);
  }
  return out;
}

/** The first two windows sharing a minute, as indexes, or null. */
export function overlap(windows) {
  const seen = new Array(DAY).fill(-1);
  for (const [i, w] of windows.entries()) {
    if (w.from == null || w.to == null || w.from === w.to) continue;
    for (const m of minutes(w)) {
      if (seen[m] >= 0 && seen[m] !== i) return [seen[m], i];
      seen[m] = i;
    }
  }
  return null;
}

/** A free stretch for a new window: the first `len` minutes on the hour
 *  that no window covers, else a quarter of an hour, else null. */
export function freeWindow(windows, len = 120) {
  const taken = new Array(DAY).fill(false);
  for (const w of windows) if (w.from != null && w.to != null && w.from !== w.to) for (const m of minutes(w)) taken[m] = true;
  for (const size of [len, 15]) {
    for (let start = 0; start < DAY; start += size === len ? 60 : 15) {
      let free = true;
      for (let m = start; m < start + size; m++) if (taken[m % DAY]) free = false;
      if (free) return { from: start, to: (start + size) % DAY };
    }
  }
  return null;
}

/** The server's contract → a draft (times as minutes, prices as numbers). */
export function toDraft(c) {
  const price = (name) => Object.entries(c.prices ?? {}).find(([k]) => k.replace(/[\s.]/g, '').toLowerCase() === name)?.[1] ?? null;
  return {
    from: c.from,
    windows: (c.offpeak ?? []).map((r) => {
      const [a, b] = r.split('-');
      return { from: toMin(a), to: toMin(b) };
    }),
    hc: price('hc'),
    hp: price('hp'),
    base: price('base'),
    fee: c.monthly_fee ?? null,
  };
}

/** A draft → the server's contract (windows in the order of the day). */
export function toContract(d) {
  const windows = [...d.windows].sort((a, b) => a.from - b.from);
  const prices = windows.length ? { HC: +d.hc, HP: +d.hp } : { Base: +d.base };
  const c = { from: d.from, prices };
  if (windows.length) c.offpeak = windows.map((w) => `${hhmm(w.from)}-${hhmm(w.to)}`);
  if (d.fee != null && d.fee !== '') c.monthly_fee = +d.fee;
  return c;
}

const ISO = /^\d{4}-\d{2}-\d{2}$/;
const priceOk = (v) => v != null && v !== '' && Number.isFinite(+v) && +v >= 0;

/** What is wrong with each draft: `[{ index, what, … }]`, `what` one of
 *  date, same_date, window, overlap, price, fee. Empty: ready to save. */
export function problems(drafts) {
  const out = [];
  const dates = new Map();
  for (const [index, d] of drafts.entries()) {
    if (!ISO.test(d.from ?? '') || Number.isNaN(Date.parse(d.from))) out.push({ index, what: 'date' });
    else if (dates.has(d.from)) out.push({ index, what: 'same_date' });
    else dates.set(d.from, index);
    const bad = d.windows.findIndex((w) => w.from == null || w.to == null || w.from === w.to);
    if (bad >= 0) out.push({ index, what: 'window', window: bad });
    else {
      const both = overlap(d.windows);
      if (both) out.push({ index, what: 'overlap', windows: both });
    }
    const prices = d.windows.length ? [d.hc, d.hp] : [d.base];
    if (!prices.every(priceOk)) out.push({ index, what: 'price' });
    if (d.fee != null && d.fee !== '' && !priceOk(d.fee)) out.push({ index, what: 'fee' });
  }
  return out;
}

/** The index of the draft in force on `day` (« 2026-10-25 »), or -1. */
export function inForce(drafts, day) {
  let best = -1;
  for (const [i, d] of drafts.entries()) {
    if (ISO.test(d.from ?? '') && d.from <= day && (best < 0 || d.from > drafts[best].from)) best = i;
  }
  return best;
}

/** A day as « 2026-10-25 », in the browser's (the house's) time zone. */
export function isoDay(ts) {
  const d = new Date(ts);
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
}

/** Where each change of period falls along the day (0 to 1), to write it
 *  under a strip: `[{ at, text, row }]`, a label too close to the one
 *  before it dropped to a second row. `room` = the share a label needs. */
export function changeMarks(windows, room = 0.12) {
  // Where one window ends as another begins, the period does not change.
  const starts = new Set(windows.map((w) => w.from % DAY));
  const ends = new Set(windows.map((w) => w.to % DAY));
  const marks = [...new Set([...starts].filter((m) => !ends.has(m)).concat([...ends].filter((m) => !starts.has(m))))].sort((a, b) => a - b);
  const last = [-1, -1];
  return marks.map((m) => {
    const at = m / DAY;
    const row = at - last[0] >= room || last[0] < 0 ? 0 : at - last[1] >= room || last[1] < 0 ? 1 : 0;
    last[row] = at;
    return { at, text: hhmm(m), row };
  });
}
