// Checks the solar demo's model (src/maison/lib/solar-sim.js) on made-up
// data: no network, no Moli. `node web/scripts/solar-sim.check.mjs`
// prints each check and exits 1 on the first broken invariant.

import {
  DEFAULTS,
  sunPosition,
  clearSkyPower,
  clearSkyEnergy,
  hourKt,
  pvHours,
  buildData,
  simulate,
  summarize,
  byDay,
  lastDays,
  loadEffects,
  proposals,
  flowNow,
  mainWindow,
} from '../src/maison/lib/solar-sim.js';

const HOUR = 3_600_000;
const DAY = 86_400_000;
let failed = 0;
let passed = 0;

function check(name, ok, detail = '') {
  if (ok) {
    passed += 1;
    console.log(`  ok   ${name}${detail ? ` (${detail})` : ''}`);
  } else {
    failed += 1;
    console.log(`  FAIL ${name}${detail ? ` (${detail})` : ''}`);
  }
}

const near = (a, b, tol) => Math.abs(a - b) <= tol;
const r1 = (v) => Math.round(v * 10) / 10;

// ---- a made-up house: 31 days of hours, Paris time --------------------------------

const clock = new Intl.DateTimeFormat('en-CA', { timeZone: 'Europe/Paris', hour: '2-digit', hourCycle: 'h23' });
const hourOf = (ts) => Number(clock.format(ts)) % 24;

function report(nowTs, { water = true, car = true } = {}) {
  const meters = [
    { id: 'linky-hc', name: 'Linky heures creuses', role: 'grid' },
    { id: 'linky-hp', name: 'Linky heures pleines', role: 'grid' },
    { id: 'general', name: 'Général', role: 'total' },
    { id: 'c2', name: 'PAC salon + pompe piscine', role: 'circuit' },
  ];
  if (water) meters.push({ id: 'c4', name: 'Chauffe-eau', role: 'circuit' });
  if (car) meters.push({ id: 'c6', name: 'Voiture électrique', role: 'circuit' });
  meters.push({ id: 'tele', name: 'Télé', role: 'appliance' });
  const first = Math.floor(nowTs / HOUR) * HOUR - 743 * HOUR;
  const buckets = [];
  for (let t = first; t <= nowTs; t += HOUR) {
    const h = hourOf(t);
    const d = Math.floor(t / DAY);
    const hc = h >= 22 || h < 6;
    const price = hc ? 0.137155 : 0.172103;
    // The floor, a morning bump, an evening peak.
    let base = 0.3 + (h >= 7 && h < 9 ? 0.4 : 0) + (h >= 18 && h < 23 ? 0.9 : 0) + (h >= 12 && h < 14 ? 0.3 : 0);
    const pac = h >= 13 && h < 19 ? 0.5 : 0;
    const ballon = water && (h >= 22 || h < 2) ? 1.1 : 0;
    const voiture = car && d % 3 === 0 && (h >= 21 || h < 1) ? 1.5 : 0;
    const tele = h >= 19 && h < 23 ? 0.09 : 0.01;
    const partial = t + HOUR > nowTs ? (nowTs - t) / HOUR : 1;
    const total = (base + pac + ballon + voiture) * partial;
    const m = {
      general: { kwh: total, cost: total * price },
      c2: { kwh: pac * partial, cost: pac * partial * price },
      tele: { kwh: tele * partial, cost: tele * partial * price },
      [hc ? 'linky-hc' : 'linky-hp']: { kwh: total, cost: total * price },
    };
    if (water) m.c4 = { kwh: ballon * partial, cost: ballon * partial * price };
    if (car) m.c6 = { kwh: voiture * partial, cost: voiture * partial * price };
    // A missing hour now and then (Moli restarting): skipped, not zero.
    if (d % 11 === 5 && h === 3) continue;
    buckets.push({ start: t, meters: m });
  }
  return { step: 'hour', currency: '€', timezone: 'Europe/Paris', meters, buckets };
}

// ---- the sun -----------------------------------------------------------------------

