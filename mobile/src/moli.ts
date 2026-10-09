// What the app does besides showing the dashboard: it pairs the phone with
// Moli, then reports the phone's state (battery, charging, Wi-Fi, location)
// in front and in the background, so the house knows who is home.
//
// Moli side: crates/moli-phones (POST /api/mobile/register, POST
// /api/phones/<id>/report). The token lives in the keychain only.

import * as Application from "expo-application";
import * as Battery from "expo-battery";
import * as Device from "expo-device";
import * as Location from "expo-location";
import * as Notifications from "expo-notifications";
import Constants from "expo-constants";
import * as SecureStore from "expo-secure-store";
import * as TaskManager from "expo-task-manager";
import NetInfo from "@react-native-community/netinfo";
import { Platform } from "react-native";

// iOS gives the Wi-Fi's name only when asked (and with the location granted
// plus the Wi-Fi information entitlement, see app.json).
NetInfo.configure({ shouldFetchWiFiSSID: true });

// The house's address: app.config.js (house.json or MOLI_URL).
export const BASE = (Constants.expoConfig?.extra?.moliUrl as string | undefined) ?? "https://maison.example.org";
const PAIRING = "moli.pairing";
export const LOCATION_TASK = "moli-location";
export const REGION_TASK = "moli-home-region";

export type Home = { latitude: number; longitude: number; radius: number };
export type Pairing = { id: string; token: string; home?: Home | null };

export async function pairing(): Promise<Pairing | null> {
  try {
    const raw = await SecureStore.getItemAsync(PAIRING);
    return raw ? (JSON.parse(raw) as Pairing) : null;
  } catch {
    return null;
  }
}

export async function savePairing(p: Pairing): Promise<void> {
  await SecureStore.setItemAsync(PAIRING, JSON.stringify(p), {
    keychainAccessible: SecureStore.AFTER_FIRST_UNLOCK,
  });
}

/** What the phone tells Moli when it pairs. */
export function pairingRequest(pushToken: string | null) {
  return {
    name: Device.deviceName ?? Device.modelName ?? "Téléphone",
    platform: Platform.OS === "ios" ? "ios" : "android",
    model: (Device.modelName ?? "").slice(0, 60),
    ...(pushToken ? { push_token: pushToken } : {}),
  };
}

/** A script run inside the dashboard (signed in): pairs this phone. */
export function pairingScript(request: object): string {
  const body = JSON.stringify(JSON.stringify(request));
  return `
    (function () {
      fetch('/api/mobile/register', {
        method: 'POST',
        credentials: 'same-origin',
        headers: { 'content-type': 'application/json', 'x-moli-origin': 'ui' },
        body: ${body},
      })
        .then(function (r) { return r.json().then(function (j) { return { status: r.status, body: j }; }); })
        .then(function (a) { window.ReactNativeWebView.postMessage(JSON.stringify({ type: 'paired', status: a.status, body: a.body })); })
        .catch(function (e) { window.ReactNativeWebView.postMessage(JSON.stringify({ type: 'paired', status: 0, body: { error: String(e) } })); });
    })();
    true;`;
}

let lastLocation: Location.LocationObject | null = null;
let pushSent: string | null = null;

/** The push token, if notifications are allowed (no prompt here). */
async function currentPushToken(): Promise<string | null> {
  try {
    const { status } = await Notifications.getPermissionsAsync();
    if (status !== "granted") return null;
    const projectId = Constants.expoConfig?.extra?.eas?.projectId as string | undefined;
    return (await Notifications.getExpoPushTokenAsync(projectId ? { projectId } : undefined)).data;
  } catch {
    return null;
  }
}

/** A system call that may never answer must not hold the report back. */
function within<T>(ms: number, work: Promise<T>): Promise<T | null> {
  return Promise.race([work, new Promise<null>((resolve) => setTimeout(() => resolve(null), ms))]);
}

