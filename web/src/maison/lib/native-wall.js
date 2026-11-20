// The wall tablet, in the Moli app on Android (mobile/src/wall.ts): a tablet
// that shows Moli alone, its screen woken by someone passing in front of the
// camera and asleep when nobody is there. In a browser, or on a phone app
// without it (no `window.MoliNative.wall`), nothing here applies.
//
// The page asks with `window.MoliNative.wallSend(type, extra)`:
//   state                         — tell me how things are
//   configure { options }         — { enabled, camera, sensitivity, idle }
//   admin                         — Android's screen to let Moli switch the screen off
//   pin { on }                    — Android's screen pinning (Moli in front)
//   release                       — upkeep: Moli leaves the front until it opens again
// and the app answers each with a `moli-native-wall` event on `window`,
// `detail` being the state (lib: `WallState` in mobile/modules/moli-wall),
// or null when this device cannot be a wall tablet.

const EVENT = 'moli-native-wall';

/** The app can make this device a wall tablet. */
export function wallAvailable(win = globalThis.window) {
  const n = win?.MoliNative;
  return !!(n?.wall && typeof n.wallSend === 'function');
}

/** Sends `type` (with `extra`) and resolves with the state the app answers,
 *  or null (not possible here, no answer within `timeout` ms). */
export function wallAsk(type, extra = {}, win = globalThis.window, timeout = 15_000) {
  if (!wallAvailable(win)) return Promise.resolve(null);
  return new Promise((resolve) => {
    let timer;
    const done = (state) => {
      clearTimeout(timer);
      win.removeEventListener(EVENT, on);
      resolve(state ?? null);
    };
    const on = (e) => done(e.detail);
    win.addEventListener(EVENT, on);
    timer = setTimeout(() => done(null), timeout);
    win.MoliNative.wallSend(type, extra);
  });
}

/** Sends `type` without waiting (Android's own screens: the answer comes
 *  when the person is back, through `onWallState`). */
export function wallSend(type, extra = {}, win = globalThis.window) {
  if (wallAvailable(win)) win.MoliNative.wallSend(type, extra);
}

/** Every answer of the app, as it comes (also those not asked for: back
 *  from Android's screens, the state may have changed). Returns the unsubscribe. */
export function onWallState(listener, win = globalThis.window) {
  if (!win?.addEventListener) return () => {};
  const on = (e) => listener(e.detail ?? null);
  win.addEventListener(EVENT, on);
  return () => win.removeEventListener(EVENT, on);
}

/** How the tablet stays in front, from the best: run by Moli (installed by
 *  QR code), pinned, or not held (anyone may leave Moli). */
export function hold(state) {
  if (!state) return null;
  if (state.owner) return 'owner';
  if (state.pinned) return 'pinned';
  return 'free';
}

/** How the screen sleeps: switched off (administrator), or covered in black. */
export function sleepKind(state) {
  if (!state) return null;
  return state.admin ? 'off' : 'black';
}

/** The sleep delays offered, in minutes (0: never). */
export const IDLE_CHOICES = [1, 2, 5, 10, 30, 0];
