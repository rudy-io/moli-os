import { test, before, beforeEach, after } from 'node:test';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { createServer } from 'vite';

// Render the actual page and its cards. Every synthetic point is writable:
// this view must remain observational even when the driver allows commands.
let vite;
let render;
let hub;
let home;
let words;
let Page;
let ZoneCard;
let Readings;
let DiscoveryResults;
const storage = Object.getOwnPropertyDescriptor(globalThis, 'localStorage');
const document = Object.getOwnPropertyDescriptor(globalThis, 'document');

before(async () => {
  delete globalThis.localStorage;
  globalThis.document = { querySelector: () => null, documentElement: {} };
  vite = await createServer({
    root: fileURLToPath(new URL('../../../', import.meta.url)),
    server: { middlewareMode: true, watch: null, hmr: false },
    logLevel: 'error',
  });
  ({ render } = await vite.ssrLoadModule('svelte/server'));
  ({ hub } = await vite.ssrLoadModule('/src/lib/hub.svelte.js'));
  ({ home } = await vite.ssrLoadModule('/src/maison/lib/home.svelte.js'));
  words = await vite.ssrLoadModule('/src/lib/i18n.svelte.js');
  Page = (await vite.ssrLoadModule('/src/maison/pages/Airzone.svelte')).default;
  ZoneCard = (await vite.ssrLoadModule('/src/maison/ui/AirzoneZoneCard.svelte')).default;
  Readings = (await vite.ssrLoadModule('/src/maison/ui/AirzoneReadings.svelte')).default;
  DiscoveryResults = (await vite.ssrLoadModule('/src/maison/ui/AirzoneDiscoveryResults.svelte')).default;
});

beforeEach(async () => {
  await words.setLanguage('fr');
  hub.devices = {};
  hub.drivers = [];
  hub.connected = true;
  hub.chart = null;
  home.config = {};
});

after(async () => {
  await vite?.close();
  delete globalThis.document;
  if (document) Object.defineProperty(globalThis, 'document', document);
  if (storage) Object.defineProperty(globalThis, 'localStorage', storage);
});

function device(id, name, values = {}, options = {}) {
  const points = Object.entries(values).map(([key, value]) => ({
    key,
    label: `Reading ${key}`,
    kind: key === 'mode' ? { type: 'enum', values: ['off', 'heat', 'cool'] }
      : { type: typeof value === 'boolean' ? 'binary' : typeof value === 'number' ? 'numeric' : 'text' },
    unit: ['temperature', 'target_temperature', 'heatsetpoint', 'coolsetpoint'].includes(key) ? '°C'
      : ['humidity', 'battery', 'coverage'].includes(key) ? '%' : null,
    access: { read: true, write: true },
    semantic: 'other',
  }));
  return {
    id, instance: id.split(':')[0], native_name: name, label: {},
    manufacturer: 'Airzone', model: 'Local API', online: true, members: [], points,
    state: Object.fromEntries(Object.entries(values).map(([key, value]) => [key, { value, ts: 1_700_000_000_000 }])),
    ...options,
  };
}

