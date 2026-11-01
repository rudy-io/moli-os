import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { compile } from 'svelte/compiler';
import { render } from 'svelte/server';

const source = readFileSync(new URL('../components/DriverWarnings.svelte', import.meta.url), 'utf8');
const { js } = compile(source, { generate: 'server', filename: 'DriverWarnings.svelte' });
const code = js.code.replace(/from (['"])(svelte[^'"]+)\1/g,
  (_, quote, path) => `from ${quote}${import.meta.resolve(path)}${quote}`);
const { default: DriverWarnings } = await import(`data:text/javascript;base64,${Buffer.from(code).toString('base64')}`);

test('a running integration shows its warning in the shared status view', () => {
  const warning = 'HTTP sends the token unencrypted; use HTTPS.';
  const { body } = render(DriverWarnings, { props: { drivers: [
    { instance: 'bridge', status: { state: 'running' }, warning },
    { instance: 'other', status: { state: 'running' } },
  ] } });
  assert.ok(body.includes(warning), body);
  assert.match(body, /role="status"/);
  assert.match(body, /bridge/);
  assert.doesNotMatch(body, /other/);
});

test('cleared warnings leave no stale text and warning content stays escaped', () => {
  const unsafe = '<img src=x onerror=alert(1)>';
  const shown = render(DriverWarnings, { props: { drivers: [{ instance: 'bridge', warning: unsafe }] } }).body;
  assert.ok(shown.includes('&lt;img'), shown);
  assert.ok(!shown.includes('<img'), shown);
  const cleared = render(DriverWarnings, { props: { drivers: [{ instance: 'bridge', warning: null }] } }).body;
  assert.doesNotMatch(cleared, /role="status"|bridge|img/);
});
