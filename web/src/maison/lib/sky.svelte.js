// The sky over the house: the forecast (next hours, next days) and the
// weather map (wind, clouds, rain around the house, the land under it).
// Fetched when a page shows them, refreshed while it does; nothing when the
// house has no position.

import { t, locale } from '../../lib/i18n.svelte.js';

export const sky = $state({ forecast: null, map: null });

const FORECAST_EVERY = 15 * 60_000;
const MAP_EVERY = 30 * 60_000;

async function json(url) {
  try {
    const res = await fetch(url);
    return res.ok ? await res.json() : null;
  } catch {
    return null;
  }
}

async function loadForecast() {
  const f = await json('/api/weather/forecast');
  if (f) sky.forecast = f;
}

let landTries = 0;
async function loadMap() {
  const m = await json('/api/weather/map');
  if (m?.weather?.hours?.length) sky.map = m;
  // The land comes a little later the very first time: asked again soon.
  if (m && !m.land && landTries++ < 6) setTimeout(loadMap, 20_000);
}

let watchers = 0;
let timers = [];
/** Keeps the sky fresh while a page shows it; returns what stops it. */
export function watchSky() {
  if (watchers++ === 0) {
    loadForecast();
    loadMap();
    timers = [setInterval(loadForecast, FORECAST_EVERY), setInterval(loadMap, MAP_EVERY)];
  }
  return () => {
    if (--watchers === 0) {
      for (const id of timers) clearInterval(id);
      timers = [];
    }
  };
}

/** Open-Meteo's local time (« 2026-10-10T15:00 ») as a timestamp. */
export function localTs(iso, offset) {
  return Date.parse(`${iso}:00Z`) - offset * 1000;
}

/** The next hours as rows: the current hour first. */
export function hours(f) {
  const h = f?.hourly;
  if (!h?.time) return [];
  const offset = Number(f.utc_offset_seconds ?? 0);
  return h.time.map((at, i) => ({
    at: localTs(at, offset),
    hour: Number(at.slice(11, 13)),
    temp: h.temperature_2m?.[i],
    code: h.weather_code?.[i],
    rain: h.precipitation_probability?.[i] ?? 0,
    mm: h.precipitation?.[i] ?? 0,
    cloud: h.cloud_cover?.[i],
    wind: h.wind_speed_10m?.[i],
    day: h.is_day?.[i] !== 0,
  }));
}

/** The days to come as rows: today first. */
export function days(f) {
  const d = f?.daily;
  if (!d?.time) return [];
  return d.time.map((date, i) => ({
    date,
    code: d.weather_code?.[i],
    max: d.temperature_2m_max?.[i],
    min: d.temperature_2m_min?.[i],
    rain: d.precipitation_probability_max?.[i] ?? 0,
    mm: d.precipitation_sum?.[i] ?? 0,
    wind: d.wind_speed_10m_max?.[i],
  }));
}

const wet = (code) => {
  const c = Number(code);
  return c >= 51 && c !== 77;
};
const raining = (h) => wet(h.code) && h.mm > 0;
const likely = (h) => h.rain >= 50 || (wet(h.code) && h.mm >= 0.2);

/** One sentence about the hours to come, the way a person would say it
 *  (rain on its way, rain about to stop, a cold night…); null when there
 *  is nothing worth a word. */
export function outlook(rows) {
  if (rows.length < 2) return null;
  const now = rows[0];
  const next = rows.slice(1, 13);
  if (raining(now) || likely(now)) {
    const dry = next.find((h) => !likely(h) && !raining(h));
    return dry ? t('maison.meteo.fin_pluie', { h: dry.hour }) : t('maison.meteo.pluie_continue');
  }
  const rain = next.find(likely);
  if (rain) {
    const storm = Number(rain.code) >= 95;
    return t(storm ? 'maison.meteo.orage_vers' : 'maison.meteo.pluie_vers', { h: rain.hour });
  }
  const gust = next.find((h) => (h.wind ?? 0) >= 50);
  if (gust) return t('maison.meteo.vent_vers', { h: gust.hour, kmh: Math.round(gust.wind) });
  const temps = next.map((h) => h.temp).filter(Number.isFinite);
  if (temps.length && Math.min(...temps) <= 2) return t('maison.meteo.gel', { min: Math.round(Math.min(...temps)) });
  return t('maison.meteo.sec', { h: next.at(-1).hour });
}

/** « Auj. », then the weekday. */
export function dayName(date, i) {
  if (i === 0) return t('maison.meteo.aujourdhui');
  return new Date(`${date}T12:00:00`).toLocaleDateString(locale(), { weekday: 'short' }).replace('.', '');
}
