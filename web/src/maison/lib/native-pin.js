// The house code behind a fingerprint, in the Moli app. The app keeps the code
// in the phone's secure storage, locked by the fingerprint (or the face); the
// dashboard asks for it instead of the keypad and still sends the code: the
// server sees nothing new. In a browser (no `window.MoliNative.fingerprint`),
// nothing here applies and the keypad stays as it is.
//
// The app announces what it can before the page loads:
//   window.MoliNative.fingerprint = true   — a fingerprint or face is set up
//   window.MoliNative.pinSaved = true      — a code is kept for this house
// After a code typed right, the dashboard hands it to the app (`offerPin`):
// the app offers, once, to keep it behind the fingerprint (its own dialog,
// which outlives the code sheet; « no thanks » is never asked again).
// The app answers each unlock request with a `moli-native-pin` event on `window`:
//   { detail: { pin } }                    — unlocked
//   { detail: { error: 'cancel' | 'unavailable' } }

const EVENT = 'moli-native-pin';

function native(win) {
  return win?.MoliNative ?? null;
}

/** The app can keep the code behind a fingerprint. */
export function fingerprintAvailable(win = globalThis.window) {
  const n = native(win);
  return !!(n?.fingerprint && typeof n.unlockPin === 'function' && typeof n.rememberPin === 'function');
}

/** A code is kept for this house, behind the fingerprint. */
export function pinSaved(win = globalThis.window) {
  return fingerprintAvailable(win) && !!native(win).pinSaved;
}

/** Asks the app for the kept code: the fingerprint prompt, then the code, or
 *  `null` (cancelled, changed fingerprints, no answer within `timeout` ms). */
export function unlockPin(win = globalThis.window, timeout = 60_000) {
  if (!pinSaved(win)) return Promise.resolve(null);
  return new Promise((resolve) => {
    let timer;
    const done = (pin) => {
      clearTimeout(timer);
      win.removeEventListener(EVENT, onAnswer);
      resolve(typeof pin === 'string' && /^[0-9]{4,8}$/.test(pin) ? pin : null);
    };
    const onAnswer = (e) => done(e?.detail?.pin);
    win.addEventListener(EVENT, onAnswer);
    timer = setTimeout(() => done(null), timeout);
    native(win).unlockPin();
  });
}

/** A code just typed right: the app may offer, once, to keep it behind the
 *  fingerprint (nothing if already kept, or once declined). */
export function offerPin(pin, win = globalThis.window) {
  if (fingerprintAvailable(win) && !pinSaved(win) && /^[0-9]{4,8}$/.test(pin ?? '')) native(win).offerPin?.(pin);
}

/** Keeps `pin` behind the fingerprint (the app asks for it once, to lock it). */
export function rememberPin(pin, win = globalThis.window) {
  if (fingerprintAvailable(win) && /^[0-9]{4,8}$/.test(pin ?? '')) native(win).rememberPin(pin);
}

/** Forgets the kept code (refused by the house: changed since). */
export function forgetPin(win = globalThis.window) {
  if (fingerprintAvailable(win)) native(win).forgetPin?.();
}