const driver = (instance, kind = 'airzone') => ({ instance, kind, status: { state: 'running' } });
const setDevices = (devices) => { hub.devices = Object.fromEntries(devices.map((d) => [d.id, d])); };
const page = (route = 'airzone') => render(Page, { props: { route } }).body;
const text = (html) => html.replace(/<!--[\s\S]*?-->/g, '').replace(/<[^>]*>/g, ' ').replace(/\s+/g, ' ').trim();
const tags = (html, tag) => [...html.matchAll(new RegExp(`<${tag}\\b[^>]*>`, 'g'))].map(([value]) => value);
const cardLinks = (html) => tags(html, 'a').filter((tag) => /class="zone-card\b/.test(tag));

function observationOnly(html, filters = false) {
  assert.doesNotMatch(html, /role="(?:switch|slider)"|type="(?:range|number|checkbox|submit)"|<form\b/);
  for (const input of tags(html, 'input')) {
    assert.ok(filters && /type="search"/.test(input), `unexpected editor: ${input}`);
  }
  for (const select of tags(html, 'select')) {
    assert.ok(filters && /aria-label="Filtrer par système"/.test(select), `unexpected setting selector: ${select}`);
  }
  for (const button of tags(html, 'button')) {
    if (filters && /class="filter-chip\b/.test(button)) {
      assert.match(button, /aria-pressed="(?:true|false)"/);
      continue;
    }
    if (filters && /class="filter-reset\b/.test(button)) continue;
    if (/class="refresh\b/.test(button)) continue; // Stored configuration refresh, never an equipment command.
    assert.match(button, /class="metric\b/, 'only observed metrics can use a button in the zone detail');
    if (!/\bdisabled\b/.test(button)) {
      assert.match(button, /aria-label="[^"]+Voir l’historique"/, 'available metrics name their history navigation');
    }
  }
}

function house() {
  const selected = device('native-a:aabbccddeeff_s1_z2', 'Selected zone', {
    systemID: 1, zoneID: 2, on: true, mode: 'heat', temperature: 20.5, target_temperature: 21,
    humidity: 0, heat_demand: true, problem: false,
  });
  const sibling = device('native-a:aabbccddeeff_s1_z3', 'Sibling zone', { systemID: 1, zoneID: 3, temperature: 19 }, { online: false });
  const upper = device('native-a:aabbccddeeff_s2_z1', 'Upper zone', { systemID: 2, zoneID: 1, temperature: 22 });
  const other = device('native-b:112233445566_s1_z1', 'Other installation zone', { systemID: 1, zoneID: 1, temperature: 23 });
  const hidden = device('native-a:aabbccddeeff_s1_z4', 'Hidden zone', { systemID: 1, zoneID: 4, temperature: 24 });
  const bridge = device('bridge:climate_airzone', 'Bridge imitation zone', { systemID: 1, temperature: 25 });
  const mine = device('native-a:scoped_control', 'Control for selected zone', { systemID: 1, mode: 'heat' }, { model: 'Airzone system', members: [selected.id] });
  const unrelated = device('native-a:other_control', 'Control for sibling zone', { systemID: 1, mode: 'heat' }, { model: 'Airzone system', members: [sibling.id] });
  const foreign = device('native-b:foreign_control', 'Foreign control', { systemID: 1, mode: 'cool' }, { model: 'Airzone system', members: [selected.id] });
  const gateway = device('native-a:aabbccddeeff_gateway', 'Synthetic gateway', { dhw_available: false, iaq_available: true, driver: '' }, { model: 'Airzone Webserver' });
  const metadata = device('native-a:aabbccddeeff_s1_system', 'Synthetic system metadata', { systemID: 1, version: '1.78' }, { model: 'Airzone Local API' });
  hub.drivers = [driver('native-a'), driver('native-b'), driver('bridge', 'homeassistant')];
  home.config = { hidden: [hidden.id] };
  setDevices([selected, sibling, upper, other, hidden, bridge, mine, unrelated, foreign, gateway, metadata]);
  return { selected, sibling, hidden, bridge, gateway };
}

test('overview counts only native visible zones and keeps equal system numbers in separate installations', () => {
  house();
  const html = page();
  const visibleText = text(html);
  assert.equal(cardLinks(html).length, 4);
  assert.match(visibleText, /4 zones 3 systèmes 3 joignables/);
  assert.doesNotMatch(visibleText, /Hidden zone|Bridge imitation zone|Synthetic gateway|Synthetic system metadata|Control for/);
  const groups = [...html.matchAll(/<section class="system-zones\b[^>]*>([\s\S]*?)<\/section>/g)].map(([, group]) => text(group));
  assert.equal(groups.length, 3);
  assert.match(groups[0], /native-a · Système 1/);
  assert.match(groups[0], /Selected zone/);
  assert.match(groups[0], /Sibling zone/);
  assert.doesNotMatch(groups[0], /Upper zone|Other installation zone/);
  assert.match(groups[1], /native-a · Système 2.*Upper zone/);
  assert.match(groups[2], /native-b · Système 1.*Other installation zone/);
  observationOnly(html, true);
});

test('overview encodes native identities in links and detail resolves the selected zone without leaking other control scopes', () => {
  house();
  assert.ok(cardLinks(page()).some((tag) => tag.includes('href="#/airzone/native-a%3Aaabbccddeeff_s1_z2"')));
  const html = page('airzone/native-a%3Aaabbccddeeff_s1_z2');
  assert.match(text(html), /Selected zone/);
  assert.match(text(html), /Control for selected zone/);
  assert.doesNotMatch(text(html), /Control for sibling zone|Foreign control|Other installation zone|Hidden zone/);
  assert.equal(tags(html, 'button').length, 2, 'two observed main metrics link to history');
  observationOnly(html);
});

test('overview exposes labelled zone links, pressed activity filters and an explicit disconnected state', () => {
  house();
  let html = page();
  const buttons = tags(html, 'button');
  assert.equal(buttons.length, 3);
  assert.equal(buttons.filter((b) => /aria-pressed="true"/.test(b)).length, 1);
  for (const card of cardLinks(html)) {
    const id = card.match(/aria-labelledby="([^"]+)"/)[1];
    assert.ok(html.includes(`id="${id}"`), 'every zone link has a visible accessible name');
  }
  assert.match(text(html), /Toutes 4 En demande 1 Indisponibles 1/);
  hub.connected = false;
  html = page();
  assert.match(text(html), /Connexion à Moli interrompue/);
  assert.match(text(html), /En demande —/);
  assert.match(tags(html, 'button')[1], /disabled/);
  observationOnly(html, true);
});

test('zone detail preserves Fahrenheit for both primary readings and does not manufacture a missing unit', () => {
  const { selected } = house();
  for (const key of ['temperature', 'target_temperature']) selected.points.find((p) => p.key === key).unit = '°F';
  selected.state.temperature.value = 70.5;
  selected.state.target_temperature.value = 72;
  let html = page('airzone/native-a%3Aaabbccddeeff_s1_z2');
  const measurements = [...html.matchAll(/<span class="measurement\b[^>]*>([\s\S]*?)<\/span>/g)].map(([, value]) => text(value));
  assert.deepEqual(measurements, ['70,5 °F', '72 °F']);
  assert.doesNotMatch(html, /°C/);
  for (const key of ['temperature', 'target_temperature']) selected.points.find((p) => p.key === key).unit = null;
  html = page('airzone/native-a%3Aaabbccddeeff_s1_z2');
  assert.doesNotMatch(html, /°F|°C/);
  observationOnly(html);
});

test('history buttons announce the actual primary value and its declared unit to screen readers', () => {
  for (const { unit, temperature, target, labels } of [
    { unit: '°C', temperature: 20.5, target: 21, labels: ['Température mesurée : 20,5 °C. Voir l’historique', 'Consigne : 21 °C. Voir l’historique'] },
    { unit: '°F', temperature: 70.5, target: 72, labels: ['Température mesurée : 70,5 °F. Voir l’historique', 'Consigne : 72 °F. Voir l’historique'] },
    { unit: null, temperature: 20.5, target: 21, labels: ['Température mesurée : 20,5. Voir l’historique', 'Consigne : 21. Voir l’historique'] },
  ]) {
    const { selected } = house();
    for (const key of ['temperature', 'target_temperature']) selected.points.find((p) => p.key === key).unit = unit;
    selected.state.temperature.value = temperature;
    selected.state.target_temperature.value = target;
    const html = page('airzone/native-a%3Aaabbccddeeff_s1_z2');
    assert.deepEqual(tags(html, 'button').map((button) => button.match(/aria-label="([^"]+)"/)?.[1]), labels);
    observationOnly(html);
  }
  const { selected } = house();
  delete selected.state.temperature;
  delete selected.state.target_temperature;
  const html = page('airzone/native-a%3Aaabbccddeeff_s1_z2');
  const buttons = tags(html, 'button');
  assert.equal(buttons.length, 2);
  assert.ok(buttons.every((button) => /\bdisabled\b/.test(button)));
  assert.ok(buttons.every((button) => !/aria-label=/.test(button)), 'absent measurements retain their visible label without inventing an accessible value');
  const measurements = [...html.matchAll(/<span class="measurement\b[^>]*>([\s\S]*?)<\/span>/g)].map(([, value]) => text(value));
  assert.deepEqual(measurements, ['—', '—']);
  observationOnly(html);
});

test('unknown, malformed, hidden, bridge and removed zone links all return a readable not-found view', () => {
  const { selected } = house();
  for (const route of [
    'airzone/%E0%A4%A', 'airzone/unknown%3Azone', 'airzone/',
    'airzone/native-a%3Aaabbccddeeff_s1_z4', 'airzone/bridge%3Aclimate_airzone',
    'airzone/%3Cscript%3E',
  ]) {
    const html = page(route);
    assert.match(text(html), /Cette zone n’est plus disponible/);
    assert.match(html, /href="#\/airzone"/);
    assert.doesNotMatch(html, /<script>|class="measurement\b/);
    observationOnly(html);
  }
  delete hub.devices[selected.id];
  assert.match(text(page('airzone/native-a%3Aaabbccddeeff_s1_z2')), /Cette zone n’est plus disponible/);
});

test('offline cards and details retain their readings but never describe the previous demand as current', () => {
  const { selected } = house();
  selected.online = false;
  const card = render(ZoneCard, { props: { device: selected, href: '#/airzone/native-a%3Aaabbccddeeff_s1_z2' } }).body;
  assert.match(text(card), /Injoignable/);
  assert.match(text(card), /Dernières valeurs connues/);
  assert.match(text(card), /20,5/);
  assert.doesNotMatch(text(card), /Demande en cours/);
  const html = page('airzone/native-a%3Aaabbccddeeff_s1_z2');
  assert.match(text(html), /Cette zone ne répond pas/);
  assert.match(text(html), /Dernières valeurs connues/);
  assert.doesNotMatch(text(html), /Demande en cours/);
  observationOnly(card);
  observationOnly(html);
});

test('missing temperatures and unknown availability cannot become zero or live controls', () => {
  const zone = device('native-a:empty_zone', 'Unmeasured zone', { systemID: 1, zoneID: 1 }, { online: null });
  hub.drivers = [driver('native-a')];
  setDevices([zone]);
  const card = render(ZoneCard, { props: { device: zone, href: '#/airzone/native-a%3Aempty_zone' } }).body;
  assert.match(text(card), /État non communiqué/);
  assert.match(text(card), /Température mesurée — Consigne —/);
  assert.doesNotMatch(card, /°C|°F/);
  const html = page('airzone/native-a%3Aempty_zone');
  assert.match(text(html), /Disponibilité inconnue/);
  assert.equal(tags(html, 'button').length, 2);
  assert.ok(tags(html, 'button').every((button) => /\bdisabled\b/.test(button)), 'missing metrics cannot open invented history');
  observationOnly(html);
});

test('double setpoints remain separate on the zone card and detail without a fabricated single target', () => {
  const zone = device('native-a:dual', 'Dual zone', { systemID: 1, zoneID: 1, temperature: 20, heatsetpoint: 19, coolsetpoint: 24 });
  hub.drivers = [driver('native-a')];
  setDevices([zone]);
  const card = render(ZoneCard, { props: { device: zone, href: '#/airzone/native-a%3Adual' } }).body;
  assert.match(text(card), /19 °C/);
  assert.match(text(card), /24 °C/);
  const html = page('airzone/native-a%3Adual');
  const metrics = [...html.matchAll(/<span class="measurement\b[^>]*>([\s\S]*?)<\/span>/g)].map(([, value]) => text(value));
  assert.deepEqual(metrics, ['20 °C', '19 °C', '24 °C']);
  observationOnly(card);
  observationOnly(html);
});

test('readings show every advertised point once, escape supplied text and distinguish unset from missing', () => {
  const source = device('native-a:readings', 'Readings', {
    temperature: 0, on: false, mode: 'heat', mintemp: 5, thermos_version: '2.0', driver: '',
    future_field: '<script>synthetic</script>', missing_field: null,
  });
  source.points.find((p) => p.key === 'driver').label = '';
  const html = render(Readings, { props: { device: source } }).body;
  const visibleText = text(html);
  for (const point of source.points) {
    const label = point.label || point.key;
    assert.equal(visibleText.split(label).length - 1, 1, `${point.key} is neither omitted nor duplicated`);
  }
  assert.match(visibleText, /Reading temperature 0 °C/);
  assert.match(visibleText, /Reading on Non/);
  assert.match(visibleText, /Reading mode Chaud/);
  assert.match(visibleText, /driver Non renseigné/);
  assert.match(visibleText, /Reading missing_field —/);
  assert.match(html, /&lt;script/);
  assert.doesNotMatch(html, /<script>synthetic/);
  observationOnly(html);
});

test('installation diagnostics expose optional availability without treating unsupported DHW as an HVAC failure', () => {
  const { gateway } = house();
  let html = page('airzone/installation');
  assert.match(text(html), /Synthetic gateway/);
  assert.match(text(html), /Eau chaude sanitaire Non disponible/);
  assert.match(text(html), /Capteurs de qualité d’air dédiés Disponible/);
  assert.match(text(html), /Pilote démarré/);
  assert.match(text(html), /Synthetic system metadata/);
  assert.match(text(html), /Les programmations Airzone Cloud ne sont pas exposées/);
  observationOnly(html);
  gateway.online = false;
  html = page('airzone/installation');
  assert.match(text(html), /Eau chaude sanitaire Disponibilité inconnue/);
  assert.match(text(html), /Capteurs de qualité d’air dédiés Disponibilité inconnue/);
});

test('discovery choices distinguish identical models accessibly and keep existing gateways out of the add flow', () => {
  const candidate = {host:'192.168.77.20',port:3000,mac:'aabbccddeeff',gateway_type:'ws_az',zones:1,existing_instance:null,existing_enabled:null};
  const gateways = [candidate,{...candidate,host:'192.168.77.21',mac:'112233445566'}, {...candidate,host:'192.168.77.22',mac:'aabbcc112233',existing_instance:'paused',existing_enabled:false}];
  const html = render(DiscoveryResults,{props:{report:{gateways,incomplete:false}}}).body;
  const choices = tags(html,'button');
  assert.equal(choices.length,2,'an existing gateway only offers management');
  assert.match(choices[0],/aria-label="Choisir cette passerelle[^\"]+192.168.77.20"/);
  assert.match(choices[1],/aria-label="Choisir cette passerelle[^\"]+192.168.77.21"/);
  assert.match(tags(html,'a')[0],/href="#\/airzone\/installation"[^>]+aria-label="Gérer cette passerelle[^\"]+192.168.77.22"/);
  assert.match(text(html),/1 zone disponible/);
  assert.doesNotMatch(text(html),/1 zones/);
  assert.ok(tags(html,'details').every((tag)=>!tag.includes(' open')),'network identities remain optional details');
});

test('offline shared controls carry their own stale warning even when the selected zone remains online', () => {
  house();
  hub.devices['native-a:scoped_control'].online = false;
  const html = page('airzone/native-a%3Aaabbccddeeff_s1_z2');
  const shared = html.slice(html.indexOf('Control for selected zone'));
  const notices = [...shared.matchAll(/<p\b[^>]*role="status"[^>]*>([\s\S]*?)<\/p>/g)].map(([, value]) => text(value));
  assert.equal(notices.length, 1);
  assert.match(notices[0], /Ressource injoignable.*Dernières valeurs connues.*2023/);
  assert.match(text(shared), /Reading mode Chaud/, 'the stale reported setting remains accessible');
  assert.match(text(html), /Demande en cours/, 'the independent reachable zone keeps its current demand');
  observationOnly(html);
});

test('gateway readings distinguish offline and unknown availability and label retained sample timestamps', () => {
  const { gateway } = house();
  for (const [online, expected] of [[false, /Ressource injoignable/], [null, /Disponibilité inconnue/]]) {
    gateway.online = online;
    const html = page('airzone/installation');
    const notices = [...html.matchAll(/<p\b[^>]*role="status"[^>]*>([\s\S]*?)<\/p>/g)].map(([, value]) => text(value));
    assert.equal(notices.length, 1);
    assert.match(notices[0], expected);
    assert.match(notices[0], /Dernières valeurs connues.*2023/);
    assert.match(text(html), /driver Non renseigné/);
    observationOnly(html);
  }
  for (const sample of Object.values(gateway.state)) delete sample.ts;
  const html = render(Readings, { props: { device: gateway } }).body;
  assert.match(text(html), /Disponibilité inconnue Dernières valeurs connues/);
  assert.doesNotMatch(html, /Invalid Date|1970|2023/);
});

test('loading, empty native inventory and a disconnected hub communicate different states', () => {
  hub.connected = false;
  assert.match(text(page()), /Connexion à vos zones/);
  hub.connected = true;
  hub.drivers = [driver('bridge', 'homeassistant')];
  setDevices([device('bridge:climate', 'Bridge zone', { temperature: 20 })]);
  assert.match(text(page()), /Aucune zone Airzone native visible/);
  assert.equal(cardLinks(page()).length, 0);
  house();
  hub.connected = false;
  assert.match(text(page()), /Connexion à Moli interrompue/);
  assert.equal(cardLinks(page()).length, 4, 'previous observations remain accessible under the disconnected warning');
});

test('the zone card and unset diagnostic values follow the selected English locale', async () => {
  const { selected, gateway } = house();
  await words.setLanguage('en');
  const card = render(ZoneCard, { props: { device: selected, href: '#/airzone/native-a%3Aaabbccddeeff_s1_z2' } }).body;
  assert.match(text(card), /Measured temperature/);
  assert.match(text(card), /20\.5/);
  assert.doesNotMatch(text(card), /Température mesurée|airzone_ui\./);
  const html = render(Readings, { props: { device: gateway } }).body;
  assert.match(text(html), /Not set/);
  assert.doesNotMatch(text(html), /Non renseigné|airzone_ui\./);
});

test('native battery percentages are visible on cards, including zero, while missing levels stay absent', () => {
  const d = device('native:aabbccddeeff_s1_z2', 'Battery zone', { battery: 0, thermos_radio: true });
  let html = render(ZoneCard, { props: { device: d, href: '#/airzone/example' } }).body;
  assert.match(text(html), /Batterie\s+0\s*%/);
  delete d.state.battery;
  html = render(ZoneCard, { props: { device: d, href: '#/airzone/example' } }).body;
  assert.doesNotMatch(text(html), /Batterie\s+0\s*%/);
});

test('device management includes native hidden resources without admitting bridge imitations', () => {
  house();
  const html = page('airzone/devices');
  assert.match(text(html), /Gérer les appareils/);
  assert.match(text(html), /Hidden zone/);
  assert.doesNotMatch(text(html), /Bridge imitation zone/);
  assert.match(text(html), /Afficher dans Moli/);
  assert.doesNotMatch(html, /type="(?:range|number)"|role="(?:switch|slider)"/);
});

test('alerts show an explicit battery warning and retain hidden native faults', () => {
  const { selected, hidden } = house();
  const a = device(selected.id, 'Battery zone', {systemID:1,zoneID:2,battery:14,thermos_radio:true,errors:'[{"zone":"Low battery"}]'});
  const b = device(hidden.id, 'Hidden warning zone', {systemID:1,zoneID:4,errors:'[{"zone":"Error 3"}]'});
  hub.devices[a.id]=a; hub.devices[b.id]=b;
  const html=page('airzone/alerts');
  assert.match(text(html), /Batterie faible/);
  assert.match(text(html), /Hidden warning zone/);
  assert.match(html, /href="#\/airzone\/devices\//);
  assert.doesNotMatch(text(html), /Tout fonctionne|Tous les appareils fonctionnent/);
});

test('add flow distinguishes automatic discovery from physical commissioning and gateway configuration', () => {
  house();
  const html=page('airzone/add');
  assert.match(text(html), /Ajouter une passerelle Airzone/);
  assert.match(text(html), /Rechercher mes passerelles/);
  assert.equal(tags(html,'input').length,0,'the initial wizard does not ask for an IP');
  assert.match(html, /aria-current="step"/);
  assert.match(html, /<details class="help\b[^>]*>/);
  assert.match(text(html), /appairage/);
  assert.match(text(html), /passerelle/);
  assert.match(html, /href="#\/airzone\/devices"/);
  assert.doesNotMatch(html, /role="(?:switch|slider)"|type="range"/);
});

test('wired thermostat detail does not invent a battery level', () => {
  const { selected }=house();
  hub.devices[selected.id]=device(selected.id,'Wired zone',{systemID:1,zoneID:2,thermos_radio:false});
  const html=page('airzone/'+encodeURIComponent(selected.id));
  assert.match(text(html), /Thermostat filaire/);
  assert.doesNotMatch(text(html), /Batterie\s+0\s*%/);
});

test('journal renders visibility changes without treating them as labels', async () => {
  const Journal=(await vite.ssrLoadModule('/src/components/Journal.svelte')).default;
  hub.journal=[{id:1,ts:1700000000000,action:{kind:'visibility',device:'native:example',hidden:true},origin:'ui',outcome:{result:'ok'}}];
  assert.match(text(render(Journal).body), /Masqué dans Moli/);
  hub.journal=[];
});

test('journal identifies a read-only gateway installation without a label payload', async () => {
  const Journal=(await vite.ssrLoadModule('/src/components/Journal.svelte')).default;
  hub.journal=[{id:2,ts:1700000000000,action:{kind:'driver_added',instance:'new-airzone',driver_kind:'airzone',read_only:true},origin:'ui',outcome:{result:'ok'}}];
  const content=text(render(Journal).body);
  assert.match(content,/new-airzone/);
  assert.match(content,/lecture seule/);
  hub.journal=[];
});

test('installation retains gateway management with no live drivers and while disconnected',()=>{
  for (const connected of [true,false]) {
    hub.connected=connected;
    const html=page('airzone/installation');
    assert.match(text(html),/Passerelles Airzone/);
    assert.match(html,/href="#\/airzone\/add"/);
    assert.match(text(html),/Actualiser la liste/);
  }
});

test('system always exposes the gateway management route after every Airzone driver is withdrawn',async()=>{
  const System=(await vite.ssrLoadModule('/src/maison/pages/Systeme.svelte')).default;
  assert.match(render(System).body,/href="#\/airzone\/installation"/);
});

test('gateway cards distinguish pause from offline and show actual writable configuration',async()=>{
  const Card=(await vite.ssrLoadModule('/src/maison/ui/AirzoneGatewayCard.svelte')).default;
  const gateway={instance:'airzone-old',host:'172.31.1.1',port:3000,enabled:false,source:'configuration',read_only:false,running:false};
  let html=render(Card,{props:{gateway}}).body;
  assert.match(text(html),/Suivi en pause/);assert.match(text(html),/Pilotage autorisé/);
  assert.match(text(html),/Vérifier et réactiver/);assert.doesNotMatch(text(html),/Hors ligne/);
  html=render(Card,{props:{gateway:{...gateway,enabled:true,running:true,read_only:true},connected:false}}).body;
  assert.match(text(html),/État à vérifier/);assert.ok(tags(html,'button').every(tag=>/disabled/.test(tag)));
});

test('gateway connection editor retains identity and announces read-only conversion',async()=>{
  const Add=(await vite.ssrLoadModule('/src/maison/ui/AirzoneAdd.svelte')).default;
  const gateway={instance:'airzone-old',host:'172.31.1.1',port:3000,tls_fingerprint:null,mac:'aabbccddeeff',enabled:false,read_only:false};
  const html=render(Add,{props:{gateway}}).body;
  assert.match(text(html),/Réactiver cette passerelle/);
  assert.match(text(html),/Tout enregistrement active la lecture seule/);
  assert.match(html,/<input[^>]*value="airzone-old"[^>]*disabled/);
  assert.doesNotMatch(text(html),/Ajouter une autre passerelle/);
});

test('journal renders gateway pause and reconfiguration without a label payload',async()=>{
  const Journal=(await vite.ssrLoadModule('/src/components/Journal.svelte')).default;
  for (const [enabled,expected] of [[false,/mis en pause/],[true,/reconfiguré en lecture seule/]]) {
    hub.journal=[{id:3,ts:1700000000000,action:{kind:'driver_configured',instance:'airzone-old',driver_kind:'airzone',enabled,read_only:true},origin:'ui',outcome:{result:'ok'}}];
    assert.match(text(render(Journal).body),expected);
  }
});

test('Airzone describes the consultation view without falsely claiming all gateways are read-only',()=>{
  house();
  for(const route of ['airzone','airzone/installation','airzone/native-a%3Aaabbccddeeff_s1_z2']) {
    const html=page(route);
    assert.match(text(html),/Vue de consultation/);
    assert.doesNotMatch(text(html),/Équipements en lecture seule/);
  }
});

test('gateway mode controls have truthful labels and explain where authorized controls are available',async()=>{
  const Card=(await vite.ssrLoadModule('/src/maison/ui/AirzoneGatewayCard.svelte')).default;
  const gateway={instance:'airzone-example',host:'172.31.1.1',port:3000,enabled:true,source:'moli',read_only:true,running:true};
  let html=render(Card,{props:{gateway}}).body;
  assert.match(text(html),/Lecture seule/);assert.match(text(html),/Autoriser le pilotage/);
  assert.doesNotMatch(text(html),/Pilotage autorisé/);
  const action=tags(html,'button').find(tag=>/class="mode-action/.test(tag));
  assert.match(action,/aria-label="[^"]*Autoriser le pilotage[^"]*172.31.1.1/);assert.doesNotMatch(action,/disabled/);
  html=render(Card,{props:{gateway:{...gateway,read_only:false}}}).body;
  assert.match(text(html),/Pilotage autorisé/);assert.match(text(html),/Revenir en lecture seule/);
  assert.match(html,/href="#\/pieces"/);
  html=render(Card,{props:{gateway:{...gateway,enabled:false,running:false}}}).body;
  assert.match(tags(html,'button').find(tag=>/class="mode-action/.test(tag)),/disabled/);
  assert.match(text(html),/Réactivez la connexion avant/);
  html=render(Card,{props:{gateway:{...gateway,enabled:false,running:false,read_only:false}}}).body;
  assert.doesNotMatch(tags(html,'button').find(tag=>/class="mode-action/.test(tag)),/disabled/);
});

test('the code sheet explains permission scope in both directions and both languages',async()=>{
  const Sheet=(await vite.ssrLoadModule('/src/maison/ui/PinSheet.svelte')).default;
  hub.session={pin_configured:true,owner:false,min_pin:6};
  for(const [language,enable,disable] of [
    ['fr',/autorise les commandes vers tous les équipements de cette passerelle/,/confirme le retour en lecture seule/],
    ['en',/allows commands to all equipment on this gateway/,/confirms the return to read-only/],
  ]) {
    await words.setLanguage(language);
    home.held={label:'Synthetic gateway',reason:'airzone_control'};
    assert.match(text(render(Sheet).body),enable);
    home.held={label:'Synthetic gateway',reason:'airzone_read_only'};
    assert.match(text(render(Sheet).body),disable);
  }
  home.held=null;
});

test('gateway cards explain static locks and disable only the prohibited actions',async()=>{
  const Card=(await vite.ssrLoadModule('/src/maison/ui/AirzoneGatewayCard.svelte')).default;
  const gateway={instance:'airzone-example',host:'172.31.1.1',port:3000,enabled:true,source:'configuration',read_only:true,running:true};
  for (const configuration_lock of ['read_only','disabled']) {
    const html=render(Card,{props:{gateway:{...gateway,configuration_lock}}}).body;
    assert.match(text(html),/fichier de configuration Moli/);
    assert.match(tags(html,'button').find(tag=>/class="mode-action/.test(tag)),/disabled/);
    if(configuration_lock==='disabled') assert.match(tags(html,'button').find(tag=>/class="manage/.test(tag)),/disabled/);
  }
});

test('temporary revocation cards warn about restart and expose saving instead of control',async()=>{
  const Card=(await vite.ssrLoadModule('/src/maison/ui/AirzoneGatewayCard.svelte')).default;
  for(const [language,warning,retry,grant] of [
    ['fr',/peut être perdu au redémarrage/,/Réessayer l’enregistrement/,/Autoriser le pilotage/],
    ['en',/may be lost when Moli restarts/,/Retry saving/,/Authorize control/],
  ]) {
    await words.setLanguage(language);
    for(const enabled of [false,true]) {
      const gateway={instance:'airzone-example',host:'172.31.1.1',port:3000,enabled,source:'moli',read_only:true,running:enabled,temporary_revocation:true};
      const html=render(Card,{props:{gateway}}).body;
      assert.match(text(html),warning);assert.match(text(html),retry);assert.doesNotMatch(text(html),grant);
      assert.doesNotMatch(tags(html,'button').find(tag=>/class="mode-action/.test(tag)),/disabled/);
      assert.match(tags(html,'button').find(tag=>/class="manage/.test(tag)),/disabled/);
    }
  }
});
