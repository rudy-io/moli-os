// A coffee machine (Jura): drinks to brew and a maintenance cockpit.
import { t } from '../../lib/i18n.svelte.js';

const has = (d, key, write = false) => d.points.some((p) => p.key === key && (!write || p.access?.write));

/** A machine that brews drinks and keeps maintenance counts. */
export const isCoffee = (d) => has(d, 'brew', true) && has(d, 'cleaning_need');

/** A state's words and tone (`status` point). */
export const STATE_TONE = {
  ready: 'good',
  energy_saving: '',
  heating_up: 'warm',
  rinsing: 'cool',
  brewing: 'warm',
  maintenance: 'cool',
  programming: '',
  busy: '',
  switching_off: '',
  attention: 'alert',
};

export const stateWord = (s) => (s ? t(`salon.cafe.etat.${s}`) : t('salon.cafe.eteinte'));

/** A drink's name: translated when known, its code otherwise. */
export function drinkName(slug) {
  const m = /^drink_([0-9a-f]{2})$/.exec(slug ?? '');
  return m ? t('pilotes.jura.boisson_code', { code: m[1].toUpperCase() }) : t(`pilotes.jura.boisson.${slug}`);
}

export const programName = (p) => t(`salon.cafe.programme.${p}`);

/** A setting's value for people: languages and words translated, times spaced. */
export function settingWord(v) {
  if (typeof v !== 'string') return String(v ?? '—');
  if (/^\d+(min|h)$/.test(v)) return v.replace(/(min|h)$/, ' $1');
  return v === 'ml' || v === 'oz' ? v : t(`salon.cafe.valeur.${v}`);
}

/** A recipe's fields in the order people read them. */
export const FIELDS = ['strength', 'water', 'bypass', 'milk_foam', 'milk_break', 'temperature'];

/** Temperature: 0 low, 1 normal, 2 high. */
export const TEMPERATURES = ['basse', 'normale', 'haute'];

/** A field's value for people: « 8/10 », « 45 ml », « 12 s », « Haute ». */
export function fieldWord(key, f) {
  if (f?.value == null) return '—';
  if (key === 'temperature') return t(`salon.cafe.temperature.${TEMPERATURES[f.value] ?? 'normale'}`);
  if (key === 'strength') return `${f.value}/${f.max}`;
  return f.unit ? `${f.value} ${f.unit}` : String(f.value);
}
