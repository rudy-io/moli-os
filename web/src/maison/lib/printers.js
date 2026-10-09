// 3D printers, whatever their brand: Moonraker (Klipper) and Bambu say the
// same things in different words. Pure helpers for the cards and the home.

import { t } from '../../lib/i18n.svelte.js';

/** One vocabulary for both: printing, paused, done, cancelled, error, idle. */
const STATES = {
  printing: 'printing',
  running: 'printing',
  prepare: 'printing',
  slicing: 'printing',
  paused: 'paused',
  pause: 'paused',
  complete: 'done',
  finish: 'done',
  cancelled: 'cancelled',
  failed: 'error',
  error: 'error',
  standby: 'idle',
  idle: 'idle',
};

export const phase = (raw) => STATES[String(raw ?? '').toLowerCase()] ?? 'idle';

// `label` is read at each use (a getter): the house's language arrives after
// this module is loaded.
const phaseOf = (id, tone) => ({
  get label() {
    return t(`salon.impression.phase.${id}`);
  },
  tone,
});

export const PHASES = {
  printing: phaseOf('printing', 'warm'),
  paused: phaseOf('paused', 'cool'),
  done: phaseOf('done', 'good'),
  cancelled: phaseOf('cancelled', ''),
  error: phaseOf('error', 'alert'),
  idle: phaseOf('idle', ''),
};

/** A printer: a print's progress and a nozzle. */
export const isPrinter = (d) =>
  !!d && d.points.some((p) => p.key === 'progress') && d.points.some((p) => p.key === 'nozzle_temperature');

/** « Calibration_cube_PLA_8h16m » → « Calibration cube ». The slicer
 *  appends the filament and its time estimate: noise on a card. */
export function jobName(raw) {
  if (!raw) return '';
  const name = String(raw)
    .replace(/\.(gcode|3mf)$/i, '')
    .replace(/_(PLA|PETG|ABS|ASA|TPU|PA|PC|PVA|HIPS)[^_]*_\d+h\d*m?$/i, '')
    .replace(/_\d+h\d*m$/i, '');
  return name.replace(/_/g, ' ').trim();
}

export function duration(minutes) {
  if (minutes == null || !Number.isFinite(minutes)) return '—';
  const m = Math.round(minutes);
  if (m < 60) return `${m} min`;
  const h = Math.floor(m / 60);
  const r = m % 60;
  return r ? `${h} h ${String(r).padStart(2, '0')}` : `${h} h`;
}

/** The order that stops a print, in this printer's words. */
export const cancelWord = (d) =>
  d?.points.find((p) => p.key === 'control')?.kind.values?.includes('cancel') ? 'cancel' : 'stop';
