import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readMenu, readSaved, menuSettings, orderTabs, moveTab } from './menu.js';

const tabs = ['', 'moli', 'salon', 'pieces', 'plan'].map((path) => ({ path }));
const paths = (list) => list.map((t) => t.path);

test('unknown words are dropped, the order is distinct paths', () => {
  assert.deepEqual(readMenu({ position: 'diagonal', shape: 'compact', order: ['plan', 'plan', 3, 'salon'] }), { shape: 'compact', order: ['plan', 'salon'] });
  assert.deepEqual(readMenu(null), {});
  assert.deepEqual(readMenu(['left']), {});
  assert.deepEqual(readSaved('{oops'), {});
  assert.deepEqual(readSaved(null), {});
});

test("the screen's choice, else the house's, else the classic rail on the left", () => {
  assert.deepEqual(menuSettings({}, undefined), { position: 'left', shape: 'classic', order: [] });
  assert.deepEqual(menuSettings({}, { position: 'bottom', shape: 'floating' }), { position: 'bottom', shape: 'floating', order: [] });
  assert.deepEqual(menuSettings({ position: 'right' }, { position: 'bottom', shape: 'floating' }), { position: 'right', shape: 'floating', order: [] });
});

test('Accueil first, then the chosen order, then the others in their place', () => {
  assert.deepEqual(paths(orderTabs(tabs, ['plan', 'salon'])), ['', 'plan', 'salon', 'moli', 'pieces']);
  assert.deepEqual(paths(orderTabs(tabs, ['', 'pieces'])), ['', 'pieces', 'moli', 'salon', 'plan']);
  assert.deepEqual(paths(orderTabs(tabs, [])), paths(tabs));
  // A path the house no longer has changes nothing.
  assert.deepEqual(paths(orderTabs(tabs, ['cameras', 'plan'])), ['', 'plan', 'moli', 'salon', 'pieces']);
});

test('moving a tab swaps it with its neighbour, never above Accueil', () => {
  assert.deepEqual(moveTab(tabs, 'salon', -1), ['salon', 'moli', 'pieces', 'plan']);
  assert.deepEqual(moveTab(tabs, 'salon', 1), ['moli', 'pieces', 'salon', 'plan']);
  assert.deepEqual(moveTab(tabs, 'moli', -1), ['moli', 'salon', 'pieces', 'plan']);
  assert.deepEqual(moveTab(tabs, 'plan', 1), ['moli', 'salon', 'pieces', 'plan']);
  assert.deepEqual(moveTab(tabs, '', 1), ['moli', 'salon', 'pieces', 'plan']);
});

