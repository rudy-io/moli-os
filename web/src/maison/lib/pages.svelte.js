// The family dashboard's pages, in the menu's order, and whether this house
// has anything for each: a page with nothing to show stays out of the menu
// (its address still answers) and comes back by itself with the first
// device that fills it. The menu and « Comment ça marche » share it.
import { hub, home, value, deviceKind, roomOf } from './home.svelte.js';
import { tempKey } from './temperatures.js';
import { energy } from './energy-live.svelte.js';
import { isPrinter } from './printers.js';
import { isPaperPrinter } from './paper.js';

// `label` is a catalogue key, said where it is shown; Moli is a name.
export const NAV = [
  { path: '', label: 'commun.nav.accueil', icon: 'home' },
  { path: 'moli', name: 'Moli', orb: true },
  { path: 'salon', label: 'commun.nav.salon', icon: 'remote' },
  { path: 'pieces', label: 'commun.nav.pieces', icon: 'light-group' },
  { path: 'chauffage', label: 'commun.nav.chauffage', icon: 'stove' },
  { path: 'fenetres', label: 'commun.nav.fenetres', icon: 'roof-window' },
  { path: 'plan', label: 'commun.nav.plan', icon: 'home-roof' },
  { path: 'carte', label: 'commun.nav.carte', icon: 'map' },
  { path: 'cameras', label: 'commun.nav.cameras', icon: 'cctv' },
  { path: 'parler', label: 'commun.nav.parler', icon: 'talk' },
  { path: 'energie', label: 'commun.nav.energie', icon: 'bolt' },
  { path: 'temperatures', label: 'commun.nav.temperatures', icon: 'thermometer' },
  { path: 'impression', label: 'commun.nav.impression', icon: 'printer3d' },
  { path: 'dehors', label: 'commun.nav.dehors', icon: 'flower-outline' },
  { path: 'automatismes', label: 'commun.nav.automatismes', icon: 'robot' },
  { path: 'infra', label: 'commun.nav.infra', icon: 'server' },
];

const has = (d, key, write = false) => d.points.some((p) => p.key === key && (!write || p.access?.write));

/** For each page that depends on the house, whether it has something to
 *  show; a page not listed always shows. (Reactive: read inside $derived.) */
export function filledPages() {
  const devices = Object.values(hub.devices);
  const cfg = home.config ?? {};
  return {
    salon: !!(cfg.salon?.tv || cfg.salon?.speaker),
    chauffage: devices.some((d) => deviceKind(d) === 'climate'),
    fenetres: devices.some((d) => deviceKind(d) === 'cover'),
    carte: devices.some((d) => d.id.startsWith('personnes:') && value(d.id, 'latitude') != null),
    cameras: devices.some((d) => d.camera) || !!cfg.favorites?.cameras?.length,
    parler: devices.some((d) => has(d, 'say', true) || has(d, 'announce', true)),
    energie: !energy.missing,
    temperatures: devices.some((d) => tempKey(d) && roomOf(d)),
    impression: devices.some((d) => isPrinter(d) || isPaperPrinter(d)),
    dehors: !!(cfg.outdoor?.weather || cfg.outdoor?.air || cfg.outdoor?.pool),
    infra: devices.some((d) => d.model === 'Serveur de la maison' || d.model === 'Ordinateur' || has(d, 'external_ip')),
  };
}
