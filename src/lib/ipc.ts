import { invoke } from "@tauri-apps/api/core";
import type {
  OverlayPayload,
  Rect,
  RecordOptions,
  SessionInfo,
  Settings,
  ShortcutStatus,
  TrayMenuAction,
  StillAction,
  StillResult,
} from "./types";

export const overlayBootstrap = (monitorId: number) =>
  invoke<OverlayPayload>("overlay_bootstrap", { monitorId });

export const captureStill = (region: Rect, action: StillAction) =>
  invoke<StillResult>("capture_still", { region, action });

export const captureAllScreens = (action: StillAction) =>
  invoke<StillResult>("capture_all_screens", { action });

export const freezePng = (monitorId: number) =>
  invoke<ArrayBuffer>("freeze_png", { monitorId });

export const freezeBytes = (monitorId: number) =>
  invoke<ArrayBuffer>("freeze_bytes", { monitorId });

export const overlayListo = (monitorId: number, generation: number) =>
  invoke<void>("overlay_listo", { monitorId, generation }).catch(() => undefined);

export const cancelCapture = () => invoke<void>("cancel_capture");

export const copyColor = (color: string) => invoke<void>("copy_color", { color });

export const startRecording = (region: Rect, options: RecordOptions) =>
  invoke<SessionInfo>("start_recording", { region, options });

export const stopRecording = () => invoke<void>("stop_recording");

export const pauseRecording = (paused: boolean) =>
  invoke<void>("pause_recording", { paused });

export const cancelRecording = () => invoke<void>("cancel_recording");

export const getSettings = () => invoke<Settings>("get_settings");

export const applyPreset = (name: string) => invoke<Settings>("apply_preset", { name });

export const ffmpegCommand = (options: import("./types").EncodingSettings, fps: number) =>
  invoke<string>("ffmpeg_command", { options, fps });

export const setSettings = (settings: Settings) =>
  invoke<Settings>("set_settings", { settings });

export const openSettings = () => invoke<void>("open_settings");

export const pickDirectory = () => invoke<string | null>("pick_directory");

export const shortcutStatus = () => invoke<ShortcutStatus>("shortcut_status");

export const openFolder = (path: string) => invoke<void>("open_folder", { path });

export const trayMenuAction = (action: TrayMenuAction) =>
  invoke<void>("tray_menu_action", { action });

export const listAudioDevices = () => invoke<import("./types").AudioDevice[]>("list_audio_devices");
export const startAudioPreview = (system: boolean, microphone: boolean, systemDevice: string | null, microphoneDevice: string | null) =>
  invoke<string>("start_audio_preview", { system, microphone, systemDevice, microphoneDevice });
export const audioPreviewLevels = (id: string) => invoke<import("./types").AudioLevels>("audio_preview_levels", { id });
export const stopAudioPreview = (id: string) => invoke<void>("stop_audio_preview", { id });

export interface FfmpegInfo { available: boolean; version: string; encoders: string[]; error: string | null; }
export const ffmpegInfo = (path?: string) => invoke<FfmpegInfo>("ffmpeg_info", { path: path ?? null });

export interface EncoderCheck { width: number; height: number; fps: number; bytes: number; elapsedMs: number; }
export const testEncoder = (options: import("./types").EncodingSettings, fps: number) => invoke<EncoderCheck>("test_encoder", { options, fps });
