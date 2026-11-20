// The app's own native side (android/…/MoliAppModule.kt): absent on iOS and
// in a build without the module, where `MoliApp` is null.
import { requireOptionalNativeModule } from "expo";

export type MoliAppModule = {
  /** This build's number (Android's versionCode). */
  versionCode(): number;
  /** Moli runs this tablet (device owner): it installs without asking. */
  isOwner(): boolean;
  /** Android lets Moli install apps. */
  canInstall(): boolean;
  /** Android's screen to let Moli install apps. */
  openInstallSettings(): boolean;
  /** A secret and its challenge (RFC 7636, S256). */
  handoffKeys(): { verifier: string; challenge: string };
  /** The Cloudflare Access session in the app's web view. */
  setSessionCookie(url: string, token: string, expires: number | null): Promise<boolean>;
  /** The update downloaded from the house and checked: its path. */
  download(url: string, token: string, sha256: string): Promise<string>;
  /** Installs it: at once on a tablet Moli runs, else Android asks. */
  install(path: string): Promise<boolean>;
};

export const MoliApp = requireOptionalNativeModule<MoliAppModule>("MoliApp");
