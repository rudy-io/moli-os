import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { createServer } from 'vite';
let vite, render, Catalog, Status, Actions, Page, Control, hub, setLanguage;
const storageDescriptor = Object.getOwnPropertyDescriptor(globalThis, 'localStorage');
const documentDescriptor = Object.getOwnPropertyDescriptor(globalThis, 'document');
before(async () => {
  delete globalThis.localStorage;
  globalThis.document = { querySelector: () => null, documentElement: {} };
  vite = await createServer({ root: fileURLToPath(new URL('../../../', import.meta.url)), server: { middlewareMode: true, watch: null }, logLevel: 'error' });
  ({ render } = await vite.ssrLoadModule('svelte/server'));
  Catalog = (await vite.ssrLoadModule('/src/maison/ui/SomfyCloudCatalog.svelte')).default;
  Status = (await vite.ssrLoadModule('/src/maison/ui/SomfyCloudStatus.svelte')).default;
  Actions = (await vite.ssrLoadModule('/src/maison/ui/SomfyCloudActions.svelte')).default;
  Page = (await vite.ssrLoadModule('/src/maison/pages/Somfy.svelte')).default;
  Control = (await vite.ssrLoadModule('/src/maison/ui/SomfyScenarioControl.svelte')).default;
  ({ hub } = await vite.ssrLoadModule('/src/maison/lib/home.svelte.js'));
  ({ setLanguage } = await vite.ssrLoadModule('/src/lib/i18n.svelte.js'));
});
after(async () => {
  await vite?.close();
  delete globalThis.document;
  if (documentDescriptor) Object.defineProperty(globalThis, 'document', documentDescriptor);
  if (storageDescriptor) Object.defineProperty(globalThis, 'localStorage', storageDescriptor);
});
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
  const html = render(Catalog, { props: { observation, initialSection: 'programmes', showDefinition() {} } }).body;
  const summaries = [...html.matchAll(/<summary[^>]*>([\s\S]*?)<\/summary>/g)].map(m => m[1].replace(/<[^>]*>/g, '').trim());
  assert.ok(summaries.some(s => /Disponible · 0$/.test(s)));
  assert.ok(summaries.some(s => /Accès refusé · 1 · historique$/.test(s)));
  assert.ok(summaries.some(s => /Non pris en charge$/.test(s) && !/·|0/.test(s)), 'unavailable data has no count');
});

test('calendar programmes expose days, fixed and solar times and named actions without executable controls', () => {
  const observation = { freshness: { current: true, collections: { calendar_rules: true } }, catalog: {
    target_labels: [{ device_url: 'io://example.invalid/shutter', label: 'Store témoin <script>' }],
    collections: { calendar_rules: { status: 'available', data: { definitions: [{ id: 'demo-rule', raw: { weeklyCalendarRule: {
      dayMask: 21, period: 1, enabled: true,
      startYear: -1, startMonth: -1, startDay: -1, endYear: -1, endMonth: -1, endDay: -1,
      localCalendarDay: { dayTimeTriggers: [
        { startActionGroupTimeTrigger: { triggerType: 'TIME', hours: 7, minutes: 30, enabled: false,
          localActionGroup: { actions: [{ deviceURL: 'io://example.invalid/shutter', commands: [{ name: 'open', parameters: [] }] }] } } },
        { startActionGroupTimeTrigger: { triggerType: 'DUSK', duskOffset: -15,
          localActionGroup: { actions: [{ deviceURL: 'io://example.invalid/shutter', commands: [{ name: 'close', parameters: [] }] }] } } },
      ] },
    } } }] } } },
  } };
  const html = render(Catalog, { props: { observation, initialSection: 'programmes', showDefinition() {} } }).body;
  assert.match(html, /Lundi/);
  assert.match(html, /Mercredi/);
  assert.match(html, /Vendredi/);
  assert.match(html, /07:30/);
  assert.match(html, /coucher du soleil/);
  assert.match(html, /Store témoin &lt;script/);
  assert.match(html, /Ouvrir/);
  assert.match(html, /Fermer/);
  assert.match(html, /désactivé/i);
  assert.doesNotMatch(html, /<script/);
  assert.doesNotMatch(html, /<button[^>]*>\s*(?:Ouvrir|Fermer|Exécuter|Activer|Modifier|Importer)/);
});

test('unsupported and incomplete programmes stay qualified rather than acquiring fabricated days or times', () => {
  const observation = { freshness: { current: false, collections: {} }, catalog: { collections: {
    calendar_rules: { status: 'forbidden', data: { definitions: [{ id: 'future-rule', raw: { futureCalendarRule: { dayMask: 999, hours: 99 } } }] } },
  } } };
  const html = render(Catalog, { props: { observation, initialSection: 'programmes', showDefinition() {} } }).body;
  assert.match(html, /format.*non.*(?:reconnu|pris en charge)/i);
  assert.match(html, /Historique/);
  assert.doesNotMatch(html, /99:00|Tous les jours|sera exécuté/);
});

