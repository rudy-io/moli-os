import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { createServer } from 'vite';

// The code sheet, rendered: in the Moli app with a fingerprint, the kept code
// is one button away; without a kept code, the keypad alone (the app offers
// to keep it itself, after a code typed right); in a browser, as before.
let vite;
let render;
let hub;
let home;
let Sheet;
const storage = Object.getOwnPropertyDescriptor(globalThis, 'localStorage');
const document = Object.getOwnPropertyDescriptor(globalThis, 'document');
const win = Object.getOwnPropertyDescriptor(globalThis, 'window');

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
  Sheet = (await vite.ssrLoadModule('/src/maison/ui/PinSheet.svelte')).default;
  hub.session = { pin_configured: true, owner: false, min_pin: 6 };
  home.held = { label: 'Désarmer', reason: 'Cette action demande le code à chaque fois' };
});

after(async () => {
  home.held = null;
  await vite?.close();
  for (const [name, value] of [['localStorage', storage], ['document', document], ['window', win]]) {
    if (value) Object.defineProperty(globalThis, name, value);
    else delete globalThis[name];
  }
});

function app(bridge) {
  globalThis.window = {
    addEventListener() {},
    removeEventListener() {},
    MoliNative: bridge && { unlockPin() {}, rememberPin() {}, forgetPin() {}, ...bridge },
  };
}

const text = (html) => html.replace(/<[^>]+>/g, ' ').replace(/\s+/g, ' ');

test('a kept code: the fingerprint button', () => {
  app({ fingerprint: true, pinSaved: true });
  const html = text(render(Sheet).body);
  assert.match(html, /Avec l’empreinte/);
  assert.doesNotMatch(html, /type="checkbox"/);
});

test('no kept code yet: the keypad alone, no box (the app offers itself)', () => {
  app({ fingerprint: true, pinSaved: false });
  const body = render(Sheet).body;
  assert.doesNotMatch(text(body), /empreinte/i);
  assert.doesNotMatch(body, /type="checkbox"/);
});

test('a browser, or a phone without fingerprint: the keypad alone', () => {
  for (const bridge of [null, { fingerprint: false, pinSaved: true }]) {
    app(bridge);
    const html = text(render(Sheet).body);
    assert.doesNotMatch(html, /empreinte/i, JSON.stringify(bridge));
  }
});