console.log('Soleil');
{
  // 21 June, solar noon at the default place (42.7° N, 2.9° E) ≈ 11:50 UTC: elevation 90 − 42.7 + 23.44.
  let best = { elevation: -90 };
  for (let m = 0; m < 24 * 60; m++) {
    const ts = Date.UTC(2026, 5, 21, 0, m);
    const s = sunPosition(ts);
    if (s.elevation > best.elevation) best = { ...s, ts };
  }
  const noon = new Date(best.ts);
  check('solstice d’été : hauteur à midi solaire', near(best.elevation, 70.7, 1), `${r1(best.elevation)}°`);
  check('solstice d’été : plein sud à midi solaire', near(best.azimuth, 180, 2), `${r1(best.azimuth)}°`);
  check('midi solaire vers 11 h 50 UTC', near(noon.getUTCHours() * 60 + noon.getUTCMinutes(), 11 * 60 + 50, 5), noon.toISOString().slice(11, 16));
  const winter = sunPosition(Date.UTC(2026, 11, 21, 11, 50));
  check('solstice d’hiver : soleil bas à midi', near(winter.elevation, 90 - 42.7 - 23.44, 1.5), `${r1(winter.elevation)}°`);
  check('le matin, le soleil est à l’est', sunPosition(Date.UTC(2026, 5, 21, 6)).azimuth < 120);
  check('l’après-midi, le soleil est à l’ouest', sunPosition(Date.UTC(2026, 5, 21, 17)).azimuth > 240);
}

console.log('Production');
{
  let night = 0;
  for (const h of [0, 1, 2, 3, 21, 22, 23]) night += clearSkyEnergy(Date.UTC(2026, 5, 21, h), Date.UTC(2026, 5, 21, h + 1));
  check('production nulle la nuit', night === 0, `${night} kWh`);
  for (const [month, label] of [
    [2, 'mars'],
    [5, 'juin'],
    [9, 'octobre'],
    [11, 'décembre'],
  ]) {
    const day = Date.UTC(2026, month, 15);
    const hours = Array.from({ length: 24 }, (_, h) => clearSkyEnergy(day + h * HOUR, day + (h + 1) * HOUR));
    const peak = hours.indexOf(Math.max(...hours));
    check(`pic vers midi solaire (${label})`, peak === 11, `heure UTC ${peak}`);
    // As much 2 h 30 before solar noon as after (south-facing panels).
    let noon = day;
    for (let m = 0; m < 24 * 60; m++) if (sunPosition(day + m * 60_000).elevation > sunPosition(noon).elevation) noon = day + m * 60_000;
    const before = clearSkyPower(noon - 150 * 60_000);
    const after = clearSkyPower(noon + 150 * 60_000);
    check(`courbe symétrique autour de midi solaire (${label})`, Math.abs(before - after) < 0.02 * Math.max(before, after), `${r1(before * 100) / 100} / ${r1(after * 100) / 100} kW`);
  }
  // One kWc over a year, with the clouds drawn: a sunny-region figure.
  let year = 0;
  let clear = 0;
  for (let t = Date.UTC(2026, 0, 1); t < Date.UTC(2027, 0, 1); t += HOUR) {
    const e = clearSkyEnergy(t, t + HOUR);
    clear += e;
    year += e * hourKt(t);
  }
  check('rendement annuel plausible (1 kWc, sud 30°)', year > 1250 && year < 1750, `${Math.round(year)} kWh/kWc, ciel clair ${Math.round(clear)}`);
  check('nébulosité déterministe', hourKt(Date.UTC(2026, 9, 7, 12)) === hourKt(Date.UTC(2026, 9, 7, 12)));
  const tilted = clearSkyEnergy(Date.UTC(2026, 11, 21, 11), Date.UTC(2026, 11, 21, 12), { tilt: 45 });
  const flat = clearSkyEnergy(Date.UTC(2026, 11, 21, 11), Date.UTC(2026, 11, 21, 12), { tilt: 0 });
  check('en hiver, incliner vers le sud produit plus qu’à plat', tilted > flat, `${r1(tilted * 100) / 100} > ${r1(flat * 100) / 100}`);
  const east = clearSkyEnergy(Date.UTC(2026, 5, 21, 7), Date.UTC(2026, 5, 21, 8), { azimuth: 90 });
  const west = clearSkyEnergy(Date.UTC(2026, 5, 21, 7), Date.UTC(2026, 5, 21, 8), { azimuth: 270 });
  check('le matin, des panneaux à l’est produisent plus qu’à l’ouest', east > west);
}

// ---- the house, with panels -------------------------------------------------------

const NOW = Date.UTC(2026, 9, 7, 12, 30); // 7 October, 14 h 30 in Paris
const data = buildData(report(NOW), { now: NOW });
const pv1 = pvHours(data.hours, { now: NOW });

