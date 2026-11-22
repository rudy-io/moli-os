import { Component, type ReactNode, useCallback, useEffect, useMemo, useRef, useState } from "react";
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
import { allowedSuffixes, currentHouse, forgetHouse, looksLikeMoliSession, PRESET, saveHouse } from "./src/house";
import { declineOffer, fingerprintReady, forgetPin, keepPin, offerDeclined, pinKept, unlockPin } from "./src/pinlock";
import { t } from "./src/texts";
import { Welcome } from "./src/Welcome";
import { useWall, WALL_WATCH, wallAvailable, type WallState } from "./src/wall";
import { useUpdates } from "./src/update";
import { googleSignIn, handoffAvailable, isGoogleSignIn } from "./src/handoff";
import { MoliApp } from "./modules/moli-app";

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
// Once signed in (behind Cloudflare Access the first screen could not tell),
// the page is asked whether it is a Moli: its session answer, with the
// page's own cookies.
const MOLI_CHECK = `
  fetch('/api/session', { headers: { accept: 'application/json' } })
    .then(function (r) { return r.ok ? r.json() : null; })
    .catch(function () { return null; })
    .then(function (b) { window.ReactNativeWebView.postMessage(JSON.stringify({ type: 'moli-check', body: b })); });
  true;`;

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
    // The code kept behind the fingerprint belongs to this house only.
    if (base) await forgetPin(base);
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
        <Guard key={base} onLeave={leave}>
          <House base={base} onLeave={leave} />
        </Guard>
      )}
    </SafeAreaProvider>
  );
}

/** A screen that breaks never closes the app: it offers to retry or to
 * choose another house (a bad address must not lock anyone out). */
class Guard extends Component<{ children: ReactNode; onLeave: () => void }, { broken: boolean }> {
  state = { broken: false };

  static getDerivedStateFromError() {
    return { broken: true };
  }

  render() {
    if (!this.state.broken) return this.props.children;
    return (
      <SafeAreaView style={[styles.root, styles.center]}>
        <Text style={styles.title}>{t.brokenTitle}</Text>
        <Text style={styles.sub}>{t.brokenSub}</Text>
        <Pressable style={styles.button} onPress={() => this.setState({ broken: false })}>
          <Text style={styles.buttonText}>{t.retry}</Text>
        </Pressable>
        <Pressable style={styles.other} onPress={this.props.onLeave}>
          <Text style={styles.otherText}>{t.changeHouse}</Text>
        </Pressable>
      </SafeAreaView>
    );
  }
}

