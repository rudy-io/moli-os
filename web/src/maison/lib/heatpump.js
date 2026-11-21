// A heat pump read through its controller (Mitsubishi Ecodan through the
// ESPHome component: `room_temperature`, `water_feed_temperature`,
// `frequency`, `output_power`…), plus its own electricity meter. Pure.

/** What it does now: `off`, `defrost`, `hot_water`, `heating`, `cooling`,
 *  `idle` (the compressor waits), or `null` when nothing is known. */
export function phaseOf(v) {
  if (v.power_state === false) return 'off';
  if (typeof v.defrost === 'string' && v.defrost !== 'Normal' && v.defrost !== '') return 'defrost';
  const running = typeof v.frequency === 'number' && v.frequency > 0;
  const mode = String(v.operating_mode ?? '');
  if (!running) return v.power_state === true || mode ? 'idle' : null;
  if (/hot ?water|dhw/i.test(mode)) return 'hot_water';
  if (/cool/i.test(mode)) return 'cooling';
  return 'heating';
}

/** The pill of each phase. */
export const PHASE_TONE = { off: '', defrost: 'cool', hot_water: 'warm', heating: 'warm', cooling: 'cool', idle: 'good' };

/** Heat made for each unit of electricity (COP), only while it runs and
 *  both are measured; `null` otherwise (an idle pump draws its standby). */
export function cop(heatW, electricW, running = true) {
  if (!running) return null;
  if (typeof heatW !== 'number' || typeof electricW !== 'number') return null;
  if (!(heatW > 0) || !(electricW > 50)) return null;
  return Math.round((heatW / electricW) * 10) / 10;
}

/** A day's COP from its energy (kWh made, kWh used). */
export function dayCop(produced, consumed) {
  if (typeof produced !== 'number' || typeof consumed !== 'number' || !(consumed > 0)) return null;
  return Math.round((produced / consumed) * 10) / 10;
}

/** The water's drop through the house (flow − return), in °C. */
export function deltaT(feed, ret) {
  if (typeof feed !== 'number' || typeof ret !== 'number') return null;
  return Math.round((feed - ret) * 10) / 10;
}

/** The next set point a step away, kept in its bounds. */
export function stepped(current, kind, direction) {
  if (typeof current !== 'number') return null;
  const step = kind?.step > 0 ? kind.step : 0.5;
  const next = Math.round((current + direction * step) / step) * step;
  const min = Number.isFinite(kind?.min) ? kind.min : -Infinity;
  const max = Number.isFinite(kind?.max) ? kind.max : Infinity;
  return Math.min(max, Math.max(min, Math.round(next * 100) / 100));
}

/** The room set point's presets, as on Home Assistant's dashboard. */
// `key` names the word (`pac.preset.<key>`); a house's own presets
// (`heatpump.presets` in home.json) give a `name` instead.
export const DEFAULT_PRESETS = [
  { key: 'nuit', value: 17, icon: 'moon' },
  { key: 'eco', value: 18, icon: 'leaf' },
  { key: 'confort', value: 20, icon: 'home' },
];

/** The mode follows the room, or the water? (`Heating Room Temp`…) */
export const followsRoom = (mode) => typeof mode === 'string' && /room/i.test(mode);