console.log('Données');
{
  check('heures lues (une par heure mesurée)', data.hours.length > 700 && data.hours.length < 745, `${data.hours.length}`);
  check('jamais une heure dans le futur', data.hours.every((h) => h.start < NOW && h.end <= NOW));
  check('l’heure en cours s’arrête à maintenant', data.hours.at(-1).end === NOW);
  const kinds = data.loads.map((l) => l.kind).sort().join(',');
  check('charges pilotables reconnues', kinds === 'car,pool,water', kinds);
  const mixed = data.loads.find((l) => l.id === 'c2');
  check('circuit mixte PAC + piscine : part décalable partielle', mixed?.flex === 0.5, `${mixed?.flex}`);
  check('prix réels relus heure par heure', data.priced && data.hours.some((h) => near(h.price, 0.137155, 1e-6)) && data.hours.some((h) => near(h.price, 0.172103, 1e-6)));
  const car = data.loads.find((l) => l.kind === 'car');
  check('la voiture charge le soir (fenêtre habituelle)', car.usual?.from === 21 && car.usual?.to === 1, `${car.usual?.from} h → ${car.usual?.to} h`);
  check('puissance de charge relue (1,5 kW)', near(car.maxKw, 1.5, 0.01), `${car.maxKw}`);
  const noWater = buildData(report(NOW, { water: false }), { now: NOW });
  const ghost = noWater.loads.find((l) => l.kind === 'water');
  check('sans chauffe-eau mesuré : un ballon hypothétique, marqué', ghost?.hypothetical === true, ghost?.id);
  const pv = pvHours(data.hours, { now: NOW });
  check('production nulle la nuit (heures de la maison)', data.hours.every((h, i) => (h.hour >= 22 || h.hour < 6 ? pv[i] === 0 : true)));
  const sunny = pvHours(data.hours, { now: NOW, cloud: 0 });
  const grey = pvHours(data.hours, { now: NOW, cloud: 100 });
  const today = (list) => list.filter((_, i) => Math.floor(data.hours[i].start / DAY) === Math.floor(NOW / DAY)).reduce((t, v) => t + v, 0);
  check('météo du jour prise en compte (100 % de nuages < 0 %)', today(grey) < today(sunny) * 0.4, `${r1(today(grey))} < ${r1(today(sunny))} kWh/kWc`);
  check('mainWindow traverse minuit', JSON.stringify(mainWindow([1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1])) === '{"from":22,"to":2}');
}

function invariants(label, rows, cap = 0) {
  const tol = 1e-9;
  let worst = '';
  const ok = rows.every((r) => {
    const self = r.direct + r.battOut;
    const tests = [
      [r.direct <= r.pv + tol, 'autoconsommé ≤ production'],
      [self <= r.load + tol, 'autoconsommé ≤ consommation'],
      [Math.abs(r.direct + r.battIn + r.export - r.pv) < 1e-6, 'production = directe + batterie + surplus'],
      [Math.abs(r.direct + r.battOut + r.import - r.load) < 1e-6, 'consommation = directe + batterie + réseau'],
      [[r.pv, r.load, r.direct, r.battIn, r.battOut, r.export, r.import].every((v) => v >= -tol), 'aucune valeur négative'],
      [r.soc >= -tol && r.soc <= cap + tol, 'batterie entre vide et pleine'],
    ];
    const bad = tests.find(([t]) => !t);
    if (bad) worst = `${bad[1]} à ${new Date(r.start).toISOString()}`;
    return !bad;
  });
  check(`${label} : bilans heure par heure`, ok, worst);
  const days = byDay(rows);
  const conserved = days.every((d) => Math.abs(d.rows.reduce((t, r) => t + r.load, 0) - d.rows.reduce((t, r) => t + r.load0, 0)) < 1e-6);
  check(`${label} : la conso de chaque jour est conservée (rien ne disparaît)`, conserved);
}

