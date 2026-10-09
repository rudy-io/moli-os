// Solaire (démo) : ce que des panneaux solaires feraient sur la vraie
// consommation de la maison. Rien n'est installé : la production est
// calculée (soleil de ciel clair, nébulosité tirée au sort à graine fixe, ou
// relevée par la météo de Moli pour aujourd'hui), la consommation est celle
// des compteurs, heure par heure (/api/energy/series?step=hour).
//
// Fonctions pures, sans DOM ni réseau : la page les nourrit, et
// `web/scripts/solar-sim.check.mjs` en vérifie les invariants.

const HOUR = 3_600_000;
const DAY = 86_400_000;
const RAD = Math.PI / 180;
const EPS = 1e-9;

/** Every figure of the simulation is a hypothesis the page shows and lets
 *  change: the installation, the battery, the prices. */
export const DEFAULTS = Object.freeze({
  /** Default place (southern France, ~10 km): the house's own position comes with its configuration. */
  lat: 42.7,
  lon: 2.9,
  /** Peak power (kWc), orientation (180 = south), tilt (degrees). */
  kwc: 6,
  azimuth: 180,
  tilt: 30,
  /** System losses: inverter, wiring, heat, dust (PVGIS uses 14 %). */
  losses: 0.14,
  albedo: 0.2,
  /** Usable battery capacity (kWh), round-trip efficiency, max power as a
   *  share of the capacity per hour (0.5: a 10 kWh battery gives 5 kW). */
  battery: 0,
  batteryEff: 0.9,
  batteryPower: 0.5,
  /** €/kWh paid for the surplus sent to the grid. */
  sellPrice: 0.04,
  /** €/kWh bought, when the meters give no price. */
  buyPrice: 0.2,
  /** € per kWh of battery, installed. */
  batteryCost: 700,
  /** The clouds' draw: same seed, same sky. */
  seed: 2026,
});

// ---- the sun ---------------------------------------------------------------------

/** The sun seen from (`lat`, `lon`) at `ts` (ms, UTC): elevation and azimuth
 *  in degrees (azimuth from north, clockwise: 180 = south), and the cosine
 *  of the zenith angle. NOAA's general solar position formulas (±1°). */
export function sunPosition(ts, lat = DEFAULTS.lat, lon = DEFAULTS.lon) {
  const d = new Date(ts);
  const year = d.getUTCFullYear();
  const jan1 = Date.UTC(year, 0, 1);
  const days = (Date.UTC(year + 1, 0, 1) - jan1) / DAY;
  const midnight = Date.UTC(year, d.getUTCMonth(), d.getUTCDate());
  const hours = (ts - midnight) / HOUR;
  const doy = Math.round((midnight - jan1) / DAY);
  const g = ((2 * Math.PI) / days) * (doy + (hours - 12) / 24);
  const eqTime =
    229.18 *
    (0.000075 + 0.001868 * Math.cos(g) - 0.032077 * Math.sin(g) - 0.014615 * Math.cos(2 * g) - 0.040849 * Math.sin(2 * g));
  const decl =
    0.006918 -
    0.399912 * Math.cos(g) +
    0.070257 * Math.sin(g) -
    0.006758 * Math.cos(2 * g) +
    0.000907 * Math.sin(2 * g) -
    0.002697 * Math.cos(3 * g) +
    0.00148 * Math.sin(3 * g);
  const solarMinutes = hours * 60 + eqTime + 4 * lon;
  const ha = (solarMinutes / 4 - 180) * RAD;
  const phi = lat * RAD;
  const cosZenith = Math.min(1, Math.max(-1, Math.sin(phi) * Math.sin(decl) + Math.cos(phi) * Math.cos(decl) * Math.cos(ha)));
  const fromSouth = Math.atan2(Math.sin(ha), Math.cos(ha) * Math.sin(phi) - Math.tan(decl) * Math.cos(phi));
  return {
    elevation: 90 - Math.acos(cosZenith) / RAD,
    azimuth: (fromSouth / RAD + 180 + 360) % 360,
    cosZenith,
  };
}

