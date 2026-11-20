import { test } from 'node:test';
import assert from 'node:assert/strict';
import { forecastDevice, parseCurve, forecastHours, producedHours, dayOf, lightWindow, soFar } from './solar-forecast.js';

const H = 3_600_000;
const day0 = new Date(2026, 10, 20).getTime(); // local midnight

/** A curve of today: 1 kW from 09:00 to 12:00, nothing else. */
function curveText() {
  const kw = Array.from({ length: 48 }, (_, i) => (i >= 18 && i < 24 ? 1 : i < 2 ? null : 0));
  return JSON.stringify({ start: day0, step: 1800, kw, lo: kw.map((v) => (v == null ? null : v / 2)), hi: kw.map((v) => (v == null ? null : v * 1.5)) });
}

test('the forecast device is the one with a curve', () => {
  const values = { 'solcast:prevision/curve': '{}' };
  const value = (id, k) => values[`${id}/${k}`];
  const devices = { a: { id: 'ha:solcast-aujourdhui', manufacturer: 'Home Assistant' }, b: { id: 'solcast:prevision', manufacturer: 'Solcast' } };
  assert.equal(forecastDevice(devices, value).id, 'solcast:prevision');
  assert.equal(forecastDevice({ a: devices.a }, value), null);
});

test('the curve is read half-hour by half-hour, the unknown left out', () => {
  const slots = parseCurve(curveText());
  assert.equal(slots.length, 46);
  assert.equal(slots[0].start, day0 + 2 * 1800_000);
  assert.deepEqual(parseCurve('nope'), null);
  assert.deepEqual(parseCurve('{"start":1}'), null);
});

test('hours add their half-hours; the panels add their meters', () => {
  const hours = forecastHours(parseCurve(curveText()), day0, day0 + 24 * H);
  const nine = hours.find((h) => h.start === day0 + 9 * H);
  assert.equal(nine.kwh, 1);
  assert.equal(nine.lo, 0.5);
  assert.equal(nine.hi, 1.5);
  const series = {
    meters: [{ id: 'jardin', role: 'solar' }, { id: 'atelier', role: 'solar' }, { id: 'reseau', role: 'total' }],
    buckets: [
      { start: day0 + 9 * H, meters: { jardin: { kwh: 0.8 }, atelier: { kwh: 0.1 }, reseau: { kwh: 2 } } },
      { start: day0 + 10 * H, meters: { reseau: { kwh: 1 } } },
    ],
  };
  const made = producedHours(series);
  assert.ok(Math.abs(made.get(day0 + 9 * H) - 0.9) < 1e-9);
  assert.equal(made.has(day0 + 10 * H), false);
  assert.equal(producedHours({ meters: [], buckets: series.buckets }).size, 0);
});

test('the window spans the light, with a margin', () => {
  const hours = forecastHours(parseCurve(curveText()), day0, day0 + 24 * H);
  const w = lightWindow(hours, new Map(), dayOf(day0 + 10 * H));
  assert.deepEqual(w, [day0 + 8 * H, day0 + 13 * H]);
  assert.equal(lightWindow([], new Map(), dayOf(day0)), null);
});

test('so far: above, within, below, or too early to say', () => {
  const hours = forecastHours(parseCurve(curveText()), day0, day0 + 24 * H);
  const at = day0 + 11 * H; // 2 kWh expected so far (09:00 and 10:00)
  const made = (a, b) => new Map([[day0 + 9 * H, a], [day0 + 10 * H, b]]);
  // Above the high estimate (3 kWh): above.
  assert.equal(soFar(hours, made(1.7, 1.7), at).verdict, 'above');
  assert.equal(soFar(hours, made(1.7, 1.7), at).pct, 70);
  // 30 % more, but inside the high estimate: within.
  assert.equal(soFar(hours, made(1.3, 1.3), at).verdict, 'within');
  assert.equal(soFar(hours, made(1.05, 1), at).verdict, 'within');
  // Below by 25 %, but inside the low estimate (1 kWh): within.
  assert.equal(soFar(hours, made(0.75, 0.75), at).verdict, 'within');
  assert.equal(soFar(hours, made(0.4, 0.4), at).verdict, 'below');
  // Half past nine: half of 1 kWh expected, enough to say.
  assert.equal(soFar(hours, made(0.5, 0), day0 + 9.5 * H).expected, 0.5);
  // Before the light: nothing to say.
  assert.equal(soFar(hours, new Map(), day0 + 9.1 * H), null);
});
