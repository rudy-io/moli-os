// What kind of thing a device is, to see only some of them (the plan, the
// rooms): lights, plugs, climate and shutters, sensors, cameras, media.
import { deviceKind } from './home.svelte.js';

// `label` is a catalogue key, said with t() where it is shown.
export const CATEGORIES = [
  { id: 'lumieres', label: 'commun.filtre.lumieres', icon: 'light', kinds: ['light', 'group'] },
  { id: 'prises', label: 'commun.filtre.prises', icon: 'power', kinds: ['plug'] },
  { id: 'climat', label: 'commun.filtre.climat', icon: 'air-conditioner', kinds: ['climate', 'cover'] },
  { id: 'capteurs', label: 'commun.filtre.capteurs', icon: 'thermometer', kinds: ['sensor'] },
  { id: 'cameras', label: 'commun.filtre.cameras', icon: 'cctv', kinds: ['camera'] },
  { id: 'medias', label: 'commun.filtre.medias', icon: 'tv', kinds: ['tv', 'speaker'] },
];

export function categoryOf(d) {
  const kind = deviceKind(d);
  return CATEGORIES.find((c) => c.kinds.includes(kind))?.id ?? 'capteurs';
}

/** Nothing chosen: everything shows. */
export const keeps = (filter, d) => !filter.length || filter.includes(categoryOf(d));

/** This viewer's filter for a page (kept in the browser, may be absent). */
export function loadFilter(key) {
  try {
    const v = JSON.parse(localStorage.getItem(key) ?? '[]');
    return Array.isArray(v) ? v.filter((id) => CATEGORIES.some((c) => c.id === id)) : [];
  } catch {
    return [];
  }
}

export function saveFilter(key, filter) {
  try {
    localStorage.setItem(key, JSON.stringify(filter));
  } catch {
    // Private window: the filter lasts the visit.
  }
}