test('long action lists have a small preview and their translations follow a later language change', async () => {
  const actions = Array.from({ length: 40 }, (_, i) => ({ target: { label: `Device ${i}`, known: true }, commands: [{ kind: 'close', name: 'close' }], totalCommands: 1, truncated: false }));
  try {
    await setLanguage('en');
    const html = render(Actions, { props: { actions, total: 40 } }).body;
    assert.match(html, /Device 0/);
    assert.match(html, /Device 2/);
    assert.doesNotMatch(html, /Device 3/);
    assert.match(html, /Show 37 more actions/);
    assert.match(html, /Close/);
    assert.doesNotMatch(html, /Fermer|<button/);
  } finally { await setLanguage('fr'); }
  const french = render(Actions, { props: { actions, total: 40 } }).body;
  assert.match(french, /Fermer/);
  assert.match(french, /37 autres actions/);
});

test('daily cycles do not claim every day and nested disabled flags remain separately visible', () => {
  const observation = { freshness: { current: true, collections: { calendar_rules: true } }, catalog: { collections: {
    calendar_rules: { status: 'available', data: { definitions: [{ raw: { dailyCalendarRule: {
      period: 2, enabled: true, localCalendarDay: { enabled: false, dayTimeTriggers: [{ startActionGroupTimeTrigger: {
        triggerType: 'TIME', hours: 8, minutes: 0, enabled: true,
        localActionGroup: { enabled: false, actions: [] },
      } }] },
    } } }] } },
  } } };
  const html = render(Catalog, { props: { observation, initialSection: 'programmes', showDefinition() {} } }).body;
  const text = html.replace(/<[^>]*>/g, '');
  assert.match(text, /Tous les 2 jours/);
  assert.doesNotMatch(text, /Tous les jours/);
  assert.match(text, /Journée.*Déclaré désactivé/);
  assert.match(text, /Groupe d’actions.*Déclaré désactivé/);
});


test('TaHoma provides a direct equipment route and explains a cloud-only setup', () => {
  const before = hub.devices;
  try {
    hub.devices = {};
    const html = render(Page).body;
    assert.match(html, /href="#\/pieces\/volets"/);
    assert.match(html, /Aucun volet relié à cette instance/);
    assert.match(html, /Scénarios et programmations/);
    assert.doesNotMatch(html, /<button[^>]*>\s*(?:Lancer|Exécuter|Fermer)/);
  } finally { hub.devices = before; }
});


test('scenario cards offer Run only with current server and live Hub permission', () => {
  const previous = hub.devices;
  const connected = hub.connected;
  hub.connected = true;
  const row = { id: 'demo', permitted: true, executable: true, point: 'somfy:cloud:scenario:fixture/run', value: 'revision-token' };
  const observation = { revision: 'revision', freshness: { current: true, collections: { scenarios: true } }, execution: { read_only: false, scenarios: [row] } };
  const props = { observation, definition: { id: 'demo', label: '<script>demo</script>' }, generation: 'generation', permission() {} };
  try {
    hub.devices = { 'somfy:cloud:scenario:fixture': { online: true, points: [{ key: 'run', kind: { type: 'enum', values: ['revision-token'] }, access: { write: true } }] } };
    const enabled = render(Control, { props }).body;
    assert.match(enabled, /Lancer/);
    assert.match(enabled, /Revenir en lecture seule/);
    observation.execution.read_only = true;
    const readonly = render(Control, { props }).body;
    assert.doesNotMatch(readonly, />Lancer/);
    assert.match(readonly, /configurée en lecture seule/);
    observation.execution.read_only = false;
    observation.execution.scenarios = [{ ...row, permitted: false, executable: false, reason: 'permission_required' }];
    assert.match(render(Control, { props }).body, /Autoriser ce scénario/);
    observation.execution.scenarios = [row];
    hub.devices = {};
    assert.doesNotMatch(render(Control, { props }).body, />Lancer/);
  } finally { hub.devices = previous; hub.connected = connected; }
});


test('uncertain provider execution has a translated explanation and no completion claim', () => {
  const html = render(Status, { props: { row: { online: true, session_phase: 'ready', state: { phase: 'ready', failure: { kind: 'execution_uncertain' } } } } }).body;
  assert.match(html, /Vérifiez TaHoma avant de relancer/);
  assert.doesNotMatch(html, /systeme\.somfy\.failure|terminé|completed/);
});
