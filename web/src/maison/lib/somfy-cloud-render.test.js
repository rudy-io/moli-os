import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { createServer } from 'vite';
let vite, render, Catalog, Status;
before(async () => {
  vite = await createServer({ root: fileURLToPath(new URL('../../../', import.meta.url)), server: { middlewareMode: true, watch: null }, logLevel: 'error' });
  ({ render } = await vite.ssrLoadModule('svelte/server'));
  Catalog = (await vite.ssrLoadModule('/src/maison/ui/SomfyCloudCatalog.svelte')).default;
  Status = (await vite.ssrLoadModule('/src/maison/ui/SomfyCloudStatus.svelte')).default;
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


test('historical Ready never renders an established session without authoritative live confirmation', () => {
  for (const [session_phase, online, expected] of [
    ['ready', false, 'Disponibilité non confirmée'],
    ['refreshing', false, 'Connexion à vérifier'],
    ['login_required', false, 'Connexion à renouveler'],
    [null, false, 'Disponibilité non confirmée'],
  ]) {
    const html = render(Status, { props: { row: { state: { phase: 'ready' }, session_phase, online } } }).body;
    assert.match(html, new RegExp(expected));
    assert.doesNotMatch(html, /Session établie/);
  }
  assert.match(render(Status, { props: { row: { state: { phase: 'ready' }, session_phase: 'ready', online: true } } }).body, /Session établie/);
});
test('connection qualification retains quota failures and retry information without demanding login', () => {
  const html = render(Status, { props: { row: { state: { phase: 'unavailable', failure: { kind: 'rate_limited' }, retry_after_secs: 18446744073709551615 }, session_phase: 'ready', online: false } } }).body;
  assert.match(html, /Service indisponible/);
  assert.match(html, /Quota atteint/);
  assert.match(html, /Délai de reprise indéterminé/);
  assert.doesNotMatch(html, /Connexion à renouveler|Session établie/);
});
test('collection counts distinguish no observation, observed empty and qualified retained history', () => {
  const observation = { freshness: { current: false, collections: { calendar_rules: true } }, catalog: { collections: {
    calendar_days: { status: 'unsupported', data: null },
    calendar_rules: { status: 'available', data: { definitions: [] } },
    triggers: { status: 'forbidden', data: { definitions: [{ id: 'old' }] }, last_observation_ms: 100, last_attempt_ms: 200 },
  } } };
  const html = render(Catalog, { props: { observation, showDefinition() {} } }).body;
  const summaries = [...html.matchAll(/<summary[^>]*>([\s\S]*?)<\/summary>/g)].map(m => m[1].replace(/<[^>]*>/g, '').trim());
  assert.doesNotMatch(summaries[0], /·|0/, 'missing collection is not an empty inventory');
  assert.doesNotMatch(summaries[1], /·|0/, 'unavailable data has no count');
  assert.match(summaries[2], /Disponible · 0$/);
  assert.match(summaries[3], /Accès refusé · 1 · historique$/);
});