/** Clear-sky output (kW per installed kWc) at `ts`: Meinel's direct beam
 *  through Kasten-Young's air mass, a tenth of it as diffuse light, the
 *  ground's reflection, all on the tilted plane, minus the losses. */
export function clearSkyPower(ts, geo = DEFAULTS) {
  const g = { ...DEFAULTS, ...geo };
  const sun = sunPosition(ts, g.lat, g.lon);
  const cz = sun.cosZenith;
  if (cz <= 0.017) return 0; // the sun below ~1°: nothing worth counting
  const zenith = Math.acos(cz) / RAD;
  const airMass = 1 / (cz + 0.50572 * (96.07995 - zenith) ** -1.6364);
  const beam = 1353 * 0.7 ** (airMass ** 0.678);
  const diffuse = 0.1 * beam;
  const horizontal = beam * cz + diffuse;
  const tilt = g.tilt * RAD;
  const sz = Math.sqrt(1 - cz * cz);
  const cosIncidence = cz * Math.cos(tilt) + sz * Math.sin(tilt) * Math.cos((sun.azimuth - g.azimuth) * RAD);
  const plane =
    Math.max(0, beam * cosIncidence) + (diffuse * (1 + Math.cos(tilt))) / 2 + (horizontal * g.albedo * (1 - Math.cos(tilt))) / 2;
  return (plane / 1000) * (1 - g.losses);
}

/** Clear-sky energy (kWh per kWc) between `t0` and `t1` (ms): sampled
 *  every 10 minutes. */
export function clearSkyEnergy(t0, t1, geo = DEFAULTS) {
  if (t1 <= t0) return 0;
  const n = Math.max(1, Math.round((t1 - t0) / 600_000));
  let sum = 0;
  for (let i = 0; i < n; i++) sum += clearSkyPower(t0 + ((i + 0.5) * (t1 - t0)) / n, geo);
  return (sum / n) * ((t1 - t0) / HOUR);
}

// ---- the clouds -------------------------------------------------------------------

/** A number in [0, 1) from integers: the same keys, the same number. */
function noise(...keys) {
  let h = 0x9e3779b9;
  for (const k of keys) {
    h = Math.imul(h ^ (k | 0), 0x85ebca6b);
    h ^= h >>> 13;
    h = Math.imul(h, 0xc2b2ae35);
    h ^= h >>> 16;
  }
  return (h >>> 0) / 4294967296;
}

/** The sky of a day (UTC day number: the whole daylight of a French day
 *  falls in one): clear 55 % of the days, hazy 30 %, overcast 15 %, which
 *  averages about three quarters of the clear-sky output (a sunny region).
 *  `kt`: the share of the clear-sky output that comes through. */
export function skyOfDay(day, seed = DEFAULTS.seed) {
  const u = noise(seed, day, 1);
  const v = noise(seed, day, 2);
  if (u < 0.55) return { kind: 'clair', kt: 0.88 + 0.12 * v, spread: 0.04 };
  if (u < 0.85) return { kind: 'voile', kt: 0.55 + 0.3 * v, spread: 0.3 };
  return { kind: 'couvert', kt: 0.15 + 0.3 * v, spread: 0.2 };
}

/** The share of clear-sky output at the hour starting `ts`: the day's sky,
 *  with passing clouds hour by hour on hazy days. */
export function hourKt(ts, seed = DEFAULTS.seed) {
  const sky = skyOfDay(Math.floor(ts / DAY), seed);
  const wobble = 2 * noise(seed, Math.floor(ts / HOUR), 3) - 1;
  return Math.min(1, Math.max(0.05, sky.kt * (1 + sky.spread * wobble)));
}

/** From a cloud cover (%, the weather's `cloud_cover`): Kasten and
 *  Czeplak's 1 − 0.75 × (cover)^3.4. */
export const ktFromCover = (cover) => 1 - 0.75 * Math.min(1, Math.max(0, cover / 100)) ** 3.4;

export const skyWord = (kt) => (kt >= 0.8 ? 'clair' : kt >= 0.5 ? 'voile' : 'couvert');

/** The kWh each hour would produce per installed kWc. `cloud`: today's
 *  cloud cover (%) from the weather, used for today's hours instead of
 *  the draw. */
