import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { createServer } from 'vite';
let vite, render, Catalog;
before(async () => {
  vite = await createServer({ root: fileURLToPath(new URL('../../../', import.meta.url)), server: { middlewareMode: true, watch: null }, logLevel: 'error' });
  ({ render } = await vite.ssrLoadModule('svelte/server'));
  Catalog = (await vite.ssrLoadModule('/src/maison/ui/SomfyCloudCatalog.svelte')).default;
});
after(async () => { await vite?.close(); });
test('private definitions render escaped exact JSON, qualifications and no execution controls', () => {
  const raw = '{"n":18446744073709551615,"label":"<img src=x onerror=evil()>"}';
  const html = render(Catalog, { props: {
    observation: { freshness: { current: false, collections: { scenarios: false } }, catalog: { collections: { scenarios: { status: 'forbidden', last_observation_ms: 100, last_attempt_ms: 200, data: { definitions: [{ label: '<img src=x>', id: 'duplicate', warnings: ['not_evaluated'] }] } } }, provider_timezone: 'Bogus/Zone' } },
    details: { 'scenarios:0': raw }, busy: '', showDefinition() {},
  } }).body;
  assert.match(html, /18446744073709551615/);
  assert.match(html, /&lt;img/);
  assert.doesNotMatch(html, /<img/);
  assert.match(html, /Accès refusé/);
  assert.match(html, /Historique/);
  assert.doesNotMatch(html, />Exécuter|>Activer|>Importer|>Modifier/);
});
