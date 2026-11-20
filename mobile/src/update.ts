// The app's updates, from the house itself (Android). The house keeps the
// latest APK (`mobile/publish-android.sh`) and tells a paired phone, with its
// own token, which build it holds (`GET /api/phones/<id>/app`). A newer one:
// - on a wall tablet Moli runs (device owner), downloaded and installed
//   without asking, while the screen sleeps (nobody in front of it); Moli
//   comes back by itself (UpdatedReceiver);
// - on a phone, offered: « Installer » downloads it and Android asks to
//   confirm. Declined, it is offered again a day later.

import { useCallback, useEffect, useRef, useState } from "react";
import { Alert, AppState, Platform } from "react-native";
import * as SecureStore from "expo-secure-store";
import { MoliApp } from "../modules/moli-app";
import { MoliWall } from "../modules/moli-wall";
import { pairing } from "./moli";
import { t } from "./texts";

type Latest = { version_code: number; version_name: string; size: number; sha256: string };

/** A phone looks again this often (and at each opening past it). */
const EVERY = 6 * 3600 * 1000;
/** A tablet Moli runs looks more often: it waits for the screen to sleep. */
const OWNER_EVERY = 10 * 60 * 1000;
/** Declined: offered again after this long. */
const DECLINED_FOR = 24 * 3600 * 1000;
const DECLINED = "moli.update.declined";

export const updatesAvailable = Platform.OS === "android" && MoliApp != null;

async function latest(base: string): Promise<{ build: Latest; id: string; token: string } | null> {
  const p = await pairing();
  if (!p) return null;
  try {
    const res = await fetch(`${base}/api/phones/${encodeURIComponent(p.id)}/app`, {
      headers: { authorization: `Bearer ${p.token}`, accept: "application/json" },
    });
    if (!res.ok) return null;
    const build = (await res.json()) as Latest;
    return typeof build.version_code === "number" && /^[0-9a-f]{64}$/i.test(build.sha256 ?? "")
      ? { build, id: p.id, token: p.token }
      : null;
  } catch {
    return null;
  }
}

async function declinedRecently(code: number): Promise<boolean> {
  const raw = await SecureStore.getItemAsync(DECLINED).catch(() => null);
  const [c, at] = (raw ?? "").split(":").map(Number);
  return c === code && Date.now() - at < DECLINED_FOR;
}

/** Watches for a newer build; `downloading` while one is fetched. */
export function useUpdates(base: string) {
  const [downloading, setDownloading] = useState(false);
  const busy = useRef(false);
  const lastLook = useRef(0);

  const fetchAndInstall = useCallback(
    async (found: { build: Latest; id: string; token: string }, quiet: boolean) => {
      setDownloading(!quiet);
      try {
        const url = `${base}/api/phones/${encodeURIComponent(found.id)}/app.apk`;
        const path = await MoliApp!.download(url, found.token, found.build.sha256);
        await MoliApp!.install(path);
      } catch {
        if (!quiet) Alert.alert(t.updateTitle, t.updateFailed);
      } finally {
        setDownloading(false);
      }
    },
    [base],
  );

  const look = useCallback(async () => {
    if (!updatesAvailable || busy.current) return;
    busy.current = true;
    try {
      lastLook.current = Date.now();
      const found = await latest(base);
      if (!found || found.build.version_code <= MoliApp!.versionCode()) return;
      // Moli runs the tablet: quietly, when nobody looks at it.
      if (MoliApp!.isOwner()) {
        const wall = MoliWall?.state();
        if (wall?.enabled && MoliWall?.isAwake()) return;
        await fetchAndInstall(found, true);
        return;
      }
      if (await declinedRecently(found.build.version_code)) return;
      const code = found.build.version_code;
      Alert.alert(t.updateTitle, t.updateText, [
        {
          text: t.later,
          style: "cancel",
          onPress: () => {
            SecureStore.setItemAsync(DECLINED, `${code}:${Date.now()}`).catch(() => {});
          },
        },
        {
          text: t.updateInstall,
          onPress: () => {
            if (!MoliApp!.canInstall()) {
              // Back from Android's screen, the next look offers it again.
              Alert.alert(t.updatePermTitle, t.updatePermText, [
                { text: t.cancel, style: "cancel" },
                { text: t.continue, onPress: () => MoliApp!.openInstallSettings() },
              ]);
              return;
            }
            void fetchAndInstall(found, false);
          },
        },
      ], {
        // Tapped beside it: declined too, not offered again at once.
        cancelable: true,
        onDismiss: () => {
          SecureStore.setItemAsync(DECLINED, `${code}:${Date.now()}`).catch(() => {});
        },
      });
    } finally {
      busy.current = false;
    }
  }, [base, fetchAndInstall]);

  useEffect(() => {
    if (!updatesAvailable) return;
    const every = MoliApp!.isOwner() ? OWNER_EVERY : EVERY;
    // Not at the very start: the page and the pairing come first.
    const first = setTimeout(look, 15_000);
    const timer = setInterval(look, every);
    const app = AppState.addEventListener("change", (s) => {
      // Back in the app after a while (or from Android's permission screen).
      if (s === "active" && Date.now() - lastLook.current > Math.min(every, 60_000)) void look();
    });
    return () => {
      clearTimeout(first);
      clearInterval(timer);
      app.remove();
    };
  }, [look]);

  return { downloading };
}
