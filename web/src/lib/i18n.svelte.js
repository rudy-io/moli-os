// The house's words in its language: locales/<language>/<area>.json, shared
// with the server (crate moli-i18n). French ships in the bundle, it is the
// fallback; another language is fetched once, when the house speaks it.

const french = import.meta.glob('../../../locales/fr/*.json', { eager: true, import: 'default' });
const others = import.meta.glob(['../../../locales/*/*.json', '!../../../locales/fr/*.json'], { import: 'default' });

const fallback = Object.assign({}, ...Object.values(french));

export const i18n = $state({ language: 'fr', words: fallback });

/** The languages the dashboard can speak. */
export const languages = ['fr', ...new Set(Object.keys(others).map((p) => p.split('/').at(-2)))].sort();

/** Speaks `language` from now on (unknown: French stays). */
export async function setLanguage(language) {
  if (!language || language === i18n.language) return;
  if (language === 'fr') {
    i18n.words = fallback;
    i18n.language = 'fr';
  } else {
    const loaders = Object.entries(others).filter(([path]) => path.split('/').at(-2) === language);
    if (!loaders.length) return;
    const parts = await Promise.all(loaders.map(([, load]) => load()));
    i18n.words = { ...fallback, ...Object.assign({}, ...parts) };
    i18n.language = language;
  }
  if (typeof document !== 'undefined') document.documentElement.lang = language;
}

/** The word for `key`: `{name}` replaced by `params.name`; with
 *  `params.count`, `key_one` or `key_other` first. A missing word shows its
 *  key (visible, so found). */
export function t(key, params) {
  const words = i18n.words;
  let text = words[key];
  // The language's own rule: in French 0 and 1 are singular, in English only 1.
  if (params?.count != null) text = words[`${key}_${plural(Number(params.count))}`] ?? words[`${key}_other`] ?? text;
  if (text == null) return key;
  // A function, not a string: a value holding « $& » stays as it is.
  if (params) for (const [name, value] of Object.entries(params)) text = text.replaceAll(`{${name}}`, () => String(value));
  return text;
}

const rules = new Map();
/** `one` or `other` (another category, like `few`, falls back to `other`). */
function plural(n) {
  const lang = i18n.language;
  if (!rules.has(lang)) rules.set(lang, new Intl.PluralRules(lang));
  return rules.get(lang).select(n) === 'one' ? 'one' : 'other';
}

/** Numbers and dates the way the house's language writes them. */
export const locale = () => (i18n.language === 'fr' ? 'fr-FR' : i18n.language);
