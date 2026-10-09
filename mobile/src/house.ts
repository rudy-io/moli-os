// Which house the app opens: chosen in the app (first screen), like Home
// Assistant's server address. A build made for one house may preset it
// (house.json `url`, or MOLI_URL): the welcome screen is then skipped.
//
// The address lives in the keychain with the pairing (see moli.ts), so that
// background reports know where to go even with the app closed.

import Constants from "expo-constants";
import * as SecureStore from "expo-secure-store";

const HOUSE = "moli.house";

/** The address baked into this build, if any (a house's own build). */
export const PRESET = (Constants.expoConfig?.extra?.moliUrl as string | null | undefined) ?? null;

/** Extra domains the dashboard may move to (house.json `allowedSuffixes`). */
const EXTRA_SUFFIXES = (Constants.expoConfig?.extra?.allowedSuffixes as string[] | undefined) ?? [];

/** Cloudflare Access's sign-in pages: a house behind Access sends there first. */
const ACCESS = "cloudflareaccess.com";

export async function savedHouse(): Promise<string | null> {
  try {
    return await SecureStore.getItemAsync(HOUSE);
  } catch {
    return null;
  }
}

export async function saveHouse(base: string): Promise<void> {
  await SecureStore.setItemAsync(HOUSE, base, { keychainAccessible: SecureStore.AFTER_FIRST_UNLOCK });
}

export async function forgetHouse(): Promise<void> {
  await SecureStore.deleteItemAsync(HOUSE).catch(() => {});
}

/** The house in use: the one chosen in the app, else this build's own. */
export async function currentHouse(): Promise<string | null> {
  return (await savedHouse()) ?? PRESET;
}

/** Domains the app keeps inside itself for this house (whole labels only). */
export function allowedSuffixes(base: string): string[] {
  return [new URL(base).hostname, ACCESS, ...EXTRA_SUFFIXES];
}

/** Whether a host is a private address (home network): plain http is fine there. */
export function isLocalHost(host: string): boolean {
  const h = host.replace(/^\[|\]$/g, "").toLowerCase();
  if (h === "localhost" || h.endsWith(".local") || h.endsWith(".lan") || h.endsWith(".home") || !h.includes(".")) {
    return true;
  }
  const ip = h.split(".").map(Number);
  if (ip.length === 4 && ip.every((n) => Number.isInteger(n) && n >= 0 && n <= 255)) {
    const [a, b] = ip;
    return a === 10 || a === 127 || (a === 172 && b >= 16 && b <= 31) || (a === 192 && b === 168) || (a === 100 && b >= 64 && b <= 127);
  }
  return h.startsWith("fd") || h.startsWith("fe80") || h === "::1";
}

/**
 * What the person typed, made an address: `maison.example.org` →
 * `https://maison.example.org`, `192.168.1.x:8790` → `http://…` (a home
 * address has no certificate). Path, query and trailing slash are dropped:
 * the app always opens the dashboard's root.
 */
export function normalize(input: string): string | null {
  let text = input.trim();
  if (!text) return null;
  if (!/^[a-z][a-z0-9+.-]*:\/\//i.test(text)) {
    const host = text.split(/[/:?#]/)[0];
    text = `${isLocalHost(host) ? "http" : "https"}://${text}`;
  }
  try {
    const url = new URL(text);
    if (url.protocol !== "https:" && url.protocol !== "http:") return null;
    if (!url.hostname) return null;
    return `${url.protocol}//${url.host}`;
  } catch {
    return null;
  }
}

export type Probe =
  /** A dashboard answered (home network). */
  | { ok: true; via: "direct" }
  /** Cloudflare Access asks to sign in first: the web view will show it. */
  | { ok: true; via: "access" }
  /** A Moli answered, but only lets in its home network or Access. */
  | { ok: false; why: "refused"; message?: string }
  | { ok: false; why: "unreachable" | "not-moli" | "insecure" };

/** Knocks at the address before keeping it: is there a Moli, and can we get in? */
export async function probe(base: string): Promise<Probe> {
  const url = new URL(base);
  if (url.protocol === "http:" && !isLocalHost(url.hostname)) return { ok: false, why: "insecure" };
  const abort = new AbortController();
  const timer = setTimeout(() => abort.abort(), 10_000);
  try {
    const res = await fetch(`${base}/`, { headers: { accept: "text/html" }, signal: abort.signal });
    const landed = (() => {
      try {
        return new URL(res.url || base).hostname;
      } catch {
        return url.hostname;
      }
    })();
    if (landed === ACCESS || landed.endsWith(`.${ACCESS}`)) return { ok: true, via: "access" };
    const text = await res.text().catch(() => "");
    if (res.status === 403) {
      // Moli refuses strangers with a JSON error (crates/moli-api caller.rs).
      try {
        const body = JSON.parse(text) as { error?: string };
        if (typeof body.error === "string") return { ok: false, why: "refused", message: body.error };
      } catch {
        // Not Moli's refusal.
      }
      return { ok: false, why: "not-moli" };
    }
    if (res.ok && /moli/i.test(text)) return { ok: true, via: "direct" };
    return { ok: false, why: "not-moli" };
  } catch {
    return { ok: false, why: "unreachable" };
  } finally {
    clearTimeout(timer);
  }
}
