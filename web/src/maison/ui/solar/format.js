// Figures in French, for the solar demo.
import { t, locale } from '../../../lib/i18n.svelte.js';

const num = (v, digits = 0, min = digits) =>
  v.toLocaleString(locale(), { maximumFractionDigits: digits, minimumFractionDigits: min });

const ok = (v) => v != null && Number.isFinite(v);

/** A figure we do not have (never an em dash in the interface). */
const NONE = '–';

/** « 54 % » from a share (0.54). */
export const pct = (v) => (ok(v) ? t('energie.pct', { n: Math.round(v * 100) }) : NONE);

/** « +12 pts » from percentage points. */
export function pts(v) {
  if (!ok(v)) return NONE;
  const n = Math.round(v);
  const abs = Math.abs(n);
  return `${n > 0 ? '+' : n < 0 ? '−' : ''}${t('energie.solaire.pts', { count: abs > 1 ? 2 : 1, n: abs })}`;
}

/** « 38 € » (cents below 1 €). */
export const eur = (v) => (ok(v) ? `${num(v, Math.abs(v) < 1 ? 2 : 0)} €` : NONE);

/** « +12 € » for a gain. */
export const eurDelta = (v) => (ok(v) ? `${v >= 0 ? '+' : '−'}${eur(Math.abs(v))}` : NONE);

/** « 4,2 kW ». */
export const kw = (v) => (ok(v) ? `${num(v, 1)} kW` : NONE);

/** « 12 kWh », « 3,4 kWh ». */
export const kwhs = (v) => (ok(v) ? `${num(v, Math.abs(v) < 10 ? 1 : 0)} kWh` : NONE);

/** « 0,04 €/kWh ». */
export const perKwh = (v) => (ok(v) ? `${num(v, 3, 2)} €/kWh` : NONE);

/** « 22 h → 2 h ». */
export const hours = (w) => (w ? t('energie.solaire.plage', { from: w.from, to: w.to }) : NONE);

/** « Mardi 6 octobre », in the house's time zone. */
export function dayName(ts, timeZone, short = false) {
  const opts = short ? { weekday: 'short', day: 'numeric', month: 'short' } : { weekday: 'long', day: 'numeric', month: 'long' };
  let s;
  try {
    s = new Date(ts).toLocaleDateString(locale(), { ...opts, timeZone: timeZone ?? undefined });
  } catch {
    s = new Date(ts).toLocaleDateString(locale(), opts);
  }
  return s.charAt(0).toUpperCase() + s.slice(1);
}

// ---- the words of the simulation ---------------------------------------------------
// solar-sim.js says what happens (kinds, windows, figures); these say it in
// the house's language.

/** « clair » from a sky kind (`clair`, `voile`, `couvert`). */
export const skyKind = (kind) => t('energie.solaire.ciel.' + kind);

/** « ciel clair » from a sky kind. */
export const skyName = (kind) => t('energie.solaire.ciel_phrase', { kind: skyKind(kind) });

/** A load's name: the meter's, or the water heater the simulation imagines. */
export const loadName = (l) => l.name ?? t('energie.solaire.chauffe_eau');

/** What Moli would do with a load, from its kinds (« Recharger la voiture »). */
export function verbOf(kinds) {
  const verb = kinds.map((k) => t('energie.solaire.verbe.' + k)).join(t('energie.et'));
  return verb.charAt(0).toUpperCase() + verb.slice(1);
}

/** Why a load can move, from its kinds (or the imagined water heater). */
export const noteOf = (kinds, hypothetical) =>
  hypothetical ? t('energie.solaire.note.hypothetique') : kinds.map((k) => t('energie.solaire.note.' + k)).join(' ');

/** An idea (from `proposals`) in words: `{ title, text, note }`. */
export function ideaWords(idea) {
  if (idea.type === 'load') {
    const from = idea.window.from;
    const to = idea.window.to;
    return {
      title: t('energie.solaire.idee.charge_titre', { verb: verbOf(idea.kinds), from, to }),
      text: [
        idea.usual ? t('energie.solaire.idee.habituel', { from: idea.usual.from, to: idea.usual.to }) : '',
        t('energie.solaire.idee.charge_texte', { kwh: Math.round(idea.kwh) }),
      ]
        .filter(Boolean)
        .join(' '),
      note: noteOf(idea.kinds, idea.hypothetical),
    };
  }
  let note;
  if (idea.years == null) note = t('energie.solaire.idee.batterie_note_vide');
  else {
    const params = { cost: Math.round(idea.cost).toLocaleString(locale()), rate: idea.rate, years: Math.round(idea.years) };
    note = t(idea.years > 15 ? 'energie.solaire.idee.batterie_note_longue' : 'energie.solaire.idee.batterie_note', params);
  }
  return {
    title: t('energie.solaire.idee.batterie_titre', { cap: idea.battery }),
    text: t('energie.solaire.idee.batterie_texte', { kwh: Math.round(idea.back) }),
    note,
  };
}