console.log('Simulation');
{
  const plain = simulate(data, pv1, { kwc: 6 });
  invariants('6 kWc seuls', plain);
  const t = summarize(plain);
  check('taux entre 0 et 100 %', t.autoconsumption > 0 && t.autoconsumption <= 1 && t.autoproduction > 0 && t.autoproduction <= 1, `${Math.round(t.autoconsumption * 100)} % / ${Math.round(t.autoproduction * 100)} %`);
  check('production totale = consommée sur place + surplus', Math.abs(t.pv - t.selfUsed - t.export) < 1e-6);
  check('économies positives, surplus revendu au tarif choisi', t.savings > 0 && near(t.revenue, t.export * DEFAULTS.sellPrice, 1e-9));
  check('sans panneaux, rien ne change', summarize(simulate(data, pv1, { kwc: 0 })).savings === 0);

  const all = Object.fromEntries(data.loads.map((l) => [l.id, l.flex]));
  const piloted = simulate(data, pv1, { kwc: 6, pilot: all });
  invariants('6 kWc + pilotage', piloted);
  const p = summarize(piloted);
  check('le pilotage n’achète jamais plus au réseau', p.import <= t.import + 1e-6, `${r1(p.import)} ≤ ${r1(t.import)} kWh`);
  check('le pilotage améliore l’autoconsommation', p.autoconsumption > t.autoconsumption + 0.05, `${Math.round(t.autoconsumption * 100)} → ${Math.round(p.autoconsumption * 100)} %`);
  check('le pilotage rapporte', p.gain > t.gain, `${r1(t.gain)} → ${r1(p.gain)} €`);
  const car = data.loads.find((l) => l.kind === 'car');
  const moved = piloted.filter((r) => r.moved[car.id] > 0);
  check('la voiture ne charge que dans sa fenêtre (9 h → 17 h)', moved.length > 0 && moved.every((r) => r.hour >= 9 && r.hour < 17));
  check('jamais au-dessus de la puissance de charge', moved.every((r) => r.moved[car.id] <= car.maxKw * ((r.end - r.start) / HOUR) + 1e-9));
  check('un déplacement ne prend que du surplus', piloted.every((r) => Object.values(r.moved).reduce((s, v) => s + v, 0) <= r.pv + 1e-9));

  for (const cap of [5, 10]) {
    const b = simulate(data, pv1, { kwc: 6, battery: cap });
    invariants(`6 kWc + batterie ${cap} kWh`, b, cap);
    const s = summarize(b);
    check(`batterie ${cap} kWh : plus d’autoconsommation`, s.autoconsumption > t.autoconsumption, `${Math.round(s.autoconsumption * 100)} %`);
    check(`batterie ${cap} kWh : rend moins qu’elle ne reçoit (rendement)`, s.battOut <= s.battIn * DEFAULTS.batteryEff + 1e-6);
  }
  const small = summarize(simulate(data, pv1, { kwc: 3 }));
  const big = summarize(simulate(data, pv1, { kwc: 9 }));
  check('plus de panneaux : plus d’autoproduction, moins d’autoconsommation', big.autoproduction > small.autoproduction && big.autoconsumption < small.autoconsumption);
  const again = summarize(simulate(data, pvHours(data.hours, { now: NOW }), { kwc: 6, pilot: all, battery: 5 }));
  const once = summarize(simulate(data, pv1, { kwc: 6, pilot: all, battery: 5 }));
  check('déterministe (mêmes entrées, mêmes chiffres)', again.gain === once.gain && again.pv === once.pv);
}

console.log('Propositions');
{
  const { from, rows } = lastDays(simulate(data, pv1, { kwc: 6 }), 30);
  check('30 derniers jours', new Set(rows.map((r) => r.day)).size === 30);
  const list = proposals(data, pv1, { kwc: 6 }, from);
  check('des propositions', list.length >= 3, list.map((p) => `${p.type === 'load' ? p.kinds.join(' + ') : `batterie ${p.battery} kWh`} (${r1(p.gain)} €/mois, ${p.points > 0 ? '+' : ''}${r1(p.points)} pts)`).join(' ; '));
  check('classées par gain', list.every((p, i) => i === 0 || list[i - 1].gain >= p.gain));
  check('chiffres finis', list.every((p) => Number.isFinite(p.gain) && Number.isFinite(p.points)));
  check('la recharge de la voiture est proposée en journée', list.some((p) => p.type === 'load' && p.kinds.includes('car') && p.window.from >= 9 && p.window.from <= 16 && p.window.to >= 10 && p.window.to <= 17));
  const car = data.loads.find((l) => l.kind === 'car');
  const after = proposals(data, pv1, { kwc: 6, pilot: { [car.id]: 1 } }, from);
  check('une charge déjà pilotée n’est plus proposée', !after.some((p) => p.load === car.id));
  check('avec une batterie, plus de proposition de batterie', !proposals(data, pv1, { kwc: 6, battery: 5 }, from).some((p) => p.type === 'battery'));
  const effects = loadEffects(data, pv1, { kwc: 6 }, from);
  check('effet de chaque charge pilotable', Object.values(effects).every((e) => Number.isFinite(e.gain) && e.gain >= -1e-6), Object.entries(effects).map(([id, e]) => `${id} ${r1(e.gain)} €`).join(', '));
}

console.log('Flux du moment');
{
  const f = flowNow(4, 1.5);
  check('soleil 4 kW, maison 1,5 kW : 2,5 kW au réseau', near(f.toHouse, 1.5, 1e-9) && near(f.toGrid, 2.5, 1e-9) && f.fromGrid === 0);
  const n = flowNow(0, 0.8, { cap: 5, soc: 2, power: 2.5 });
  check('la nuit, la batterie alimente la maison', near(n.fromBattery, 0.8, 1e-9) && n.fromGrid === 0);
  const e = flowNow(0, 3, { cap: 5, soc: 2, power: 2.5 });
  check('au-delà de sa puissance, le réseau complète', near(e.fromBattery, 2.5, 1e-9) && near(e.fromGrid, 0.5, 1e-9));
}

console.log(failed ? `\n${failed} échec(s), ${passed} vérifications passées.` : `\nTout est bon : ${passed} vérifications.`);
process.exit(failed ? 1 : 0);
