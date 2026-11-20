// What kind of thing a device is, to see only some of them (the plan, the
// rooms): lights, plugs, climate, shutters and openers, sensors, cameras, media.
import { deviceKind } from './home.svelte.js';

// `label` is a catalogue key, said with t() where it is shown.
export const CATEGORIES = [
  { id: 'lumieres', label: 'commun.filtre.lumieres', icon: 'light', kinds: ['light', 'group'] },
  { id: 'prises', label: 'commun.filtre.prises', icon: 'power', kinds: ['plug'] },
  { id: 'climat', label: 'commun.filtre.climat', icon: 'air-conditioner', kinds: ['climate'] },
  // Shutters and what opens like them (a gate, a garage door on an impulse relay).
  { id: 'volets', label: 'commun.filtre.volets', icon: 'shutter', kinds: ['cover', 'impulse'] },
  { id: 'capteurs', label: 'commun.filtre.capteurs', icon: 'thermometer', kinds: ['sensor', 'plant'] },
  { id: 'cameras', label: 'commun.filtre.cameras', icon: 'cctv', kinds: ['camera'] },
  { id: 'medias', label: 'commun.filtre.medias', icon: 'tv', kinds: ['tv', 'speaker'] },
];

export function categoryOf(d) {
  const kind = deviceKind(d);
  return CATEGORIES.find((c) => c.kinds.includes(kind))?.id ?? 'capteurs';
}

/** Nothing chosen: everything shows. */
export const keeps = (filter, d) => !filter.length || filter.includes(categoryOf(d));

/** A filter saved before « Climat » and « Volets » were two (#171): a bare list,
 *  where `climat` also showed the shutters. Read so, nobody silently loses them. */
function upgrade(ids) {
  return ids.includes('climat') && !ids.includes('volets') ? [...ids, 'volets'] : ids;
}

/** This viewer's filter for a page (kept in the browser, may be absent). */
export function loadFilter(key) {
  try {
    const v = JSON.parse(localStorage.getItem(key) ?? '[]');
    const ids = Array.isArray(v) ? upgrade(v) : Array.isArray(v?.ids) ? v.ids : [];
    return ids.filter((id) => CATEGORIES.some((c) => c.id === id));
  } catch {
    return [];
  }
}

export function saveFilter(key, filter) {
  try {
    // `v: 2`: « climat » alone now means the climate alone.
    localStorage.setItem(key, JSON.stringify({ v: 2, ids: filter }));
  } catch {
    // Private window: the filter lasts the visit.
  }
}
