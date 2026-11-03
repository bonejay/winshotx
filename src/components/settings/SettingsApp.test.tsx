import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { SettingsApp, DEFAULT_ENCODING } from "./SettingsApp";
import type { Settings } from "../../lib/types";
import * as ipc from "../../lib/ipc";
vi.mock("../../lib/ipc", () => ({
  getSettings: vi.fn(), setSettings: vi.fn(async (s: Settings) => s),
  ffmpegAvailable: vi.fn(async () => true), pickDirectory: vi.fn(),
  shortcutStatus: vi.fn(async () => ({ capture: true, edit: true, record: true })),
  openFolder: vi.fn(), trayMenuAction: vi.fn(async () => {}),
}));
const AJUSTES: Settings = {
  captureShortcut: "Ctrl+Shift+2",
  editShortcut: "Super+Shift+KeyL",
  theme: "sistema",
  language: "sistema",
  recordShortcut: "Ctrl+Shift+5",
  replayShortcut: "Ctrl+Shift+6",
  saveDirectory: "C:\\Users\\prueba\\Pictures\\winshotx",
  copyAfterCapture: true,
  openEditorAfterRecording: true,
  captureCursor: false,
  recordAudio: true,
  recordMicrophone: false,
  highlightClicks: false,
  highlightKeys: false,
  fps: 30,
  replayEnabled: false,
  replaySeconds: 30,
  replayScreen: null,
  replayFps: 15,
  replayHeight: 0,
  playSound: true,
  showMagnifier: true,
  startWithWindows: false,
  captureDelaySeconds: 0,
  hideDesktopIcons: false,
  captureFlow: "toolbar",
  printScreenCapture: false,
  takeWinShiftS: false,
  onboarded: true,
  snippingKeyRestore: null,
  disabledHotkeysRestore: null,
};
beforeEach(() => { vi.clearAllMocks(); vi.mocked(ipc.getSettings).mockResolvedValue({ ...AJUSTES, encoding: { ...DEFAULT_ENCODING } }); });
async function open() { render(<SettingsApp />); await screen.findByRole("heading", { name: "Screenshots" }); }
describe("focused settings", () => {
  it("shows only screenshot, recording and preferences pages", async () => {
    await open();
    expect(screen.getByRole("button", { name: "Recording" })).toBeInTheDocument();
    expect(screen.queryByText(/GIF|replay|always record/i)).toBeNull();
    expect(ipc.getSettings).toHaveBeenCalled();
  });
  it("persists quality and encoding preferences while disabling old background settings", async () => {
    await open(); fireEvent.click(screen.getByRole("button", { name: "Recording" }));
    fireEvent.change(screen.getByLabelText("Encoding engine"), { target: { value: "ffmpeg" } });
    fireEvent.change(screen.getByLabelText("Video encoder"), { target: { value: "hevc_nvenc" } });
    fireEvent.change(screen.getByLabelText("GPU device index"), { target: { value: "1" } });
    fireEvent.change(screen.getByLabelText("Capture quality"), { target: { value: "95" } });
    await waitFor(() => expect(ipc.setSettings).toHaveBeenLastCalledWith(expect.objectContaining({
      replayEnabled: false, language: "en", encoding: expect.objectContaining({ encoder: "hevc_nvenc", preset: "p4", gpu: "1", quality: 95 }),
    })));
  });
  it("rejects malformed advanced arguments without persisting them", async () => {
    await open(); fireEvent.click(screen.getByRole("button", { name: "Recording" }));
    fireEvent.change(screen.getByLabelText("Encoding engine"), { target: { value: "ffmpeg" } });
    fireEvent.blur(screen.getByLabelText("Advanced FFmpeg arguments"), { target: { value: "-g 120" } });
    expect(screen.getByRole("alert")).toHaveTextContent("JSON array");
    fireEvent.blur(screen.getByLabelText("Advanced FFmpeg arguments"), { target: { value: '["-g", "120"]' } });
    await waitFor(() => expect(ipc.setSettings).toHaveBeenLastCalledWith(expect.objectContaining({ encoding: expect.objectContaining({ extraArgs: '["-g", "120"]' }) })));
  });
  it("reports persistence failures", async () => {
    vi.mocked(ipc.setSettings).mockRejectedValueOnce("Could not save settings");
    await open(); fireEvent.click(screen.getByRole("switch", { name: "Show magnifier" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Could not save settings");
  });
});
