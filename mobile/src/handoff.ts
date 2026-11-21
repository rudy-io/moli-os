// Signing in with Google behind Cloudflare Access. Google refuses to sign
// in inside an app's web view (it sent the person to Chrome, and the app
// stayed black). So the sign-in runs in a browser tab of the app's own
// (Android's Custom Tab), and its session is brought back:
// 1. the tab opens the house's `/api/mobile/handoff`, behind Access: the
//    person signs in with Google there, as in any browser;
// 2. Moli keeps the Access token two minutes under a one-time code and sends
//    the tab back to the app (`moli://handoff?code=…`);
// 3. the app trades the code, with the secret its challenge was made of,
//    for the token (`POST /api/phones/handoff`), sets it as the web view's
//    Access cookie, and opens the house signed in.
// The token never travels in a link (PKCE, RFC 7636): see crates/moli-api/src/appli.rs.

import * as WebBrowser from "expo-web-browser";
import { Platform } from "react-native";
import { MoliApp } from "../modules/moli-app";

const BACK = "moli://handoff";

/** Where Google's sign-in starts: it never works inside the web view. */
export function isGoogleSignIn(host: string): boolean {
  return host === "accounts.google.com";
}

/** Only behind Cloudflare Access, on Android (the cookie is set natively). */
export const handoffAvailable = (base: string) =>
  Platform.OS === "android" && MoliApp != null && base.startsWith("https://");

/** Signs in with Google in a browser tab; `ok` once the web view holds the
 *  session. */
export async function googleSignIn(base: string): Promise<"ok" | "cancel" | "error"> {
  if (!MoliApp) return "error";
  const { verifier, challenge } = MoliApp.handoffKeys();
  const start = `${base}/api/mobile/handoff?challenge=${challenge}&back=${encodeURIComponent(BACK)}`;
  let result: WebBrowser.WebBrowserAuthSessionResult;
  try {
    result = await WebBrowser.openAuthSessionAsync(start, BACK, { showInRecents: false });
  } catch {
    return "error";
  }
  if (result.type !== "success") return "cancel";
  const code = /[?&]code=([A-Za-z0-9_-]{20,64})/.exec(result.url)?.[1];
  if (!code) return "error";
  try {
    const res = await fetch(`${base}/api/phones/handoff`, {
      method: "POST",
      headers: { "content-type": "application/json", accept: "application/json" },
      body: JSON.stringify({ code, verifier }),
    });
    if (!res.ok) return "error";
    const { token, expires } = (await res.json()) as { token?: string; expires?: number | null };
    if (!token) return "error";
    await MoliApp.setSessionCookie(base, token, expires ?? null);
    return "ok";
  } catch {
    return "error";
  }
}
