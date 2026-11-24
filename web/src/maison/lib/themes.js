// The family dashboard's themes: colours, font, corners and a wallpaper,
// chosen screen by screen (the hall tablet need not look like a phone).
//
// A theme only says what it changes; everything else stays Porcelaine by day
// and Nuit by night (`maison.css`). The house adds its own themes in
// `home.json` (`themes`, and `theme` for the one a new screen starts with),
// written by hand or by an agent. Nothing a theme says reaches the page
// unchecked: colours are plain hex, the wallpaper is a path or an https
// address, and text the family could not read is replaced, and said.

/** The CSS tokens of `.maison` (maison.css), day and night: what a theme starts from. */
export const BASE = {
  day: {
    bg: '#edf0f5',
    surface: '#ffffff',
    'surface-2': '#f5f7fa',
    'surface-3': '#e9edf3',
    ink: '#171c28',
    'ink-2': '#4a5265',
    'ink-3': '#6b7386',
    line: '#e1e5ec',
    warm: '#f0a53a',
    'warm-ink': '#8a4b06',
    'warm-deep': '#3a2a10',
    'warm-soft': '#fff2dc',
    'warm-glow': '#f0a53a61',
    sun: '#e8b931',
    cool: '#4b74f2',
    'cool-soft': '#e8eefe',
    good: '#2c9a88',
    'good-soft': '#e1f3ef',
    alert: '#df5a40',
    'alert-soft': '#fde8e3',
    'on-warm': '#ffffff',
    'on-cool': '#ffffff',
    'on-good': '#ffffff',
    'on-alert': '#ffffff',
  },
  night: {
    bg: '#0d1016',
    surface: '#161a22',
    'surface-2': '#1b2029',
    'surface-3': '#232935',
    ink: '#eceff5',
    'ink-2': '#b3bac8',
    'ink-3': '#788195',
    line: '#252b37',
    warm: '#f5ae4c',
    'warm-ink': '#ffd49a',
    'warm-deep': '#3a2a10',
    'warm-soft': '#33270f',
    'warm-glow': '#f5ae4c4d',
    sun: '#e8b931',
    cool: '#7e9dff',
    'cool-soft': '#1a2238',
    good: '#4fc0ad',
    'good-soft': '#142b28',
    alert: '#ff7b62',
    'alert-soft': '#3a1c16',
    'on-warm': '#ffffff',
    'on-cool': '#ffffff',
    'on-good': '#ffffff',
    'on-alert': '#ffffff',
  },
};

export const TOKENS = Object.keys(BASE.day);
const ACCENTS = ['warm', 'cool', 'good', 'alert'];

/** Font stacks a theme may name: Figtree is in the package, the others are the device's own. */
export const FONTS = {
  figtree: "'Figtree Variable', 'Figtree', ui-sans-serif, system-ui, -apple-system, 'Segoe UI', Roboto, sans-serif",
  system: "system-ui, -apple-system, 'Segoe UI', Roboto, 'Helvetica Neue', sans-serif",
  rounded: "ui-rounded, 'SF Pro Rounded', 'Nunito', 'Quicksand', 'Varela Round', system-ui, sans-serif",
  serif: "'Iowan Old Style', 'Palatino Linotype', Palatino, 'Book Antiqua', Georgia, serif",
  mono: "ui-monospace, 'SF Mono', 'Cascadia Code', 'Roboto Mono', Menlo, Consolas, monospace",
};

/** Corners, from square to very round: a scale on the four radii of maison.css. */
export const CORNERS = { square: 0.3, soft: 0.65, round: 1, pill: 1.3 };
const RADII = { 'r-xl': 32, 'r-lg': 26, 'r-md': 20, 'r-sm': 14 };

/** Text against its background, and how much contrast it needs (WCAG: 4.5 text, 3 large/bold). */
const CHECKS = [
  ['ink', 'surface', 4.5],
  ['ink', 'bg', 4.5],
  ['ink', 'surface-2', 4.5],
  ['ink-2', 'surface', 4.5],
  ['ink-3', 'surface', 4],
  ['warm-ink', 'warm-soft', 4.5],
  ['warm-deep', 'warm', 4.5],
  ['on-warm', 'warm', 3],
  ['on-cool', 'cool', 3],
  ['on-good', 'good', 3],
  ['on-alert', 'alert', 3],
];

// ---- colours ----------------------------------------------------------------------

const HEX = /^#([0-9a-f]{3}|[0-9a-f]{4}|[0-9a-f]{6}|[0-9a-f]{8})$/i;

