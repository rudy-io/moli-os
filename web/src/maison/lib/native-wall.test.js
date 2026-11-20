import { test } from 'node:test';
import assert from 'node:assert/strict';
import { wallAvailable, wallAsk, onWallState, hold, sleepKind } from './native-wall.js';

/** A window with the app's bridge: `answer(type, extra)` gives the state it sends back. */
function fakeWindow(answer) {
  const listeners = new Set();
  const sent = [];
  const win = {
    addEventListener: (_, fn) => listeners.add(fn),
    removeEventListener: (_, fn) => listeners.delete(fn),
    dispatch: (detail) => [...listeners].forEach((fn) => fn({ detail })),
    MoliNative: {
      wall: true,
      wallSend: (type, extra) => {
        sent.push({ type, ...extra });
        const state = answer?.(type, extra);
        if (state !== undefined) queueMicrotask(() => win.dispatch(state));
      },
    },
  };
  return { win, sent, listeners };
}

test('only the app with the module can make a wall tablet', () => {
  assert.equal(wallAvailable({}), false);
  assert.equal(wallAvailable({ MoliNative: { platform: 'ios' } }), false);
  assert.equal(wallAvailable({ MoliNative: { wall: false, wallSend() {} } }), false);
  assert.equal(wallAvailable(fakeWindow().win), true);
});

test('a question is answered with the state, and leaves nothing listening', async () => {
  const { win, sent, listeners } = fakeWindow((type, extra) => ({ enabled: extra.options?.enabled ?? false }));
  const state = await wallAsk('configure', { options: { enabled: true } }, win);
  assert.deepEqual(state, { enabled: true });
  assert.deepEqual(sent, [{ type: 'configure', options: { enabled: true } }]);
  assert.equal(listeners.size, 0);
});

test('no app, no answer: null, never a wait', async () => {
  assert.equal(await wallAsk('state', {}, {}), null);
  const { win } = fakeWindow(() => undefined);
  assert.equal(await wallAsk('state', {}, win, 10), null);
});

test('states not asked for still reach the card, until it leaves', () => {
  const { win } = fakeWindow();
  const seen = [];
  const off = onWallState((s) => seen.push(s), win);
  win.dispatch({ admin: true });
  off();
  win.dispatch({ admin: false });
  assert.deepEqual(seen, [{ admin: true }]);
});

test('how the tablet is held and how its screen sleeps', () => {
  assert.equal(hold({ owner: true, pinned: true }), 'owner');
  assert.equal(hold({ owner: false, pinned: true }), 'pinned');
  assert.equal(hold({ owner: false, pinned: false }), 'free');
  assert.equal(hold(null), null);
  assert.equal(sleepKind({ admin: true }), 'off');
  assert.equal(sleepKind({ admin: false }), 'black');
});
