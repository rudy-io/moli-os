// What a valve did over a period, read from `/api/history`, and what its schedule
// asked over the same period. Pure arithmetic, no Svelte, no DOM, no hub: the
// chart's points, steps and scales are testable on their own.
import { DAYS, parseDay } from './schedule.js';

const finite = (v) => typeof v === 'number' && Number.isFinite(v);

/** A numeric series as `[ts, value]` points: the bucket averages when the hub
 *  bucketed the window, else the changes themselves. A value held from before
 *  the window opens it; the last one holds until the window closes. */
export function seriesPoints(series) {
  if (!series) return [];
  if (series.buckets?.length) return series.buckets.filter((b) => finite(b.avg)).map((b) => [b.ts, b.avg]);
  const steps = seriesSteps(series);
  const last = steps.at(-1);
  // Without this, a room whose temperature never moved has one point: no line at all.
  return last && last[0] < series.to ? [...steps, [series.to, last[1]]] : steps;
}

/** A numeric value that holds until the next change, as `[ts, value]` steps. */
export function seriesSteps(series) {
  if (!series) return [];
  const steps = (series.raw ?? []).filter(([, v]) => finite(v)).map(([ts, v]) => [ts, v]);
  // A truncated series lost its oldest changes: `before` would be held across the hole.
  if (series.before && finite(series.before[1]) && !series.truncated) steps.unshift([series.from, series.before[1]]);
  return steps;
}

/** What a schedule asks over `[from, to]`, as steps. `days` maps Z2M's day names
 *  to their text. Dates are local, like the valve's own clock; a day with no
 *  text holds the previous value. The table is today's: the hub keeps no history
 *  of it, so a past week is read with the table in force now. */
export function programmeSteps(spec, days, from, to) {
  if (!spec || !(to > from)) return [];
  const start = new Date(from);
  const steps = [];
  // One day back: the step in force at `from` may belong to the day before.
  for (let d = new Date(start.getFullYear(), start.getMonth(), start.getDate() - 1); d.getTime() <= to; d = new Date(d.getFullYear(), d.getMonth(), d.getDate() + 1)) {
    const text = days?.[DAYS[(d.getDay() + 6) % 7]];
    if (typeof text !== 'string' || !text) continue;
    for (const s of parseDay(spec, text)) steps.push([new Date(d.getFullYear(), d.getMonth(), d.getDate(), 0, s.min).getTime(), s.deg]);
  }
  const held = steps.filter(([ts]) => ts <= from).at(-1);
  const later = steps.filter(([ts]) => ts > from && ts <= to);
  return held ? [[from, held[1]], ...later] : later;
}

/** The vertical range holding every value, with a margin; null when there is none. */
export function extent(lists, margin = 0.08) {
  const values = lists.flat().map((p) => p[1]).filter(finite);
  if (!values.length) return null;
  const lo = Math.min(...values);
  const hi = Math.max(...values);
  const pad = (hi - lo || 1) * margin;
  return { min: lo - pad, max: hi + pad };
}

/** `x` and `y` mapping a time and a value into a box (`left`, `top`, `width`, `height`). */
export function scaleOf(from, to, range, box) {
  if (!range || !(to > from) || !(range.max > range.min)) return null;
  return {
    x: (ts) => box.left + ((ts - from) / (to - from)) * box.width,
    y: (v) => box.top + (1 - (v - range.min) / (range.max - range.min)) * box.height,
  };
}

const fix = (n) => Math.round(n * 10) / 10;

/** A line through the points. */
export function linePath(points, scale) {
  return points.map(([ts, v], i) => `${i ? 'L' : 'M'}${fix(scale.x(ts))},${fix(scale.y(v))}`).join('');
}

/** A step line: each value holds until the next change, the last until `to`. */
export function stepPath(steps, to, scale) {
  if (!steps.length) return '';
  const d = steps.map(([ts, v], i) => (i ? `H${fix(scale.x(ts))}V${fix(scale.y(v))}` : `M${fix(scale.x(ts))},${fix(scale.y(v))}`)).join('');
  return `${d}H${fix(scale.x(to))}`;
}

/** Round values for the vertical axis: at most `count`, on a 1, 2 or 5 step. */
export function valueTicks(range, count = 4) {
  if (!range) return [];
  const raw = (range.max - range.min) / count;
  const base = 10 ** Math.floor(Math.log10(raw));
  const step = [1, 2, 5, 10].map((m) => m * base).find((s) => s >= raw);
  const ticks = [];
  for (let v = Math.ceil(range.min / step) * step; v <= range.max + 1e-9; v += step) ticks.push(Math.round(v * 1e6) / 1e6);
  return ticks;
}

/** Times for the horizontal axis: every `hoursStep` hours on the local clock
 *  (a day boundary for the week), inside `[from, to]`. */
export function timeTicks(from, to, hours) {
  const stepHours = hours <= 6 ? 1 : hours <= 24 ? 6 : 24;
  const ticks = [];
  const d = new Date(from);
  d.setMinutes(0, 0, 0);
  d.setHours(stepHours === 24 ? 0 : Math.ceil(d.getHours() / stepHours) * stepHours);
  // On the clock, not by adding milliseconds: a clock change would shift every later tick.
  for (; d.getTime() <= to; d.setHours(d.getHours() + stepHours)) if (d.getTime() >= from) ticks.push(d.getTime());
  return ticks;
}
