import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { createServer } from 'vite';

// The code sheet, rendered: in the Moli app with a fingerprint, the kept code
// is one button away; without a kept code, a box offers to keep it; in a
// browser, the keypad alone, as before.
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

test('a kept code: the fingerprint button, no box', () => {
  app({ fingerprint: true, pinSaved: true });
  const html = text(render(Sheet).body);
  assert.match(html, /Avec l’empreinte/);
  assert.doesNotMatch(html, /Retenir le code/);
});

test('no kept code yet: the box to keep it, ticked', () => {
  app({ fingerprint: true, pinSaved: false });
  const body = render(Sheet).body;
  assert.match(text(body), /Retenir le code avec l’empreinte/);
  assert.match(body, /type="checkbox"[^>]*checked/);
  assert.doesNotMatch(text(body), /Avec l’empreinte/);
});

test('a browser, or a phone without fingerprint: the keypad alone', () => {
  for (const bridge of [null, { fingerprint: false, pinSaved: true }]) {
    app(bridge);
    const html = text(render(Sheet).body);
    assert.doesNotMatch(html, /empreinte/i, JSON.stringify(bridge));
  }
});
