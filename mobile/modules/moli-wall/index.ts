// The wall tablet's native side (android/…/MoliWallModule.kt): absent on
// iOS and in a build without the module, where `MoliWall` is null.
import { requireOptionalNativeModule } from "expo";

export type WallState = {
  /** The tablet is a wall tablet: Moli is its home screen and stays in front. */
  enabled: boolean;
  /** The front camera wakes the screen. */
  camera: boolean;
  /** 1 (only someone close by) to 5 (the least movement). */
  sensitivity: number;
  /** Minutes without anyone before the screen sleeps; 0: never. */
  idle: number;
  /** Installed by QR code: Moli runs the tablet (no lock screen, no way out). */
  owner: boolean;
  /** Moli may put the screen to sleep. */
  admin: boolean;
  /** Moli is the home screen. */
  home: boolean;
  /** Moli is pinned in front. */
  pinned: boolean;
  cameraAllowed: boolean;
  /** A tablet-sized screen. */
  tablet: boolean;
};

export type WallOptions = { enabled: boolean; camera: boolean; sensitivity: number; idle: number };

type Subscription = { remove(): void };

export type MoliWallModule = {
  state(): WallState;
  dress(): boolean;
  configure(options: WallOptions): WallState;
  pin(on: boolean): boolean;
  wake(): void;
  sleep(): boolean;
  isAwake(): boolean;
  askAdmin(): boolean;
  release(): WallState;
  addListener(event: "onMotion", listener: (e: { at: number }) => void): Subscription;
};

export const MoliWall = requireOptionalNativeModule<MoliWallModule>("MoliWall");
