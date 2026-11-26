import { test } from 'node:test';
import assert from 'node:assert/strict';
import { WindMap } from './windmap.js';

/** A page's animation frames, by hand: asked for, taken back, run. */
function frames() {
  let next = 1;
  const waiting = new Map();
  globalThis.requestAnimationFrame = (fn) => (waiting.set(next, fn), next++);
  globalThis.cancelAnimationFrame = (id) => waiting.delete(id);
  return {
    waiting,
    run() {
      const now = [...waiting.values()];
      waiting.clear();
      now.forEach((fn) => fn(0));
    },
  };
}

test('a hidden page then shown again keeps one loop, not one more each time', () => {
  const page = frames();
  const wm = new WindMap({}, {}, null);
  wm.start();
  page.run();
  // The screen sleeps and wakes: no frame runs in between.
  for (let i = 0; i < 5; i++) {
    wm.stop();
    wm.start();
  }
  assert.equal(page.waiting.size, 1);
  page.run();
  page.run();
  assert.equal(page.waiting.size, 1);
});

test('stopped, nothing is left waiting', () => {
  const page = frames();
  const wm = new WindMap({}, {}, null);
  wm.start();
  wm.stop();
  assert.equal(page.waiting.size, 0);
  page.run();
  assert.equal(page.waiting.size, 0);
});
