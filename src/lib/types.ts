export interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface MonitorInfo {
  id: number;
  label: string;
  x: number;
  y: number;
  width: number;
  height: number;
  scale: number;
  isPrimary: boolean;
}

export interface WindowRect {
  title: string;
  rect: Rect;
  hwnd: number;
}

export type CaptureTarget = "rectangle" | "window" | "screen";

export type CaptureFlow = "toolbar" | "instant";

export type Theme = "sistema" | "claro" | "oscuro";

export type Language = "sistema" | "es" | "en" | "zh";

export type OverlayIntent = "capture" | "record";

export type CaptureMode = "still" | "video";

export interface OverlayPayload {
  monitor: MonitorInfo;
  generation: number;
  windows: WindowRect[];
  settings: Settings;
  intent: OverlayIntent;
  screenNumber: number;
  screenCount: number;
  lastRegion: Rect | null;
}

export type StillAction = "copy" | "save" | "save_as" | "pin" | "text";

export interface StillResult {
  path: string | null;
  copied: boolean;
  width: number;
  height: number;
}

export type RecordFormat = "video";

export type SessionFormat = RecordFormat | "still";

export interface RecordOptions {
  format: RecordFormat;
  fps: number;
  audio: boolean;
  microphone: boolean;
  window?: number;
}

export interface SessionInfo {
  id: string;
  region: Rect;
  fps: number;
  frameCount: number;
  durationMs: number;
  hasAudio: boolean;
  format: SessionFormat;
  mp4Path: string | null;
}

export interface RecordingTick {
  outputPath?: string;
  elapsedMs: number;
  frames: number;
  bytes: number;
  paused: boolean;
  saving: boolean;
  format: RecordFormat;
  audio: boolean;
  microphone: boolean;
}

export interface EncodingSettings {
  ffmpegPath?: string;
  rateControl?: "crf" | "bitrate";
  crf?: number;
  engine: "native" | "ffmpeg";
  encoder: string;
  quality: number;
  bitrateMbps: number;
  preset: string;
  gpu: string;
  extraArgs: string;
}

export interface Settings {
  encoding?: EncodingSettings;
  captureTarget?: CaptureTarget;
  recordTarget?: CaptureTarget;
  captureShortcut: string;
  theme: Theme;
  language: Language;
  recordShortcut: string;
  replayShortcut: string;
  saveDirectory: string;
  copyAfterCapture: boolean;
  captureCursor: boolean;
  recordAudio: boolean;
  systemAudioDevice?: string | null;
  microphoneDevice?: string | null;
  recordMicrophone: boolean;
  fps: number;
  replayEnabled: boolean;
  replaySeconds: number;
  replayScreen: number | null;
  replayFps: number;
  replayHeight: number;
  playSound: boolean;
  showMagnifier: boolean;
  startWithWindows: boolean;
  captureDelaySeconds: number;
  hideDesktopIcons: boolean;
  captureFlow: CaptureFlow;
  printScreenCapture: boolean;
  takeWinShiftS: boolean;
  onboarded: boolean;
  snippingKeyRestore: number | null;
  disabledHotkeysRestore: string | null;
}

export interface ShortcutStatus {
  capture: boolean;
  record: boolean;
  replay: boolean;
  printScreen: boolean;
  winShiftS: boolean;
}

export interface Screen {
  id: number;
  label: string;
  x: number;
  y: number;
  width: number;
  height: number;
  scale: number;
  isPrimary: boolean;
}

export interface ReplayStatus {
  running: boolean;
  seconds: number;
  screen: number;
  screenLabel: string;
  bytes: number;
  bytesPerSecond: number;
  width: number;
  height: number;
  bufferedMs: number;
}

export interface PrintScreenState {
  enabled: boolean;
  active: boolean;
  takenByWindows: boolean;
}

export interface CacheStats {
  bytes: number;
  sessions: number;
}

export const EVENTS = {
  overlayShow: "winshotx://overlay-show",
  overlayHide: "winshotx://overlay-hide",
  recordingTick: "winshotx://recording-tick",
  recordingStopped: "winshotx://recording-stopped",
  settingsChanged: "winshotx://settings-changed",
  checkUpdate: "winshotx://check-update",
  settingsShown: "winshotx://settings-shown",
  replay: "winshotx://replay",
  trayMenuOpened: "winshotx://tray-menu-opened",
  screenNumber: "winshotx://screen-number",
  overlayMode: "winshotx://overlay-mode",
  overlayTakeScreen: "winshotx://overlay-take-screen",
  countdown: "winshotx://countdown",
} as const;

export interface OverlayModeState {
  target?: CaptureTarget;
  mode: CaptureMode;
  fullScreen: boolean;
  withToolbar: boolean;
}

export interface TrayMenuState {
  version: string;
  recording: boolean;
  replay: boolean;
  captureShortcut: string;
  recordShortcut: string;
  replayShortcut: string;
}

export interface CapturaReciente {
  path: string;
  nombre: string;
  haceSegundos: number;
}

export type TrayMenuAction =
  | "capture"
  | "record"
  | "replay"
  | "folder"
  | "settings"
  | "update"
  | "quit";

export interface AudioDevice {
  id: string;
  name: string;
  kind: "output" | "input";
  isDefault: boolean;
}
export interface AudioLevel {
  peak: number;
  db: number;
  state: "disabled" | "connecting" | "ready" | "error";
  error: string | null;
}
export interface AudioLevels {
  id: string;
  running: boolean;
  system: AudioLevel;
  microphone: AudioLevel;
}
