import { invoke } from "@tauri-apps/api/core";
import type {
  CacheStats,
  CapturaReciente,
  CaptureFlow,
  OverlayPayload,
  Rect,
  RecordOptions,
  Screen,
  SessionInfo,
  PrintScreenState,
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

export const capturaModerna = () => invoke<boolean>("captura_moderna");

export const cronoMarca = (etapa: string) =>
  invoke<void>("crono_marca", { etapa }).catch(() => undefined);

export const copyColor = (color: string) => invoke<void>("copy_color", { color });

export const copyPinned = (path: string) => invoke<void>("copy_pinned", { path });

export const savePinned = (path: string) => invoke<string>("save_pinned", { path });

export const pinnedText = (path: string) => invoke<void>("pinned_text", { path });

export const startRecording = (region: Rect, options: RecordOptions) =>
  invoke<SessionInfo>("start_recording", { region, options });

export const stopRecording = () => invoke<void>("stop_recording");

export const pauseRecording = (paused: boolean) =>
  invoke<void>("pause_recording", { paused });

export const cancelRecording = () => invoke<void>("cancel_recording");

export const ultimasCapturas = (cuantas: number) =>
  invoke<CapturaReciente[]>("ultimas_capturas", { cuantas });

export const copiarCaptura = (path: string) => invoke<void>("copiar_captura", { path });

export const ffmpegAvailable = (path?: string) => invoke<boolean>("ffmpeg_available", { path: path ?? null });

export const getSettings = () => invoke<Settings>("get_settings");

export const setSettings = (settings: Settings) =>
  invoke<Settings>("set_settings", { settings });

export const setCaptureFlow = (flow: CaptureFlow) =>
  invoke<void>("set_capture_flow", { flow });

export const openSettings = () => invoke<void>("open_settings");

export const pickDirectory = () => invoke<string | null>("pick_directory");

export const revealInExplorer = (path: string) =>
  invoke<void>("reveal_in_explorer", { path });


export const justUpdated = () => invoke<boolean>("just_updated");
export const isStoreBuild = () => invoke<boolean>("is_store_build");

export const shortcutStatus = () => invoke<ShortcutStatus>("shortcut_status");

export const printScreenState = () => invoke<PrintScreenState>("print_screen_state");

export const listScreens = () => invoke<Screen[]>("list_screens");

export const showScreenNumber = (screen: number) =>
  invoke<void>("show_screen_number", { screen });

export const usePrintScreen = (enabled: boolean) =>
  invoke<PrintScreenState>("use_print_screen", { enabled });

export const cacheStats = () => invoke<CacheStats>("cache_stats");

export const clearCache = () => invoke<CacheStats>("clear_cache");

export const openFolder = (path: string) => invoke<void>("open_folder", { path });

export const openUrl = (url: string) => invoke<void>("open_url", { url });

export const openWindowsApps = () => invoke<void>("open_windows_apps");

export const removeSnippingTool = () => invoke<boolean>("remove_snipping_tool");

export const useWinShiftS = (enabled: boolean) =>
  invoke<boolean>("use_win_shift_s", { enabled });

export const restartShell = () => invoke<ShortcutStatus>("restart_shell");

export const quitApp = () => invoke<void>("quit_app");

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