export function pvHours(hours, { geo = DEFAULTS, seed = DEFAULTS.seed, cloud = null, now = Date.now() } = {}) {
  const today = Math.floor(now / DAY);
  return hours.map((h) => {
    const kt = cloud != null && Math.floor(h.start / DAY) === today ? ktFromCover(cloud) : hourKt(h.start, seed);
    return clearSkyEnergy(h.start, h.end ?? h.start + HOUR, geo) * kt;
  });
}

/** The day's sky as the simulation sees it: drawn, or today's weather. */
export function skyOf(ts, { seed = DEFAULTS.seed, cloud = null, now = Date.now() } = {}) {
  if (cloud != null && Math.floor(ts / DAY) === Math.floor(now / DAY)) {
    const kt = ktFromCover(cloud);
    return { kind: skyWord(kt), kt, real: true, cover: cloud };
  }
  const sky = skyOfDay(Math.floor(ts / DAY), seed);
  return { kind: sky.kind, kt: sky.kt, real: false };
}

// ---- the house -------------------------------------------------------------------

/** What Moli can move to the sunny hours, recognized by the meter's name.
 *  `flex`: the share that can move (hypothesis); `window`: the hours it may
 *  move to ([from, to), local time). The words (what to do, why it can
 *  move) belong to the page: it says them in the house's language from `kind`. */
const KINDS = [
  {
    kind: 'car',
    test: /voiture|v[ée]hicule|recharge|borne|irve|wallbox|\bve\b/i,
    icon: 'car',
    flex: 1,
    window: [9, 17],
    priority: 2,
  },
  {
    kind: 'water',
    test: /chauffe[- ]?eau|ballon|cumulus|\becs\b/i,
    icon: 'water-boiler',
    flex: 1,
    window: [8, 18],
    priority: 1,
  },
  {
    kind: 'pool',
    test: /piscine|pool|filtration/i,
    icon: 'pool',
    flex: 1,
    window: [8, 19],
    priority: 3,
  },
  {
    kind: 'heat',
    test: /\bpac\b|pompe [àa] chaleur|\bclim/i,
    icon: 'heat-pump',
    flex: 0.25,
    window: [10, 17],
    priority: 4,
  },
];

/** The movable loads among the meters: circuits and appliances measured
 *  at their plug (never an appliance inside a circuit already counted). */
export function findLoads(meters = []) {
  const loads = [];
  const taken = new Set();
  const candidates = [
    ...meters.filter((m) => m.role === 'circuit'),
    ...meters.filter((m) => m.role === 'appliance' && !m.estimated),
  ];
  for (const m of candidates) {
    if (m.within && taken.has(m.within)) continue;
    const kinds = KINDS.filter((k) => k.test.test(m.name));
    if (!kinds.length) continue;
    const [first] = kinds;
    const flex = Math.max(0.25, Math.floor((kinds.reduce((t, k) => t + k.flex, 0) / kinds.length) * 4) / 4);
    taken.add(m.id);
    loads.push({
      id: m.id,
      meters: [m.id],
      name: m.name,
      kind: first.kind,
      kinds: kinds.map((k) => k.kind),
      icon: first.icon,
      flex,
      window: [Math.min(...kinds.map((k) => k.window[0])), Math.max(...kinds.map((k) => k.window[1]))],
      priority: first.priority,
      hypothetical: false,
    });
  }
  return loads;
}