/** One report: what is known now, plus what woke the app. */
export async function report(
  reason: string,
  location?: Location.LocationObject | null,
  fresh = false,
): Promise<void> {
  const p = await pairing();
  if (!p) return;
  if (location) lastLocation = location;
  const body: Record<string, unknown> = { reason: reason.slice(0, 60) };
  try {
    const level = await within(3000, Battery.getBatteryLevelAsync());
    if (level != null && level >= 0) body.battery = Math.round(level * 100);
    const state = await within(3000, Battery.getBatteryStateAsync());
    if (state != null) body.charging = state === Battery.BatteryState.CHARGING || state === Battery.BatteryState.FULL;
  } catch {
    // No battery reading (simulator): the rest still goes.
  }
  try {
    const net = await within(4000, NetInfo.fetch());
    const ssid = net?.type === "wifi" ? (net.details as { ssid?: string | null } | null)?.ssid : null;
    if (ssid) body.wifi = ssid.slice(0, 60);
  } catch {
    // Wi-Fi name unknown (permission): fine.
  }
  if (!location && fresh) {
    // On opening: a fresh position (a phone lying still has none cached).
    try {
      const fg = await Location.getForegroundPermissionsAsync();
      if (fg.status === "granted") {
        lastLocation =
          (await within(10_000, Location.getCurrentPositionAsync({ accuracy: Location.Accuracy.Balanced }))) ??
          lastLocation;
      }
    } catch {
      // No position now: the report goes without it.
    }
  } else if (!location && !lastLocation) {
    try {
      lastLocation = await within(3000, Location.getLastKnownPositionAsync({ maxAge: 10 * 60 * 1000 }));
    } catch {
      lastLocation = null;
    }
  }
  const at = location ?? lastLocation;
  if (at) {
    body.latitude = at.coords.latitude;
    body.longitude = at.coords.longitude;
    if (at.coords.accuracy != null) body.accuracy = Math.round(at.coords.accuracy);
  }
  body.app = `${Application.nativeApplicationVersion ?? "?"} (${Application.nativeBuildVersion ?? "?"})`;
  // The push token once per launch (or when it changes): Moli keeps it current.
  const token = await within(4000, currentPushToken());
  if (token && token !== pushSent) body.push_token = token;
  try {
    const abort = new AbortController();
    const timer = setTimeout(() => abort.abort(), 15_000);
    const res = await fetch(`${BASE}/api/phones/${p.id}/report`, {
      method: "POST",
      headers: { "content-type": "application/json", authorization: `Bearer ${p.token}` },
      body: JSON.stringify(body),
      signal: abort.signal,
    }).finally(() => clearTimeout(timer));
    if (res.status === 401) {
      // Removed in Moli: pair again next time the dashboard opens.
      await SecureStore.deleteItemAsync(PAIRING);
      return;
    }
    if (!res.ok) return;
    if (typeof body.push_token === "string") pushSent = body.push_token;
    const answer = (await res.json()) as { home?: Home | null };
    if (answer.home && JSON.stringify(answer.home) !== JSON.stringify(p.home ?? null)) {
      await savePairing({ ...p, home: answer.home });
      await watchHome(answer.home);
    }
  } catch {
    // Offline: the next event will report.
  }
}

/**
 * On every launch, already paired: what the location permission allows now
 * (it may have been « once », or changed in Settings), and the background
 * watching restarted if « always » is granted. `ask`: the permission is
 * missing but iOS may still ask (show the priming screen).
 */
export async function resume(): Promise<"always" | "foreground" | "ask" | "denied"> {
  const fg = await Location.getForegroundPermissionsAsync();
  if (fg.status !== "granted") return fg.canAskAgain && (await mayAsk()) ? "ask" : "denied";
  const bg = await Location.getBackgroundPermissionsAsync();
  if (bg.status !== "granted") return bg.canAskAgain && (await mayAsk()) ? "ask" : "foreground";
  return startBackground();
}

const ASKED = "moli.location-asked";

/** The priming screen once a week at most: never a nag at each opening. */
async function mayAsk(): Promise<boolean> {
  try {
    const last = Number((await SecureStore.getItemAsync(ASKED)) ?? 0);
    if (Date.now() - last < 7 * 24 * 3600 * 1000) return false;
    await SecureStore.setItemAsync(ASKED, String(Date.now()));
    return true;
  } catch {
    return false;
  }
}

/** Asks for the location (while using, then always) and starts watching. */
export async function startBackground(): Promise<"always" | "foreground" | "denied"> {
  const fg = await Location.requestForegroundPermissionsAsync();
  if (fg.status !== "granted") return "denied";
  const bg = await Location.requestBackgroundPermissionsAsync();
  if (bg.status !== "granted") return "foreground";
  const running = await Location.hasStartedLocationUpdatesAsync(LOCATION_TASK).catch(() => false);
  if (!running) {
    await Location.startLocationUpdatesAsync(LOCATION_TASK, {
      accuracy: Location.Accuracy.Balanced,
      distanceInterval: 250,
      deferredUpdatesInterval: 5 * 60 * 1000,
      pausesUpdatesAutomatically: true,
      activityType: Location.ActivityType.Other,
      showsBackgroundLocationIndicator: false,
      foregroundService: {
        notificationTitle: "Moli",
        notificationBody: "Moli sait si vous êtes à la maison.",
      },
    });
  }
  const p = await pairing();
  if (p?.home) await watchHome(p.home);
  return "always";
}

/** The home's edge: arriving and leaving wake the app, even closed. */
async function watchHome(home: Home): Promise<void> {
  const bg = await Location.getBackgroundPermissionsAsync();
  if (bg.status !== "granted") return;
  await Location.startGeofencingAsync(REGION_TASK, [
    {
      identifier: "maison",
      latitude: home.latitude,
      longitude: home.longitude,
      radius: Math.max(100, home.radius),
      notifyOnEnter: true,
      notifyOnExit: true,
    },
  ]);
}

// Background tasks: defined at load time (index.ts imports this file first).
TaskManager.defineTask(LOCATION_TASK, async ({ data, error }) => {
  if (error) return;
  const locations = (data as { locations?: Location.LocationObject[] } | undefined)?.locations ?? [];
  await report("position", locations[locations.length - 1] ?? null);
});

TaskManager.defineTask(REGION_TASK, async ({ data, error }) => {
  if (error) return;
  const event = (data as { eventType?: Location.GeofencingEventType } | undefined)?.eventType;
  const reason = event === Location.GeofencingEventType.Enter ? "arrivée" : "départ";
  let here: Location.LocationObject | null = null;
  try {
    here = await within(15_000, Location.getCurrentPositionAsync({ accuracy: Location.Accuracy.Balanced }));
  } catch {
    here = null;
  }
  await report(reason, here);
});
