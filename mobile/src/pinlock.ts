// The house code behind the fingerprint (or the face). The dashboard asks for
// it instead of the keypad (web/src/maison/lib/native-pin.js) and sends it as
// if typed: Moli itself sees nothing new.
//
// The code sits in the phone's secure storage with `requireAuthentication`:
// the Android keystore (or the iOS keychain) gives it back only after the
// fingerprint, and forgets it by itself when the fingerprints change (a new
// finger enrolled reads `null`). One code per house (its host). A plain flag
// next to it says a code is kept, so the dashboard knows without a prompt.

import * as LocalAuthentication from "expo-local-authentication";
import * as SecureStore from "expo-secure-store";
import { t } from "./texts";

const PIN_SHAPE = /^[0-9]{4,8}$/;

const keyOf = (base: string) => {
  let host = base;
  try {
    host = new URL(base).host;
  } catch {
    // keep it as typed
  }
  return "pin." + host.replace(/[^A-Za-z0-9.-]/g, "_");
};
const flagOf = (base: string) => keyOf(base) + ".kept";
const declinedOf = (base: string) => keyOf(base) + ".declined";

const locked: SecureStore.SecureStoreOptions = {
  requireAuthentication: true,
  keychainAccessible: SecureStore.WHEN_PASSCODE_SET_THIS_DEVICE_ONLY,
};

/** A fingerprint or a face is set up on this phone. */
export async function fingerprintReady(): Promise<boolean> {
  try {
    return (await LocalAuthentication.hasHardwareAsync()) && (await LocalAuthentication.isEnrolledAsync());
  } catch {
    return false;
  }
}

/** A code is kept for this house. */
export async function pinKept(base: string): Promise<boolean> {
  return (await SecureStore.getItemAsync(flagOf(base)).catch(() => null)) === "1";
}

/** The person said « no thanks » once: never offered again for this house. */
export async function offerDeclined(base: string): Promise<boolean> {
  return (await SecureStore.getItemAsync(declinedOf(base)).catch(() => null)) === "1";
}

export async function declineOffer(base: string): Promise<void> {
  await SecureStore.setItemAsync(declinedOf(base), "1").catch(() => {});
}

/** Keeps the code behind the fingerprint (the phone asks for it once). */
export async function keepPin(base: string, pin: string): Promise<boolean> {
  if (!PIN_SHAPE.test(pin) || !(await fingerprintReady())) return false;
  try {
    await SecureStore.setItemAsync(keyOf(base), pin, { ...locked, authenticationPrompt: t.fingerprintKeep });
    await SecureStore.setItemAsync(flagOf(base), "1");
    return true;
  } catch {
    return false;
  }
}

/** The kept code after the fingerprint; `cancel` if the person said no,
 *  `unavailable` if it is gone (fingerprints changed): then forgotten. */
export async function unlockPin(base: string): Promise<{ pin: string } | { error: "cancel" | "unavailable" }> {
  let pin: string | null;
  try {
    pin = await SecureStore.getItemAsync(keyOf(base), { ...locked, authenticationPrompt: t.fingerprintUnlock });
  } catch {
    return { error: "cancel" };
  }
  if (pin && PIN_SHAPE.test(pin)) return { pin };
  await forgetPin(base);
  return { error: "unavailable" };
}

/** Forgets the kept code (refused by the house, or another house chosen). */
export async function forgetPin(base: string): Promise<void> {
  await SecureStore.deleteItemAsync(keyOf(base), locked).catch(() => {});
  await SecureStore.deleteItemAsync(flagOf(base)).catch(() => {});
}
