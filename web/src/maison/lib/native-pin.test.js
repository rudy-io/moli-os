import { test } from 'node:test';
import assert from 'node:assert/strict';
import { fingerprintAvailable, pinSaved, unlockPin, rememberPin, forgetPin, offerPin } from './native-pin.js';

/** A window with the app's bridge; `answer` is what the app sends back. */
function phone({ fingerprint = true, saved = true, answer } = {}) {
  const listeners = new Set();
  const calls = [];
  const win = {
    addEventListener: (name, fn) => name === 'moli-native-pin' && listeners.add(fn),
    removeEventListener: (name, fn) => listeners.delete(fn),
    MoliNative: {
      fingerprint,
      pinSaved: saved,
      unlockPin: () => {
        calls.push(['unlock']);
        if (answer !== undefined) queueMicrotask(() => listeners.forEach((fn) => fn({ detail: answer })));
      },
      rememberPin: (pin) => calls.push(['remember', pin]),
      offerPin: (pin) => calls.push(['offer', pin]),
      forgetPin: () => calls.push(['forget']),
    },
  };
  return { win, calls, listeners };
}

test('a browser, or a phone without fingerprint, keeps the keypad', async () => {
  assert.equal(fingerprintAvailable({}), false);
  assert.equal(fingerprintAvailable(undefined), false);
  assert.equal(pinSaved({}), false);
  assert.equal(await unlockPin({}), null);
  const { win, calls } = phone({ fingerprint: false });
  assert.equal(fingerprintAvailable(win), false);
  rememberPin('1234', win);
  forgetPin(win);
  assert.equal(await unlockPin(win), null);
  assert.deepEqual(calls, [], 'nothing is asked of an app that cannot');
});

test('the kept code comes back after the fingerprint', async () => {
  const { win, calls, listeners } = phone({ answer: { pin: '482913' } });
  assert.equal(pinSaved(win), true);
  assert.equal(await unlockPin(win), '482913');
  assert.deepEqual(calls, [['unlock']]);
  assert.equal(listeners.size, 0, 'the listener is removed once answered');
});

test('a cancelled fingerprint, or a strange answer, gives no code', async () => {
  for (const answer of [{ error: 'cancel' }, { pin: '12' }, { pin: 'abcd' }, { pin: 123456 }, {}]) {
    const { win } = phone({ answer });
    assert.equal(await unlockPin(win), null, JSON.stringify(answer));
  }
});

test('no answer from the app ends after the timeout', async () => {
  const { win, listeners } = phone({});
  assert.equal(await unlockPin(win, 5), null);
  assert.equal(listeners.size, 0);
});

test('nothing is asked when no code is kept', async () => {
  const { win, calls } = phone({ saved: false, answer: { pin: '1234' } });
  assert.equal(pinSaved(win), false);
  assert.equal(await unlockPin(win), null);
  assert.deepEqual(calls, []);
});

test('only a code of 4 to 8 digits is kept; forgetting goes to the app', () => {
  const { win, calls } = phone({ saved: false });
  rememberPin('12', win);
  rememberPin('123456789', win);
  rememberPin('12a4', win);
  rememberPin(undefined, win);
  rememberPin('4829', win);
  forgetPin(win);
  assert.deepEqual(calls, [['remember', '4829'], ['forget']]);
});

test('a code typed right is offered to the app once nothing is kept, never twice', () => {
  const fresh = phone({ saved: false });
  offerPin('12', fresh.win);
  offerPin('482913', fresh.win);
  assert.deepEqual(fresh.calls, [['offer', '482913']]);
  const kept = phone({ saved: true });
  offerPin('482913', kept.win);
  assert.deepEqual(kept.calls, [], 'a kept code is not offered again');
  const none = phone({ fingerprint: false, saved: false });
  offerPin('482913', none.win);
  assert.deepEqual(none.calls, []);
});
