import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  ActivityIndicator,
  Alert,
  AppState,
  BackHandler,
  Linking,
  Platform,
  Pressable,
  StyleSheet,
  Text,
  View,
} from "react-native";
import { SafeAreaProvider, SafeAreaView } from "react-native-safe-area-context";
import { StatusBar } from "expo-status-bar";
import { WebView, type WebViewMessageEvent } from "react-native-webview";
import * as Battery from "expo-battery";
import * as Notifications from "expo-notifications";
import Constants from "expo-constants";
import { pairing, pairingRequest, pairingScript, report, resume, savePairing, startBackground, unpair } from "./src/moli";
import { allowedSuffixes, currentHouse, forgetHouse, PRESET, saveHouse } from "./src/house";
import { t } from "./src/texts";
import { Welcome } from "./src/Welcome";

// The shell: Moli's own dashboard, at the house's address, chosen on the
// first screen (src/house.ts) unless this build is made for one house.
// Behind Cloudflare Access, the sign-in page is Cloudflare's: it stays in the
// app too. Domains, never exact hosts (a redirect must not throw the user
// out), but whole labels only: "example.org" lets in example.org and its
// subdomains, never evilexample.org.
const inDomain = (host: string, domain: string) => {
  const d = domain.replace(/^\./, "");
  return host === d || host.endsWith("." + d);
};
const BG = "#161512";
// Tells the shell the page's background colour, now and when it changes
// (the dashboard follows daylight).
const THEME_WATCH = `
  (function () {
    var last = '';
    function send() {
      var bg = getComputedStyle(document.body).backgroundColor;
      if (bg && bg !== last && bg !== 'rgba(0, 0, 0, 0)') {
        last = bg;
        window.ReactNativeWebView.postMessage(JSON.stringify({ type: 'theme', bg: bg }));
      }
    }
    send();
    new MutationObserver(send).observe(document.documentElement, { attributes: true, subtree: false });
    setInterval(send, 5000);
  })();
  true;`;
const GOLD = "#E8B931";
const EVERY = 10 * 60 * 1000;
// Back from the background, the page has this long to answer, or it is reloaded.
const PAGE_ALIVE = 3000;
const PING = `window.ReactNativeWebView.postMessage(JSON.stringify({ type: 'alive' })); true;`;

// A notification that arrives while the app is open shows anyway.
Notifications.setNotificationHandler({
  handleNotification: async () => ({
    shouldShowBanner: true,
    shouldShowList: true,
    shouldPlaySound: true,
    shouldSetBadge: false,
  }),
});

async function pushToken(): Promise<string | null> {
  try {
    const { status } = await Notifications.requestPermissionsAsync();
    if (status !== "granted") return null;
    const projectId = Constants.expoConfig?.extra?.eas?.projectId as string | undefined;
    const token = await Notifications.getExpoPushTokenAsync(projectId ? { projectId } : undefined);
    return token.data;
  } catch {
    return null;
  }
}

// Which house, then its dashboard. `undefined`: still reading the keychain.
export default function App() {
  const [base, setBase] = useState<string | null | undefined>(undefined);
  const [last, setLast] = useState<string | null>(null);

  useEffect(() => {
    currentHouse().then(setBase, () => setBase(PRESET));
  }, []);

  const choose = useCallback(async (next: string) => {
    // A pairing made with another house is that house's: let it go. One from
    // before the app could change house (no `base`) follows the first house
    // chosen; if it is not its house, Moli refuses the token (401) and the
    // phone pairs again.
    const p = await pairing();
    if (p?.base && p.base !== next) await unpair();
    await saveHouse(next);
    setBase(next);
  }, []);

  const leave = useCallback(async () => {
    setLast(base ?? null);
    await unpair();
    await forgetHouse();
    // Even a build made for one house lets its owner type another.
    setBase(null);
  }, [base]);

  return (
    <SafeAreaProvider>
      {base === undefined ? (
        <View style={styles.center} />
      ) : base === null ? (
        <Welcome initial={last} onDone={choose} />
      ) : (
        <House key={base} base={base} onLeave={leave} />
      )}
    </SafeAreaProvider>
  );
}