/** Hour and local day (« 2026-10-07 ») of a timestamp in `timeZone`. */
export function localClock(timeZone) {
  let format;
  try {
    format = new Intl.DateTimeFormat('en-CA', { timeZone, year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', hourCycle: 'h23' });
  } catch {
    format = new Intl.DateTimeFormat('en-CA', { year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', hourCycle: 'h23' });
  }
  return (ts) => {
    const p = {};
    for (const x of format.formatToParts(ts)) p[x.type] = x.value;
    return { day: `${p.year}-${p.month}-${p.day}`, hour: Number(p.hour) % 24 };
  };
}

const sum = (list) => list.reduce((t, v) => t + v, 0);

function median(list) {
  if (!list.length) return null;
  const s = [...list].sort((a, b) => a - b);
  return s[Math.floor(s.length / 2)];
}

/** Where a load's energy goes over the day (`hist`, 24 values): the block
 *  of hours around the busiest one, `{ from, to }` (`to` excluded, may be
 *  past midnight). Null for nothing. */
export function mainWindow(hist) {
  const max = Math.max(...hist);
  if (!(max > 0)) return null;
  const peak = hist.indexOf(max);
  const on = (h) => hist[((h % 24) + 24) % 24] >= max * 0.4;
  let from = peak;
  let to = peak;
  while (on(from - 1) && peak - from < 23) from--;
  while (on(to + 1) && to - from < 23) to++;
  return { from: ((from % 24) + 24) % 24, to: (to + 1) % 24 };
}

/** A hypothetical water heater, when none is measured: 4 kWh a night
 *  (22 h → 6 h), taken from what no circuit explains, as long as the house
 *  draws that much at night. Marked as such everywhere. */
function addHypotheticalWater(hours, loads) {
  const id = '__ballon';
  const days = new Set(hours.map((h) => h.day)).size || 1;
  let total = 0;
  for (const h of hours) {
    const night = h.hour >= 22 || h.hour < 6;
    const free = h.kwh - sum(Object.values(h.use));
    const dur = (h.end - h.start) / HOUR;
    h.use[id] = night ? Math.min(0.5 * dur, Math.max(0, free) * 0.6) : 0;
    total += h.use[id];
  }
  if (total / days < 0.5) {
    for (const h of hours) delete h.use[id];
    return;
  }
  loads.push({
    id,
    meters: [],
    name: null, // no meter gives it one: the page names it
    kind: 'water',
    kinds: ['water'],
    icon: 'water-boiler',
    flex: 1,
    window: [8, 18],
    priority: 1,
    hypothetical: true,
    maxKw: 2,
  });
}

/**
 * The house hour by hour, from an hourly energy report: `{ hours, loads,
 * timezone, priced }`. Each hour: `{ start, end, day, hour, kwh, price,
 * use }` (`use`: kWh of each movable load). `end` stops at `now` for the
 * hour under way. The price is what the house paid that hour (its meters'
 * cost), else the usual price of that hour of the day, else `buyPrice`.
 */
export function buildData(report, { now = Date.now(), buyPrice = DEFAULTS.buyPrice, hypothetical = true } = {}) {
  const meters = report?.meters ?? [];
  const total = meters.find((m) => m.role === 'total');
  const grid = meters.filter((m) => m.role === 'grid');
  const clock = localClock(report?.timezone);
  const loads = findLoads(meters);
  const hours = [];
  let priced = false;
  for (const b of report?.buckets ?? []) {
    if (b.start >= now) continue;
    let kwh = null;
    let cost = null;
    if (total && b.meters[total.id]) {
      kwh = b.meters[total.id].kwh;
      cost = b.meters[total.id].cost ?? null;
    } else if (grid.some((g) => b.meters[g.id])) {
      kwh = sum(grid.map((g) => b.meters[g.id]?.kwh ?? 0));
      cost = grid.some((g) => b.meters[g.id]?.cost != null) ? sum(grid.map((g) => b.meters[g.id]?.cost ?? 0)) : null;
    }
    if (kwh == null || !Number.isFinite(kwh)) continue;
    kwh = Math.max(0, kwh);
    const use = {};
    for (const l of loads) use[l.id] = Math.max(0, sum(l.meters.map((id) => b.meters[id]?.kwh ?? 0)));
    // The circuits never add up to more than the whole house.
    const used = sum(Object.values(use));
    if (used > kwh && used > 0) for (const id in use) use[id] *= kwh / used;
    const { day, hour } = clock(b.start);
    const price = kwh > 0.02 && cost != null && cost > 0 ? cost / kwh : null;
    if (price != null) priced = true;
    hours.push({ start: b.start, end: Math.min(b.start + HOUR, now), day, hour, kwh, price, use });
  }
  const byHour = Array.from({ length: 24 }, () => []);
  for (const h of hours) if (h.price != null) byHour[h.hour].push(h.price);
  const usual = byHour.map(median);
  const overall = median(hours.filter((h) => h.price != null).map((h) => h.price));
  for (const h of hours) h.price ??= usual[h.hour] ?? overall ?? buyPrice;

  if (hypothetical && hours.length && !loads.some((l) => l.kinds.includes('water'))) addHypotheticalWater(hours, loads);

  const days = new Set(hours.map((h) => h.day)).size || 1;
  for (const l of loads) {
    const hist = new Array(24).fill(0);
    let peak = 0;
    let energy = 0;
    for (const h of hours) {
      const v = h.use[l.id] ?? 0;
      hist[h.hour] += v / days;
      energy += v;
      const dur = (h.end - h.start) / HOUR;
      if (dur > 0.5) peak = Math.max(peak, v / dur);
    }
    l.perDay = energy / days;
    l.usual = mainWindow(hist);
    l.maxKw ??= Math.min(11, Math.max(0.5, peak));
  }
  return { hours, loads, timezone: report?.timezone ?? null, priced };
}

// ---- the simulation --------------------------------------------------------------

const inWindow = (hour, [from, to]) => (from <= to ? hour >= from && hour < to : hour >= from || hour < to);

function groupByDay(rows) {
  const out = new Map();
  rows.forEach((r, i) => {
    if (!out.has(r.day)) out.set(r.day, []);
    out.get(r.day).push(i);
  });
  return [...out.values()];
}

/**
 * Hour by hour with panels: `pv1` (kWh per kWc for each hour, `pvHours`)
 * times `kwc`; the movable loads Moli pilots (`pilot`: load id → share
 * moved) shifted, within their day, to the hours where the panels give
 * more than the rest of the house draws; then the battery, if any. Each
 * row: `{ start, end, day, hour, price, pv, load0, load, moved, direct,
 * battIn, battOut, soc, export, import }` (kWh; `load0` before shifting).
 */
export function simulate(data, pv1, options = {}) {
  const o = { ...DEFAULTS, ...options };
  const pilot = o.pilot ?? {};
  const rows = data.hours.map((h, i) => ({
    start: h.start,
    end: h.end,
    day: h.day,
    hour: h.hour,
    price: h.price,
    pv: Math.max(0, (pv1[i] ?? 0) * o.kwc),
    load0: h.kwh,
    load: h.kwh,
    moved: {},
  }));

  const active = data.loads.filter((l) => pilot[l.id] > 0).sort((a, b) => a.priority - b.priority);
  if (active.length && o.kwc > 0) {
    for (const idx of groupByDay(rows)) {
      const flexible = active.map((l) => idx.map((i) => Math.min(1, pilot[l.id]) * (data.hours[i].use[l.id] ?? 0)));
      // What stays put whatever happens, and the sun left over above it.
      const load = idx.map((i, k) => rows[i].load0 - sum(flexible.map((f) => f[k])));
      const room = idx.map((i, k) => Math.max(0, rows[i].pv - load[k]));
      active.forEach((l, n) => {
        const f = flexible[n];
        const energy = sum(f);
        if (energy <= EPS) return;
        let left = energy;
        const put = new Array(idx.length).fill(0);
        const order = idx
          .map((_, k) => k)
          .filter((k) => inWindow(rows[idx[k]].hour, l.window))
          .sort((a, b) => room[b] - room[a]);
        for (const k of order) {
          if (left <= EPS || room[k] <= EPS) break;
          const r = rows[idx[k]];
          const x = Math.min(room[k], l.maxKw * ((r.end - r.start) / HOUR), left);
          put[k] += x;
          room[k] -= x;
          left -= x;
        }
        // What found no sun stays where it was (the off-peak night, usually).
        const keep = left / energy;
        idx.forEach((i, k) => {
          const stay = f[k] * keep;
          load[k] += put[k] + stay;
          room[k] = Math.max(0, room[k] - stay);
          if (put[k] > EPS) rows[i].moved[l.id] = put[k];
        });
      });
      idx.forEach((i, k) => (rows[i].load = Math.max(0, load[k])));
    }
  }

  const cap = Math.max(0, o.battery);
  const eta = Math.sqrt(o.batteryEff);
  let soc = 0;
  for (const r of rows) {
    const dur = (r.end - r.start) / HOUR;
    const direct = Math.min(r.pv, r.load);
    let surplus = r.pv - direct;
    let need = r.load - direct;
    let battIn = 0;
    let battOut = 0;
    if (cap > 0) {
      battIn = Math.max(0, Math.min(surplus, cap * o.batteryPower * dur, (cap - soc) / eta));
      soc += battIn * eta;
      surplus -= battIn;
      battOut = Math.max(0, Math.min(need, cap * o.batteryPower * dur, soc * eta));
      soc -= battOut / eta;
      need -= battOut;
    }
    r.direct = direct;
    r.battIn = battIn;
    r.battOut = battOut;
    r.soc = soc;
    r.export = Math.max(0, surplus);
    r.import = Math.max(0, need);
  }
  return rows;
}

/**
 * Totals over rows: energies (kWh), the bill without panels (`before`) and
 * with them (`after`, what is still bought), the surplus sold
 * (`revenue`), and the two rates:
 * - autoconsumption: share of the production used on site (directly or
 *   through the battery): 1 − exported / produced;
 * - autoproduction: share of the house's needs the sun covers:
 *   1 − bought / consumed.
 */
export function summarize(rows, sellPrice = DEFAULTS.sellPrice) {
  const t = { pv: 0, load: 0, direct: 0, battIn: 0, battOut: 0, export: 0, import: 0, before: 0, after: 0, hours: rows.length };
  for (const r of rows) {
    t.pv += r.pv;
    t.load += r.load;
    t.direct += r.direct;
    t.battIn += r.battIn;
    t.battOut += r.battOut;
    t.export += r.export;
    t.import += r.import;
    t.before += r.load0 * r.price;
    t.after += r.import * r.price;
  }
  t.selfUsed = t.pv - t.export;
  t.autoconsumption = t.pv > EPS ? t.selfUsed / t.pv : null;
  t.autoproduction = t.load > EPS ? (t.load - t.import) / t.load : null;
  t.revenue = t.export * sellPrice;
  t.savings = t.before - t.after;
  t.gain = t.savings + t.revenue;
  t.days = new Set(rows.map((r) => r.day)).size;
  return t;
}

/** Day by day: `[{ day, start, rows, ...summarize }]`, oldest first. */
export function byDay(rows, sellPrice = DEFAULTS.sellPrice) {
  return groupByDay(rows).map((idx) => {
    const list = idx.map((i) => rows[i]);
    return { day: list[0].day, start: list[0].start, rows: list, ...summarize(list, sellPrice) };
  });
}

/** The last `n` days of the rows (today included): `{ from, rows }`. */
export function lastDays(rows, n = 30) {
  const keys = [...new Set(rows.map((r) => r.day))].slice(-n);
  const first = rows.find((r) => r.day === keys[0]);
  return { from: first?.start ?? 0, rows: rows.filter((r) => r.start >= (first?.start ?? 0)) };
}

/** Where Moli put a load's energy, as a block of hours. */
function movedWindow(rows, id) {
  const hist = new Array(24).fill(0);
  for (const r of rows) hist[r.hour] += r.moved[id] ?? 0;
  return { window: mainWindow(hist), kwh: sum(hist) };
}

const scale = (t) => (t.days > 0 ? 30 / t.days : 1);

/** The share of a load Moli would move: as piloted, else as chosen
 *  (`shares`), else its kind's default. */
const shareOf = (o, l) => (o.pilot?.[l.id] > 0 ? o.pilot[l.id] : (o.shares?.[l.id] ?? l.flex));

/** What each load brings: the simulation with it piloted (at its share)
 *  against without, `{ [id]: { gain (€/month), points, autoprod, kwh
 *  (moved per month), window } }`. `options.shares`: load id → share. */
export function loadEffects(data, pv1, options, from = -Infinity) {
  const o = { ...DEFAULTS, ...options, pilot: { ...(options.pilot ?? {}) } };
  const out = {};
  for (const l of data.loads) {
    const share = shareOf(o, l);
    const withRows = simulate(data, pv1, { ...o, pilot: { ...o.pilot, [l.id]: share } }).filter((r) => r.start >= from);
    const without = summarize(
      simulate(data, pv1, { ...o, pilot: { ...o.pilot, [l.id]: 0 } }).filter((r) => r.start >= from),
      o.sellPrice,
    );
    const t = summarize(withRows, o.sellPrice);
    const moved = movedWindow(withRows, l.id);
    out[l.id] = {
      gain: (t.gain - without.gain) * scale(t),
      points: ((t.autoconsumption ?? 0) - (without.autoconsumption ?? 0)) * 100,
      autoprod: ((t.autoproduction ?? 0) - (without.autoproduction ?? 0)) * 100,
      kwh: moved.kwh * scale(t),
      window: moved.window,
    };
  }
  return out;
}

/**
 * Improvements, best first: each movable load not piloted yet, and a
 * battery when there is none. Every figure comes from running the
 * simulation with the change: `{ id, type, load?, battery?, icon, gain
 * (€/month), points (autoconsumption), autoprod, years? }`, and what the
 * page needs to write it up: for a load, its `kinds`, the `window` Moli
 * would use, the `usual` one, `kwh` moved per month; for a battery, its
 * `cost`, the price per kWh (`rate`) and `back` (kWh a month given back).
 */
export function proposals(data, pv1, options, from = -Infinity) {
  const o = { ...DEFAULTS, ...options, pilot: { ...(options.pilot ?? {}) } };
  const run = (x) => summarize(simulate(data, pv1, x).filter((r) => r.start >= from), x.sellPrice);
  const base = run(o);
  if (!(base.pv > EPS)) return [];
  const out = [];
  const effects = loadEffects(data, pv1, o, from);
  for (const l of data.loads) {
    if (o.pilot[l.id] > 0) continue;
    const e = effects[l.id];
    if (!e?.window || e.kwh < 0.5) continue;
    out.push({
      id: `load:${l.id}`,
      type: 'load',
      load: l.id,
      kinds: l.kinds,
      flex: shareOf(o, l),
      icon: l.icon,
      window: e.window,
      usual: l.usual,
      kwh: e.kwh,
      hypothetical: l.hypothetical,
      gain: e.gain,
      points: e.points,
      autoprod: e.autoprod,
    });
  }
  if (!(o.battery > 0)) {
    for (const cap of [5, 10]) {
      const t = run({ ...o, battery: cap });
      const gain = (t.gain - base.gain) * scale(t);
      const cost = cap * o.batteryCost;
      out.push({
        id: `battery:${cap}`,
        type: 'battery',
        battery: cap,
        icon: 'battery',
        back: t.battOut * scale(t),
        cost,
        rate: o.batteryCost,
        gain,
        points: ((t.autoconsumption ?? 0) - (base.autoconsumption ?? 0)) * 100,
        autoprod: ((t.autoproduction ?? 0) - (base.autoproduction ?? 0)) * 100,
        years: gain > 0 ? cost / (gain * 12) : null,
      });
    }
  }
  return out.filter((p) => p.gain > 0.05 || p.points > 0.5).sort((a, b) => b.gain - a.gain);
}

/** The flows right now (kW): sun to house, sun to grid, grid to house,
 *  and the battery's share, from what the panels give and the house
 *  draws. `battery`: `{ cap, soc, power }` (kWh, kWh, kW) or null. */
export function flowNow(pvKw, loadKw, battery = null) {
  const pv = Math.max(0, pvKw ?? 0);
  const load = Math.max(0, loadKw ?? 0);
  const toHouse = Math.min(pv, load);
  let surplus = pv - toHouse;
  let need = load - toHouse;
  let toBattery = 0;
  let fromBattery = 0;
  if (battery?.cap > 0) {
    if (surplus > 0 && battery.soc < battery.cap - 0.01) toBattery = Math.min(surplus, battery.power);
    if (need > 0 && battery.soc > 0.01) fromBattery = Math.min(need, battery.power);
    surplus -= toBattery;
    need -= fromBattery;
  }
  return { pv, load, toHouse, toGrid: surplus, fromGrid: need, toBattery, fromBattery };
}