function House({ base, onLeave }: { base: string; onLeave: () => void }) {
  const webRef = useRef<WebView>(null);
  const [failed, setFailed] = useState(false);
  // The address turned out not to be a Moli (checked once signed in).
  const [notMoli, setNotMoli] = useState(false);
  const checked = useRef(false);
  const [askLocation, setAskLocation] = useState(false);
  // The top of the screen takes the dashboard's own colour (day or night).
  const [top, setTop] = useState<{ bg: string; dark: boolean }>({ bg: BG, dark: true });
  const canGoBack = useRef(false);
  const pairingAsked = useRef(false);
  const alive = useRef(true);
  // The house code behind the fingerprint (src/pinlock.ts): what the
  // dashboard is told, before the page loads and whenever it changes.
  const [lock, setLock] = useState({ fingerprint: false, kept: false });
  useEffect(() => {
    let live = true;
    Promise.all([fingerprintReady(), pinKept(base)]).then(([fingerprint, kept]) => {
      if (live) setLock({ fingerprint, kept: fingerprint && kept });
    });
    return () => {
      live = false;
    };
  }, [base]);
  useEffect(() => {
    webRef.current?.injectJavaScript(
      `if (window.MoliNative) { window.MoliNative.fingerprint = ${lock.fingerprint}; window.MoliNative.pinSaved = ${lock.kept}; } true;`,
    );
  }, [lock]);
  // The wall tablet (src/wall.ts): its state is told to the page with a
  // `moli-native-wall` event on `window` (null: not possible here).
  const tellWall = useCallback((state: WallState | null) => {
    webRef.current?.injectJavaScript(
      `window.dispatchEvent(new CustomEvent('moli-native-wall', { detail: ${JSON.stringify(state)} })); true;`,
    );
  }, []);
  const wall = useWall(tellWall);
  // The app's own updates, from the house (src/update.ts).
  const updates = useUpdates(base);
  // Known only to the bridge injected in the main frame: on Android every
  // frame of the page reaches ReactNativeWebView.postMessage, a third-party
  // iframe of the dashboard included, but it cannot read the main frame's
  // window.MoliNative. The house's own orders (wall-, pin-) carry it. Drawn
  // from the platform's secure random (the PKCE verifier), else Math.random.
  const nonce = useMemo(
    () => MoliApp?.handoffKeys().verifier ?? `${Math.random().toString(36).slice(2)}${Date.now().toString(36)}`,
    [],
  );
  const bridge = useMemo(
    () =>
      `window.MoliNative = { platform: ${JSON.stringify(Platform.OS)}, mic: true, ` +
      `fingerprint: ${lock.fingerprint}, pinSaved: ${lock.kept}, ` +
      `changeHouse: function () { window.ReactNativeWebView.postMessage(JSON.stringify({ type: 'change-house' })); }, ` +
      `unlockPin: function () { window.ReactNativeWebView.postMessage(JSON.stringify({ type: 'pin-unlock', k: ${JSON.stringify(nonce)} })); }, ` +
      `rememberPin: function (pin) { window.ReactNativeWebView.postMessage(JSON.stringify({ type: 'pin-remember', pin: String(pin), k: ${JSON.stringify(nonce)} })); }, ` +
      `offerPin: function (pin) { window.ReactNativeWebView.postMessage(JSON.stringify({ type: 'pin-offer', pin: String(pin), k: ${JSON.stringify(nonce)} })); }, ` +
      `forgetPin: function () { window.ReactNativeWebView.postMessage(JSON.stringify({ type: 'pin-forget', k: ${JSON.stringify(nonce)} })); }, ` +
      `wall: ${wallAvailable}, ` +
      `wallSend: function (type, extra) { window.ReactNativeWebView.postMessage(JSON.stringify(Object.assign({}, extra || {}, { type: 'wall-' + type, k: ${JSON.stringify(nonce)} }))); } }; true;`,
    [lock, nonce],
  );

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

  // Google's sign-in, behind Cloudflare Access: in a browser tab of the
  // app's own, its session then brought back (src/handoff.ts).
  const signingIn = useRef(false);
  const signInWithGoogle = useCallback(async () => {
    if (signingIn.current) return;
    signingIn.current = true;
    try {
      const done = await googleSignIn(base);
      if (done === "ok") webRef.current?.injectJavaScript(`location.replace(${JSON.stringify(base + "/")}); true;`);
      else if (done === "error") Alert.alert(t.unreachableTitle, t.googleFailed);
    } finally {
      signingIn.current = false;
    }
  }, [base]);

  // Everything outside the house's hosts (and its sign-in page) opens in the browser.
  const onShouldStart = useCallback((req: { url: string }) => {
    try {
      const url = new URL(req.url);
      if (url.protocol === "about:" || url.protocol === "blob:" || url.protocol === "data:") return true;
      const internal = allowed.some((d) => inDomain(url.hostname, d));
      if ((url.protocol === "https:" || url.protocol === "http:") && internal) return true;
      if (isGoogleSignIn(url.hostname) && handoffAvailable(base)) {
        void signInWithGoogle();
        return false;
      }
    } catch {
      return false;
    }
    Linking.openURL(req.url).catch(() => {});
    return false;
  }, [allowed, base, signInWithGoogle]);

  // Once the dashboard itself shows (signed in), pair this phone if needed.
  const onLoadEnd = useCallback(async (e: { nativeEvent: { url: string } }) => {
    let url: URL;
    try {
      url = new URL(e.nativeEvent.url);
    } catch {
      return;
    }
    if (url.hostname !== new URL(base).hostname || url.pathname.startsWith("/cdn-cgi/")) return;
    if (!checked.current) {
      checked.current = true;
      webRef.current?.injectJavaScript(MOLI_CHECK);
    }
    if (pairingAsked.current || (await pairing())) return;
    pairingAsked.current = true;
    const token = await pushToken();
    webRef.current?.injectJavaScript(pairingScript(pairingRequest(token)));
  }, [base]);

  const onMessage = useCallback(async (e: WebViewMessageEvent) => {
    let msg: {
      type?: string;
      status?: number;
      pin?: string;
      body?: { id?: string; token?: string; home?: unknown; error?: string };
    };
    try {
      msg = JSON.parse(e.nativeEvent.data);
    } catch {
      return;
    }
    if (msg.type === "alive") {
      alive.current = true;
      return;
    }
    if (msg.type === "moli-check") {
      // A network hiccup (null) is not proof: only a plain « not Moli » is.
      const body = (msg as { body?: unknown }).body;
      if (body !== null && body !== undefined && !looksLikeMoliSession(body)) setNotMoli(true);
      return;
    }
    // The wall tablet: only the house's own pages may set it up.
    if (msg.type?.startsWith("wall-")) {
      let fromHouse = false;
      try {
        fromHouse = new URL(e.nativeEvent.url).host === new URL(base).host;
      } catch {
        fromHouse = false;
      }
      if (fromHouse && (msg as { k?: unknown }).k === nonce) await wall.onMessage(msg as Parameters<typeof wall.onMessage>[0]);
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
    // The code behind the fingerprint: only for the house's own pages, never
    // for another page the web view may show (the sign-in page, a redirect).
    if (msg.type?.startsWith("pin-")) {
      let fromHouse = false;
      try {
        fromHouse = new URL(e.nativeEvent.url).host === new URL(base).host;
      } catch {
        fromHouse = false;
      }
      // The page's address is the main frame's: the nonce tells the bridge
      // from an iframe of the page.
      if (!fromHouse || (msg as { k?: unknown }).k !== nonce) return;
      if (msg.type === "pin-unlock") {
        const answer = await unlockPin(base);
        if ("error" in answer && answer.error === "unavailable") setLock((l) => ({ ...l, kept: false }));
        webRef.current?.injectJavaScript(
          `window.dispatchEvent(new CustomEvent('moli-native-pin', { detail: ${JSON.stringify(answer)} })); true;`,
        );
      } else if (msg.type === "pin-remember" && typeof msg.pin === "string") {
        if (await keepPin(base, msg.pin)) setLock((l) => ({ ...l, kept: true }));
      } else if (msg.type === "pin-offer" && typeof msg.pin === "string") {
        // A code just typed right: offered once, in the app's own dialog
        // (the code sheet closes behind it). « No thanks » is final.
        const pin = msg.pin;
        if (!(await fingerprintReady()) || (await pinKept(base)) || (await offerDeclined(base))) return;
        Alert.alert(t.fingerprintOfferTitle, t.fingerprintOfferText, [
          { text: t.fingerprintNo, style: "cancel", onPress: () => declineOffer(base) },
          {
            text: t.fingerprintYes,
            onPress: async () => {
              if (await keepPin(base, pin)) setLock((l) => ({ ...l, kept: true }));
            },
          },
        ]);
      } else if (msg.type === "pin-forget") {
        await forgetPin(base);
        setLock((l) => ({ ...l, kept: false }));
      }
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
  }, [base, askLeave, wall.onMessage, nonce]);

  const startLocation = useCallback(async () => {
    setAskLocation(false);
    try {
      await startBackground();
    } finally {
      await report("localisation");
    }
  }, []);

  return (
      // Every edge: Android's buttons at the bottom (or on the side, a tablet
      // turned) must not cover the dashboard's menu.
      <SafeAreaView style={[styles.root, { backgroundColor: top.bg }]} edges={["top", "bottom", "left", "right"]}>
        <StatusBar style={top.dark ? "light" : "dark"} hidden={!!wall.state?.enabled} />
        {notMoli ? (
          <View style={styles.center}>
            <Text style={styles.title}>{t.notMoliTitle}</Text>
            <Text style={styles.sub}>{t.notMoli}</Text>
            <Text style={styles.address}>{base.replace(/^https?:\/\//, "")}</Text>
            <Pressable style={styles.button} onPress={onLeave}>
              <Text style={styles.buttonText}>{t.changeHouse}</Text>
            </Pressable>
          </View>
        ) : failed ? (
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
            injectedJavaScriptBeforeContentLoaded={bridge}
            injectedJavaScript={THEME_WATCH + WALL_WATCH}
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
        {wall.dark && (
          // Nobody there, and Moli may not switch the screen off: black over
          // the page until a touch or a movement.
          <Pressable style={[StyleSheet.absoluteFill, styles.dark]} onPress={wall.wake} />
        )}
        {updates.downloading && (
          <View style={styles.toast} pointerEvents="none">
            <ActivityIndicator size="small" color={GOLD} />
            <Text style={styles.toastText}>{t.updateDownloading}</Text>
          </View>
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
  dark: { backgroundColor: "#000" },
  toast: {
    position: "absolute",
    alignSelf: "center",
    bottom: 96,
    flexDirection: "row",
    alignItems: "center",
    gap: 10,
    paddingVertical: 10,
    paddingHorizontal: 16,
    borderRadius: 999,
    backgroundColor: "rgba(22, 21, 18, 0.92)",
  },
  toastText: { color: "#f4f1ea", fontSize: 14, fontWeight: "600" },
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
