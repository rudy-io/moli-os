import { test, before, beforeEach, after } from 'node:test';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { createServer } from 'vite';

let vite, render, Detail, words;
const storage = Object.getOwnPropertyDescriptor(globalThis, 'localStorage');
const document = Object.getOwnPropertyDescriptor(globalThis, 'document');

before(async () => {
  delete globalThis.localStorage;
  globalThis.document = { querySelector: () => null, documentElement: {} };
  vite = await createServer({
    root: fileURLToPath(new URL('../../../', import.meta.url)),
    server: { middlewareMode: true, watch: null, hmr: false }, logLevel: 'error',
  });
  ({ render } = await vite.ssrLoadModule('svelte/server'));
  words = await vite.ssrLoadModule('/src/lib/i18n.svelte.js');
  Detail = (await vite.ssrLoadModule('/src/maison/ui/AirzoneZoneDetail.svelte')).default;
});

beforeEach(async () => { await words.setLanguage('fr'); });
after(async () => {
  await vite?.close();
  delete globalThis.document;
  if (document) Object.defineProperty(globalThis, 'document', document);
  if (storage) Object.defineProperty(globalThis, 'localStorage', storage);
});

function zone(values = {}, extra = {}) {
  return {
    id: 'native:synthetic-zone', native_name: 'Synthetic zone', online: true, label: {},
    points: Object.entries(values).map(([key, value]) => ({
      key, label: `Reported ${key}`, semantic: 'other',
      kind: { type: typeof value === 'number' ? 'numeric' : typeof value === 'boolean' ? 'binary' : 'text' },
      unit: ['temperature', 'target_temperature', 'heatsetpoint', 'coolsetpoint'].includes(key) ? '°C'
        : ['humidity', 'battery', 'coverage'].includes(key) ? '%' : null,
      access: { read: true, write: true },
    })),
    state: Object.fromEntries(Object.entries(values).map(([key, value]) => [key, { value, ts: 1_700_000_000_000 }])),
    ...extra,
  };
}
const htmlOf = (device, controls = []) => render(Detail, { props: { zone: device, controls, onhistory: () => {} } }).body;
const text = (html) => html.replace(/<!--[\s\S]*?-->/g, '').replace(/<[^>]*>/g, ' ').replace(/\s+/g, ' ').trim();
const buttons = (html) => [...html.matchAll(/<button\b[^>]*>/g)].map(([tag]) => tag);

test('read-only detail makes measured and target history discoverable even for writable fixtures', () => {
  const html = htmlOf(zone({ temperature: 20.5, target_temperature: 21, humidity: 0, on: true }));
  assert.doesNotMatch(html, /role="(?:switch|slider)"|<input|<select|<form/);
  assert.equal(buttons(html).length, 2);
  for (const button of buttons(html)) {
    assert.match(button, /class="metric\b/);
    assert.match(button, /aria-label="[^"]+Voir l’historique"/);
  }
  assert.match(text(html), /Température mesurée 20,5 °C/);
  assert.match(text(html), /Consigne 21 °C/);
  assert.equal(text(html).split('Voir l’historique').length - 1, 2, 'history is visible without requiring hover');
  assert.match(text(html), /Humidité 0 %/);
  assert.match(html, /href="#\/airzone\/devices\/native%3Asynthetic-zone"/);
});

test('dual setpoints have their own history targets without inventing a single setpoint', () => {
  const html = htmlOf(zone({ temperature: 20, heatsetpoint: 19, coolsetpoint: 24 }));
  assert.equal(buttons(html).length, 3);
  assert.deepEqual(buttons(html).map((button) => button.match(/aria-label="([^"]+)"/)?.[1]), [
    'Température mesurée : 20 °C. Voir l’historique',
    'Consigne de chauffage : 19 °C. Voir l’historique',
    'Consigne de refroidissement : 24 °C. Voir l’historique',
  ]);
});

test('battery and radio information precede technical disclosure and retain zero levels', () => {
  const html = htmlOf(zone({ battery: 0, coverage: 100, thermos_radio: true, future_parameter: '<script>fixture</script>' }));
  const content = text(html);
  assert.match(content, /Batterie 0 %/);
  assert.match(content, /Couverture radio 100 %/);
  assert.ok(content.indexOf('Batterie') < content.indexOf('Paramètres et informations'));
  assert.match(content, /Reported future_parameter &lt;script>fixture&lt;\/script>/);
  assert.doesNotMatch(html, /<script>/);
  assert.match(html, /<details class="technical\b[^>]*>/);
  assert.doesNotMatch(html.match(/<details class="technical\b[^>]*>/)?.[0] ?? '', /\bopen\b/);

  const invalid = zone({ battery: 120, coverage: -1, thermos_radio: true });
  const invalidHtml = htmlOf(invalid);
  const health = invalidHtml.match(/<section class="health-card\b[\s\S]*?<\/section>/)?.[0] ?? '';
  assert.match(text(health), /Niveau de batterie non communiqué/);
  assert.match(text(health), /Couverture radio Non communiqué/);
  assert.doesNotMatch(text(health), /120|−?1 %/);
});

test('offline demand remains historical and shared settings remain accessible without editors', () => {
  const html = htmlOf(zone({ temperature: 16, target_temperature: 23, heat_demand: true }, { online: false }), [
    zone({ systemID: 1, future_shared_value: 'Synthetic setting' }, { id: 'native:synthetic-system', native_name: 'Synthetic shared settings' }),
  ]);
  assert.match(text(html), /Cette zone ne répond pas/);
  assert.match(text(html), /Dernières valeurs connues/);
  assert.doesNotMatch(text(html), /Demande en cours/);
  assert.match(text(html), /Synthetic shared settings/);
  assert.match(text(html), /Reported future_shared_value Synthetic setting/);
  assert.doesNotMatch(html, /<input|<select|<form|role="(?:switch|slider)"/);
});

test('missing samples stay unavailable and all new words are available in English', async () => {
  const missing = htmlOf(zone({}, { online: null }));
  assert.equal(buttons(missing).length, 2);
  assert.ok(buttons(missing).every((button) => /\bdisabled\b/.test(button) && !/aria-label=/.test(button)));
  assert.match(text(missing), /Disponibilité inconnue/);
  assert.doesNotMatch(missing, /°C|°F/);
  await words.setLanguage('en');
  const english = htmlOf(zone({ temperature: 20, heat_demand: false, thermos_radio: false }));
  assert.doesNotMatch(text(english), /airzone_detail\.|Voir l’historique|Nom, pièce|Chauffage/);
  assert.match(text(english), /View history/);
});