function House({ base, onLeave }: { base: string; onLeave: () => void }) {
  const webRef = useRef<WebView>(null);
  const [failed, setFailed] = useState(false);
  const [askLocation, setAskLocation] = useState(false);
  // The top of the screen takes the dashboard's own colour (day or night).
  const [top, setTop] = useState<{ bg: string; dark: boolean }>({ bg: BG, dark: true });
  const canGoBack = useRef(false);
  const pairingAsked = useRef(false);
  const alive = useRef(true);

  // A sleeping app may lose its page: iOS kills the web view's process when
  // memory runs short (onContentProcessDidTerminate reloads it), and the view
  // then stays empty, a black screen. Back from the background, the page
  // must answer a ping, or it is reloaded.
  useEffect(() => {
    let away = false;
    let check: ReturnType<typeof setTimeout> | undefined;
    const sub = AppState.addEventListener("change", (s) => {
      if (s === "background") away = true;
      if (s !== "active" || !away) return;
      away = false;
      alive.current = false;
      webRef.current?.injectJavaScript(PING);
      clearTimeout(check);
      check = setTimeout(() => {
        if (!alive.current) webRef.current?.reload();
      }, PAGE_ALIVE);
    });
    return () => {
      clearTimeout(check);
      sub.remove();
    };
  }, []);

  // Android back button → the dashboard's history.
  useEffect(() => {
    if (Platform.OS !== "android") return;
    const sub = BackHandler.addEventListener("hardwareBackPress", () => {
      if (canGoBack.current) {
        webRef.current?.goBack();
        return true;
      }
      return false;
    });
    return () => sub.remove();
  }, []);

  // Reports while the app is open: on opening, every 10 minutes, and when
  // the battery starts or stops charging.
  useEffect(() => {
    // Each opening: the permission as it is now (maybe « once », or changed
    // in Settings), background watching restarted, a fresh report.
    const opened = async () => {
      if (await pairing()) {
        const state = await resume().catch(() => "denied" as const);
        if (state === "ask") setAskLocation(true);
      }
      await report("ouverture", null, true);
    };
    opened();
    const timer = setInterval(() => report("ouverte"), EVERY);
    const app = AppState.addEventListener("change", (s) => {
      if (s === "active") opened();
    });
    const charge = Battery.addBatteryStateListener(() => report("charge"));
    let lastLevel = 0;
    const level = Battery.addBatteryLevelListener(({ batteryLevel }) => {
      // Every 5 % only: a report per percent would be noise.
      const pct = Math.round(batteryLevel * 100);
      if (Math.abs(pct - lastLevel) >= 5) {
        lastLevel = pct;
        report("batterie");
      }
    });
    return () => {
      clearInterval(timer);
      app.remove();
      charge.remove();
      level.remove();
    };
  }, []);

  const allowed = useMemo(() => allowedSuffixes(base), [base]);

  const askLeave = useCallback(() => {
    Alert.alert(t.leaveTitle, t.leaveText, [
      { text: t.cancel, style: "cancel" },
      { text: t.leave, style: "destructive", onPress: onLeave },
    ]);
  }, [onLeave]);

  // Everything outside the house's hosts (and its sign-in page) opens in the browser.
  const onShouldStart = useCallback((req: { url: string }) => {
    try {
      const url = new URL(req.url);
      if (url.protocol === "about:" || url.protocol === "blob:" || url.protocol === "data:") return true;
      const internal = allowed.some((d) => inDomain(url.hostname, d));
      if ((url.protocol === "https:" || url.protocol === "http:") && internal) return true;
    } catch {
      return false;
    }
    Linking.openURL(req.url).catch(() => {});
    return false;
  }, [allowed]);

  // Once the dashboard itself shows (signed in), pair this phone if needed.
  const onLoadEnd = useCallback(async (e: { nativeEvent: { url: string } }) => {
    let url: URL;
    try {
      url = new URL(e.nativeEvent.url);
    } catch {
      return;
    }
    if (url.hostname !== new URL(base).hostname || url.pathname.startsWith("/cdn-cgi/")) return;
    if (pairingAsked.current || (await pairing())) return;
    pairingAsked.current = true;
    const token = await pushToken();
    webRef.current?.injectJavaScript(pairingScript(pairingRequest(token)));
  }, [base]);

  const onMessage = useCallback(async (e: WebViewMessageEvent) => {
    let msg: { type?: string; status?: number; body?: { id?: string; token?: string; home?: unknown; error?: string } };
    try {
      msg = JSON.parse(e.nativeEvent.data);
    } catch {
      return;
    }
    if (msg.type === "alive") {
      alive.current = true;
      return;
    }
    if (msg.type === "theme" && typeof (msg as { bg?: unknown }).bg === "string") {
      const bg = (msg as { bg: string }).bg;
      const rgb = bg.match(/\d+(\.\d+)?/g)?.slice(0, 3).map(Number) ?? [22, 21, 18];
      const light = 0.299 * rgb[0] + 0.587 * rgb[1] + 0.114 * rgb[2] > 140;
      setTop({ bg, dark: !light });
      return;
    }
    // The dashboard may offer to change house (window.MoliNative.changeHouse).
    if (msg.type === "change-house") {
      askLeave();
      return;
    }
    if (msg.type !== "paired") return;
    if (msg.status === 200 && msg.body?.id && msg.body.token) {
      await savePairing({ id: msg.body.id, token: msg.body.token, home: (msg.body.home as never) ?? null, base });
      await report("appairage");
      setAskLocation(true);
    } else {
      // Not signed in yet, or refused: try again on the next page.
      pairingAsked.current = false;
    }
  }, [base, askLeave]);

  const startLocation = useCallback(async () => {
    setAskLocation(false);
    try {
      await startBackground();
    } finally {
      await report("localisation");
    }
  }, []);

  return (
      <SafeAreaView style={[styles.root, { backgroundColor: top.bg }]} edges={["top"]}>
        <StatusBar style={top.dark ? "light" : "dark"} />
        {failed ? (
          <View style={styles.center}>
            <Text style={styles.title}>{t.unreachableTitle}</Text>
            <Text style={styles.sub}>{t.unreachableSub}</Text>
            <Text style={styles.address}>{base.replace(/^https?:\/\//, "")}</Text>
            <Pressable
              style={styles.button}
              onPress={() => {
                setFailed(false);
                webRef.current?.reload();
              }}
            >
              <Text style={styles.buttonText}>{t.retry}</Text>
            </Pressable>
            <Pressable style={styles.other} onPress={askLeave}>
              <Text style={styles.otherText}>{t.changeHouse}</Text>
            </Pressable>
          </View>
        ) : (
          <WebView
            ref={webRef}
            source={{ uri: base }}
            style={styles.web}
            applicationNameForUserAgent="MoliNative"
            injectedJavaScriptBeforeContentLoaded={`window.MoliNative = { platform: ${JSON.stringify(Platform.OS)}, mic: true, changeHouse: function () { window.ReactNativeWebView.postMessage(JSON.stringify({ type: 'change-house' })); } }; true;`}
            injectedJavaScript={THEME_WATCH}
            allowsBackForwardNavigationGestures
            allowsInlineMediaPlayback
            mediaPlaybackRequiresUserAction={false}
            // The page's microphone (talking to Moli): the app asks iOS once,
            // the page is not asked again.
            mediaCapturePermissionGrantType="grant"
            onNavigationStateChange={(s) => {
              canGoBack.current = s.canGoBack;
            }}
            onShouldStartLoadWithRequest={onShouldStart}
            onLoadEnd={onLoadEnd}
            onMessage={onMessage}
            onContentProcessDidTerminate={() => webRef.current?.reload()}
            onError={() => setFailed(true)}
            onHttpError={(e) => {
              if (e.nativeEvent.statusCode >= 500) setFailed(true);
            }}
            startInLoadingState
            renderLoading={() => (
              <View style={[styles.center, StyleSheet.absoluteFill]}>
                <ActivityIndicator size="large" color={GOLD} />
              </View>
            )}
            setBuiltInZoomControls={false}
            textZoom={100}
          />
        )}
        {askLocation && (
          <View style={styles.sheet}>
            <Text style={styles.sheetTitle}>{t.locationTitle}</Text>
            <Text style={styles.sheetText}>{t.locationText}</Text>
            <View style={styles.row}>
              <Pressable style={styles.later} onPress={() => setAskLocation(false)}>
                <Text style={styles.laterText}>{t.later}</Text>
              </Pressable>
              <Pressable style={styles.go} onPress={startLocation}>
                <Text style={styles.goText}>{t.continue}</Text>
              </Pressable>
            </View>
          </View>
        )}
      </SafeAreaView>
  );
}

const styles = StyleSheet.create({
  root: { flex: 1, backgroundColor: BG },
  web: { flex: 1, backgroundColor: BG },
  center: { flex: 1, alignItems: "center", justifyContent: "center", backgroundColor: BG, padding: 24 },
  title: { color: "#f3efe6", fontSize: 22, fontWeight: "600" },
  sub: { color: "#b9b2a4", fontSize: 14, marginTop: 8, textAlign: "center" },
  button: { marginTop: 20, borderColor: GOLD, borderWidth: 1, borderRadius: 999, paddingHorizontal: 24, paddingVertical: 10 },
  buttonText: { color: GOLD, fontSize: 15, fontWeight: "500" },
  address: { color: "#8a8376", fontSize: 13, marginTop: 14 },
  other: { marginTop: 14, paddingVertical: 8, paddingHorizontal: 16 },
  otherText: { color: "#b9b2a4", fontSize: 14, fontWeight: "600" },
  sheet: {
    position: "absolute",
    left: 12,
    right: 12,
    bottom: 24,
    padding: 20,
    borderRadius: 22,
    backgroundColor: "#22201c",
    shadowColor: "#000",
    shadowOpacity: 0.35,
    shadowRadius: 18,
    elevation: 12,
  },
  sheetTitle: { color: "#f3efe6", fontSize: 18, fontWeight: "700" },
  sheetText: { color: "#cfc8ba", fontSize: 14.5, lineHeight: 20, marginTop: 8 },
  row: { flexDirection: "row", justifyContent: "flex-end", marginTop: 16, gap: 10 },
  later: { paddingHorizontal: 16, paddingVertical: 10 },
  laterText: { color: "#b9b2a4", fontSize: 15, fontWeight: "600" },
  go: { backgroundColor: GOLD, borderRadius: 999, paddingHorizontal: 22, paddingVertical: 10 },
  goText: { color: "#161512", fontSize: 15, fontWeight: "700" },
});
