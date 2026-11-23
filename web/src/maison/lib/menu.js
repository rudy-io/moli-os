// The menu's shape and place, screen by screen: where it sits (left, right,
// top, bottom), how it looks (classic: icons and names; compact: icons
// alone; floating: a card off the edge) and the order of its tabs.
// A screen's own choice (`maison-menu` in the browser) wins over the house's
// (`menu` in home.json); anything unknown falls back to the classic rail on the left.

export const POSITIONS = ['left', 'right', 'top', 'bottom'];
export const SHAPES = ['classic', 'compact', 'floating'];
export const DEFAULT = { position: 'left', shape: 'classic', order: [] };

/** One menu setting made safe: unknown words dropped, the order a list of distinct paths. */
export function readMenu(raw) {
  const out = {};
  if (!raw || typeof raw !== 'object' || Array.isArray(raw)) return out;
  if (POSITIONS.includes(raw.position)) out.position = raw.position;
  if (SHAPES.includes(raw.shape)) out.shape = raw.shape;
  if (Array.isArray(raw.order)) out.order = [...new Set(raw.order.filter((p) => typeof p === 'string' && p.length <= 40))];
  return out;
}

/** What the browser kept (`maison-menu`), read without trusting it. */
export function readSaved(text) {
  try {
    return readMenu(JSON.parse(text ?? 'null'));
  } catch {
    return {};
  }
}

/** This screen's menu: its own choices, else the house's, else the default. */
export function menuSettings(saved, house) {
  return { ...DEFAULT, ...readMenu(house), ...saved };
}

/** The tabs in the chosen order: Accueil first, then those named in `order`,
 *  then the others (a tab that appears later) where the house lists them. */
export function orderTabs(tabs, order = []) {
  const rank = (tab) => {
    if (tab.path === '') return -1;
    const at = order.indexOf(tab.path);
    return at >= 0 ? at : order.length + tabs.indexOf(tab);
  };
  return [...tabs].sort((a, b) => rank(a) - rank(b));
}

/** The order after moving `path` one place up (`-1`) or down (`+1`) among `tabs` (as shown). */
export function moveTab(tabs, path, delta) {
  const paths = tabs.map((t) => t.path);
  const from = paths.indexOf(path);
  const to = from + delta;
  // Accueil stays first: nothing moves above it, it does not move.
  if (from <= 0 || to <= 0 || to >= paths.length) return paths.filter((p) => p !== '');
  [paths[from], paths[to]] = [paths[to], paths[from]];
  return paths.filter((p) => p !== '');
}