/** `#rgb`, `#rgba`, `#rrggbb` or `#rrggbbaa` → [r, g, b, a] (a from 0 to 1); anything else: null. */
export function parseHex(text) {
  if (typeof text !== 'string' || !HEX.test(text.trim())) return null;
  let h = text.trim().slice(1);
  if (h.length <= 4) h = [...h].map((c) => c + c).join('');
  const n = (i) => parseInt(h.slice(i, i + 2), 16);
  return [n(0), n(2), n(4), h.length === 8 ? n(6) / 255 : 1];
}

const two = (v) => Math.round(Math.min(255, Math.max(0, v))).toString(16).padStart(2, '0');

/** [r, g, b, a] → `#rrggbb` (or `#rrggbbaa` when not opaque). */
export function toHex([r, g, b, a = 1]) {
  return `#${two(r)}${two(g)}${two(b)}${a < 1 ? two(a * 255) : ''}`;
}

/** From `a` towards `b`, by `t` (0: a, 1: b). */
export function mix(a, b, t) {
  const x = parseHex(a);
  const y = parseHex(b);
  return toHex([0, 1, 2].map((i) => x[i] + (y[i] - x[i]) * t));
}

function luminance(hex) {
  const [r, g, b] = parseHex(hex).map((v) => {
    const s = v / 255;
    return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

/** WCAG contrast ratio of two opaque colours, from 1 to 21. */
export function contrast(a, b) {
  const [x, y] = [luminance(a), luminance(b)].sort((m, n) => n - m);
  return (x + 0.05) / (y + 0.05);
}

// ---- reading a theme ----------------------------------------------------------------

const ID = /^[a-z0-9][a-z0-9-]{0,39}$/;
// A wallpaper is an image Moli keeps (`/api/plan/images/<name>`, the names `plan.rs`
// `image_name_ok` writes), never an address elsewhere: each screen of the house would
// otherwise fetch it from a third party (the house's IP, every guest's screen too).
// It goes into a CSS url(), so nothing but those letters.
const IMAGE = /^\/api\/plan\/images\/[0-9a-f]{16}\.(png|jpg|webp|svg)$/;

function readColors(raw, where, problems) {
  const out = {};
  if (raw == null) return out;
  if (typeof raw !== 'object' || Array.isArray(raw)) {
    problems.push({ code: 'mode', key: where });
    return out;
  }
  for (const [key, value] of Object.entries(raw)) {
    if (key === 'background') continue;
    if (!TOKENS.includes(key)) problems.push({ code: 'unknown', key: `${where}.${key}` });
    else if (!parseHex(value)) problems.push({ code: 'color', key: `${where}.${key}`, value: String(value) });
    else out[key] = toHex(parseHex(value));
  }
  return out;
}

function readBackground(raw, where, problems) {
  if (raw == null) return null;
  if (typeof raw !== 'object' || Array.isArray(raw)) {
    problems.push({ code: 'background', key: where });
    return null;
  }
  const out = {};
  if (raw.image != null) {
    if (typeof raw.image === 'string' && IMAGE.test(raw.image)) out.image = raw.image;
    else problems.push({ code: 'image', key: `${where}.image` });
  }
  if (raw.gradient != null) {
    const colors = Array.isArray(raw.gradient) ? raw.gradient.map(parseHex) : [];
    if (colors.length >= 2 && colors.length <= 6 && colors.every(Boolean)) out.gradient = colors.map((c) => toHex(c));
    else problems.push({ code: 'gradient', key: `${where}.gradient` });
  }
  if (raw.dim != null) {
    if (typeof raw.dim === 'number' && raw.dim >= 0 && raw.dim <= 0.9) out.dim = raw.dim;
    else problems.push({ code: 'dim', key: `${where}.dim` });
  }
  return out.image || out.gradient ? out : null;
}

/**
 * A theme as `home.json` writes it, made safe:
 * `{ id, name, day: {token: hex, background}, night: {…}, font, corners, background }`.
 * Returns `{ theme, problems }`; `theme` is null when it cannot be used at all (no valid id).
 */
export function readTheme(raw) {
  const problems = [];
  if (!raw || typeof raw !== 'object' || typeof raw.id !== 'string' || !ID.test(raw.id)) {
    return { theme: null, problems: [{ code: 'id', key: String(raw?.id ?? '') }] };
  }
  const name =
    typeof raw.name === 'string' || (raw.name && typeof raw.name === 'object' && !Array.isArray(raw.name))
      ? raw.name
      : raw.id;
  const theme = {
    id: raw.id,
    name,
    day: readColors(raw.day, 'day', problems),
    night: readColors(raw.night, 'night', problems),
    background: readBackground(raw.background, 'background', problems),
    dayBackground: readBackground(raw.day?.background, 'day.background', problems),
    nightBackground: readBackground(raw.night?.background, 'night.background', problems),
    font: null,
    corners: null,
  };
  if (raw.font != null) {
    if (Object.hasOwn(FONTS, raw.font)) theme.font = raw.font;
    else problems.push({ code: 'font', key: String(raw.font) });
  }
  if (raw.corners != null) {
    if (Object.hasOwn(CORNERS, raw.corners)) theme.corners = raw.corners;
    else problems.push({ code: 'corners', key: String(raw.corners) });
  }
  for (const key of Object.keys(raw)) {
    if (!['id', 'name', 'day', 'night', 'background', 'font', 'corners', 'builtin'].includes(key)) problems.push({ code: 'unknown', key });
  }
  return { theme, problems };
}

/** The theme's name in the reader's language: a string, or `{ fr, en }`. */
export function themeName(theme, locale) {
  if (typeof theme.name === 'string') return theme.name;
  return theme.name?.[locale] ?? theme.name?.fr ?? theme.name?.en ?? Object.values(theme.name ?? {}).find((v) => typeof v === 'string') ?? theme.id;
}

// ---- the themes Moli brings -----------------------------------------------------------

/** Built-in themes; their names are catalogue keys (`builtin`). Porcelaine is the base itself. */
export const BUILTIN = [
  { id: 'porcelaine', builtin: 'commun.theme_sheet.themes.porcelaine' },
  {
    id: 'sauge',
    builtin: 'commun.theme_sheet.themes.sauge',
    corners: 'soft',
    day: {
      bg: '#eceee6', surface: '#fbfbf6', ink: '#1f2a22', 'ink-2': '#475649', 'ink-3': '#5f6e62',
      warm: '#dc9a3c', cool: '#4a7aa3', good: '#4f8a55', alert: '#c95a3a',
    },
    night: {
      bg: '#0e1310', surface: '#161e18', ink: '#e7ede5', 'ink-2': '#b0bdb1', 'ink-3': '#84928a',
      warm: '#e8a952', cool: '#86aedb', good: '#7cbb80', alert: '#f08262',
    },
  },
  {
    id: 'ocean',
    builtin: 'commun.theme_sheet.themes.ocean',
    day: {
      bg: '#e6eff4', surface: '#ffffff', ink: '#0e2230', 'ink-2': '#3d5566', 'ink-3': '#587083',
      warm: '#f0a23e', cool: '#1f78bd', good: '#1a9690', alert: '#d9574a',
      background: { gradient: ['#eaf4f9', '#d3e5ef'] },
    },
    night: {
      bg: '#07121b', surface: '#0e1d29', ink: '#e5f0f6', 'ink-2': '#a8c0ce', 'ink-3': '#7d97a8',
      warm: '#f5b459', cool: '#5fb2ec', good: '#4cc6bc', alert: '#ff7c6a',
      background: { gradient: ['#06101a', '#0b2233'] },
    },
  },
  {
    id: 'terracotta',
    builtin: 'commun.theme_sheet.themes.terracotta',
    font: 'serif',
    corners: 'soft',
    day: {
      bg: '#f3ebe3', surface: '#fffaf4', ink: '#2b1d16', 'ink-2': '#5c483d', 'ink-3': '#735f53',
      warm: '#e08a3c', cool: '#4b74ab', good: '#3b8a75', alert: '#c1432e',
    },
    night: {
      bg: '#14100d', surface: '#1f1814', ink: '#f2e8df', 'ink-2': '#c6b4a7', 'ink-3': '#9a877b',
      warm: '#f0a052', cool: '#86a6dc', good: '#6cbfa8', alert: '#f07a60',
    },
  },
];

/** Every theme a screen may choose: Moli's, then the house's (`home.json`; same id: the house's wins). */
export function themeList(config) {
  const list = BUILTIN.map((raw) => ({ ...readTheme(raw).theme, builtin: raw.builtin }));
  const problems = {};
  for (const raw of Array.isArray(config?.themes) ? config.themes : []) {
    const { theme, problems: found } = readTheme(raw);
    if (!theme) continue;
    if (found.length) problems[theme.id] = found;
    const at = list.findIndex((t) => t.id === theme.id);
    if (at >= 0) list[at] = theme;
    else list.push(theme);
  }
  return { list, problems };
}

/** This screen's theme: its own choice, else the house's (`home.json` `theme`), else Porcelaine. */
export function pickTheme(list, chosen, houseDefault) {
  return list.find((t) => t.id === chosen) ?? list.find((t) => t.id === houseDefault) ?? list[0];
}

// ---- applying it ---------------------------------------------------------------------

/** Text on a solid accent: white when it reads well, else the day ink. */
const onAccent = (accent) => (contrast('#ffffff', accent) >= 3 ? '#ffffff' : BASE.day.ink);

/**
 * The tokens of `theme` in `mode` ('day' | 'night'), with what it did not say
 * derived from what it did (a lighter surface, softer accents, text on accents),
 * then every pair of text and background checked. Returns
 * `{ tokens, problems }`: all the tokens, and the colours replaced to stay readable.
 */
export function resolveTokens(theme, mode) {
  const base = BASE[mode];
  const given = theme?.[mode] ?? {};
  const t = { ...base, ...given };
  const touched = new Set(Object.keys(given));
  const derive = (key, value) => {
    if (key in given) return;
    t[key] = value;
    touched.add(key);
  };

  if ('surface' in given || 'ink' in given) {
    derive('surface-2', mix(t.surface, t.ink, 0.035));
    derive('surface-3', mix(t.surface, t.ink, 0.08));
    derive('line', mix(t.surface, t.ink, 0.11));
  }
  const soft = mode === 'day' ? 0.13 : 0.16;
  for (const accent of ACCENTS) {
    if (!(accent in given) && !('surface' in given)) continue;
    derive(`${accent}-soft`, mix(t.surface, t[accent], soft));
    if (accent in given) derive(`on-${accent}`, onAccent(t[accent]));
  }
  if ('warm' in given) {
    const [r, g, b] = parseHex(t.warm);
    derive('warm-glow', toHex([r, g, b, mode === 'day' ? 0.38 : 0.3]));
    derive('warm-deep', mix(t.warm, '#000000', 0.76));
  }
  if ('warm' in given || 'ink' in given) derive('warm-ink', mix(t.warm, t.ink, 0.55));

  const problems = [];
  for (const [fg, bg, need] of CHECKS) {
    if (!touched.has(fg) && !touched.has(bg)) continue;
    if (contrast(t[fg], t[bg]) >= need) continue;
    const other = BASE[mode === 'day' ? 'night' : 'day'];
    const candidates = [base[fg], other[fg], '#ffffff', BASE.day.ink, '#000000'];
    const fix =
      candidates.find((c) => contrast(c, t[bg]) >= need) ?? candidates.reduce((a, b) => (contrast(b, t[bg]) > contrast(a, t[bg]) ? b : a));
    problems.push({ code: 'contrast', key: fg, against: bg, value: t[fg], now: fix });
    t[fg] = fix;
    touched.add(fg);
  }
  return { tokens: t, touched, problems };
}

const cssUrl = (path) => `url("${path}")`;

/** The wallpaper of `theme` in `mode`, as a CSS background (null: the plain `--bg`). */
export function wallpaper(theme, mode, bg) {
  const paper = (mode === 'day' ? theme?.dayBackground : theme?.nightBackground) ?? theme?.background;
  if (!paper) return null;
  const layers = [];
  if (paper.dim) {
    const [r, g, b] = parseHex(bg);
    const veil = toHex([r, g, b, paper.dim]);
    layers.push(`linear-gradient(${veil}, ${veil})`);
  }
  if (paper.image) layers.push(`${cssUrl(paper.image)} center / cover no-repeat`);
  if (paper.gradient) layers.push(`linear-gradient(160deg, ${paper.gradient.join(', ')})`);
  return `${layers.join(', ')}, ${bg}`;
}

/**
 * Everything a screen needs to wear `theme` in `mode`: the inline style of
 * `.maison` (only what differs from maison.css), the wallpaper, the page's
 * background colour, and the colours replaced to stay readable.
 */
export function applyTheme(theme, mode) {
  const { tokens, touched, problems } = resolveTokens(theme, mode);
  const decls = [...touched].sort().map((key) => `--${key}: ${tokens[key]}`);
  if (theme?.font && theme.font !== 'figtree') decls.push(`--font: ${FONTS[theme.font]}`);
  if (theme?.corners && theme.corners !== 'round') {
    for (const [key, px] of Object.entries(RADII)) decls.push(`--${key}: ${Math.round(px * CORNERS[theme.corners])}px`);
  }
  return {
    style: decls.join('; '),
    wallpaper: wallpaper(theme, mode, tokens.bg),
    bg: tokens.bg,
    tokens,
    font: FONTS[theme?.font ?? 'figtree'],
    corners: CORNERS[theme?.corners ?? 'round'],
    problems,
  };
}
