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

export type Theme = "sistema" | "claro" | "oscuro";

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

export type StillAction = "copy";

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
  engine: "ffmpeg";
  encoder: string;
  quality: number;
  bitrateMbps: number;
  preset: string;
  gpu: string;
  extraArgs: string;
}

export interface RecordingPreset {
  name: string;
  fps: number;
  encoding: EncodingSettings;
}

export interface Settings {
  presets: RecordingPreset[];
  activePreset: string;
  encoding?: EncodingSettings;
  captureTarget?: CaptureTarget;
  recordTarget?: CaptureTarget;
  captureShortcut: string;
  theme: Theme;
  recordShortcut: string;
  saveDirectory: string;
  videoDirectory: string;
  saveScreenshots: boolean;
  captureCursor: boolean;
  recordAudio: boolean;
  systemAudioDevice?: string | null;
  microphoneDevice?: string | null;
  recordMicrophone: boolean;
  fps: number;
  playSound: boolean;
  showMagnifier: boolean;
  startWithWindows: boolean;
  captureDelaySeconds: number;
}

export interface ShortcutStatus {
  capture: boolean;
  record: boolean;
}

export const EVENTS = {
  overlayShow: "winshotx://overlay-show",
  overlayHide: "winshotx://overlay-hide",
  recordingTick: "winshotx://recording-tick",
  recordingStopped: "winshotx://recording-stopped",
  settingsChanged: "winshotx://settings-changed",
  settingsShown: "winshotx://settings-shown",
  overlayMode: "winshotx://overlay-mode",
  overlayTakeScreen: "winshotx://overlay-take-screen",
  countdown: "winshotx://countdown",
} as const;

export interface OverlayModeState {
  target?: CaptureTarget;
  mode: CaptureMode;
  fullScreen: boolean;
  /** Name of the recording preset just applied, so every screen shows it. */
  preset?: string;
}

export type TrayMenuAction =
  | "capture"
  | "record"
  | "folder"
  | "settings"
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
