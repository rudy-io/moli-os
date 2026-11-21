// The wall tablet (modules/moli-wall): a tablet that shows Moli alone, its
// screen on while someone is there and asleep otherwise. The page's
// « Tablette murale » card sets it up through `window.MoliNative.wall…`
// (messages `wall-*`); the native side wakes the screen when the front camera
// sees a movement, and this file puts it to sleep after `idle` minutes
// without a movement or a touch.
//
// Without the right to switch the screen off (Moli not administrator), the
// page is covered in black instead: the backlight stays on, but nothing
// shows, and a touch or a movement brings the page back.
import { useCallback, useEffect, useRef, useState } from "react";
import { AppState, PermissionsAndroid, Platform } from "react-native";
import { MoliWall, type WallOptions, type WallState } from "../modules/moli-wall";

export type { WallState };

/** Whether this app can be a wall tablet (Android, with the module). */
export const wallAvailable = Platform.OS === "android" && MoliWall != null;

/** The page reports touches, at most every 5 s (`wall-activity`). */
export const WALL_WATCH = `
  (function () {
    if (!window.MoliNative || !window.MoliNative.wall) return;
    var last = 0;
    function seen() {
      var now = Date.now();
      if (now - last < 5000) return;
      last = now;
      // Through the bridge: it carries the nonce the app checks (App.tsx).
      window.MoliNative.wallSend('activity');
    }
    document.addEventListener('pointerdown', seen, true);
    document.addEventListener('keydown', seen, true);
  })();
  true;`;

/** Checked this often whether the screen should sleep. */
const TICK = 10_000;

export type WallMessage = { type?: string; options?: Partial<WallOptions>; on?: boolean };

export function useWall(tell: (state: WallState | null) => void) {
  const [state, setState] = useState<WallState | null>(() => (wallAvailable ? MoliWall!.state() : null));
  const [dark, setDark] = useState(false);
  const lastSeen = useRef(Date.now());
  const stateRef = useRef(state);
  stateRef.current = state;

  const refresh = useCallback(() => {
    if (!wallAvailable) return null;
    const next = MoliWall!.state();
    setState(next);
    tell(next);
    return next;
  }, [tell]);

  /** Someone is there: the timer starts again, the page shows. */
  const seen = useCallback(() => {
    lastSeen.current = Date.now();
    setDark(false);
  }, []);

  // Orientation and full screen, once the shell is up (a tablet turns freely).
  useEffect(() => {
    if (wallAvailable) MoliWall!.dress();
  }, []);

  // Movements seen by the camera (the native side has woken the screen).
  useEffect(() => {
    if (!wallAvailable) return;
    const sub = MoliWall!.addListener("onMotion", seen);
    return () => sub.remove();
  }, [seen]);

  // Back in front (woken by hand, or after Settings): someone is there, and
  // the state may have changed (administrator granted, pinning left).
  useEffect(() => {
    if (!wallAvailable) return;
    const sub = AppState.addEventListener("change", (s) => {
      if (s !== "active") return;
      seen();
      refresh();
    });
    return () => sub.remove();
  }, [seen, refresh]);

  // Asleep after `idle` minutes without anyone.
  useEffect(() => {
    if (!wallAvailable) return;
    const timer = setInterval(() => {
      const s = stateRef.current;
      if (!s?.enabled || s.idle <= 0) return;
      if (Date.now() - lastSeen.current < s.idle * 60_000) return;
      if (!MoliWall!.isAwake()) return;
      if (!MoliWall!.sleep()) setDark(true);
    }, TICK);
    return () => clearInterval(timer);
  }, []);

  /** A `wall-*` message from the page. True when it was one. */
  const onMessage = useCallback(
    async (msg: WallMessage): Promise<boolean> => {
      if (!msg.type?.startsWith("wall-")) return false;
      if (!wallAvailable) {
        tell(null);
        return true;
      }
      const current = MoliWall!.state();
      switch (msg.type) {
        case "wall-activity":
          seen();
          return true;
        case "wall-state":
          refresh();
          return true;
        case "wall-configure": {
          const o = msg.options ?? {};
          const options: WallOptions = {
            enabled: o.enabled ?? current.enabled,
            camera: o.camera ?? current.camera,
            sensitivity: o.sensitivity ?? current.sensitivity,
            idle: o.idle ?? current.idle,
          };
          // The camera is asked for the first time it is needed.
          if (options.enabled && options.camera && !current.cameraAllowed) {
            const answer = await PermissionsAndroid.request(PermissionsAndroid.PERMISSIONS.CAMERA);
            if (answer !== PermissionsAndroid.RESULTS.GRANTED) options.camera = false;
          }
          MoliWall!.configure(options);
          seen();
          refresh();
          return true;
        }
        case "wall-admin":
          MoliWall!.askAdmin();
          return true;
        case "wall-pin":
          MoliWall!.pin(msg.on !== false);
          setTimeout(refresh, 1500);
          return true;
        case "wall-release":
          MoliWall!.release();
          refresh();
          return true;
        default:
          return true;
      }
    },
    [refresh, seen, tell],
  );

  return { state, dark, wake: seen, onMessage };
}
