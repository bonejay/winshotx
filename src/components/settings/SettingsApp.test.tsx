import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { SettingsApp, DEFAULT_ENCODING } from "./SettingsApp";
import type { Settings } from "../../lib/types";
import * as ipc from "../../lib/ipc";
vi.mock("../../lib/ipc", () => ({
  getSettings: vi.fn(), setSettings: vi.fn(async (s: Settings) => s),
  testEncoder: vi.fn(async () => ({ width: 1280, height: 720, fps: 60, bytes: 12000, elapsedMs: 100 })),
  ffmpegAvailable: vi.fn(async () => true), pickDirectory: vi.fn(),
  ffmpegInfo: vi.fn(async () => ({ available: true, version: "FFmpeg 7.1.1", encoders: ["libx264", "hevc_nvenc"], error: null })),
  listAudioDevices: vi.fn(async () => []), startAudioPreview: vi.fn(), stopAudioPreview: vi.fn(), audioPreviewLevels: vi.fn(),
  shortcutStatus: vi.fn(async () => ({ capture: true, edit: true, record: true })),
  openFolder: vi.fn(), trayMenuAction: vi.fn(async () => {}),
  ffmpegCommand: vi.fn(async () => "ffmpeg -f rawvideo"), applyPreset: vi.fn(),
}));
const AJUSTES: Settings = {
  presets: [],
  activePreset: "",
  captureShortcut: "Ctrl+Shift+2",
  theme: "sistema",
  recordShortcut: "Ctrl+Shift+5",
  saveDirectory: "C:\\Users\\prueba\\Pictures\\winshotx",
  videoDirectory: "C:\\Users\\prueba\\Videos\\winshotx",
  saveScreenshots: false,
  captureCursor: false,
  recordAudio: true,
  recordMicrophone: false,
  fps: 30,
  playSound: true,
  showMagnifier: true,
  startWithWindows: false,
  captureDelaySeconds: 0,
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
  it("persists quality and encoding preferences", async () => {
    await open(); fireEvent.click(screen.getByRole("button", { name: "Recording" }));

    fireEvent.change(screen.getByLabelText("Video encoder"), { target: { value: "hevc_nvenc" } });
    fireEvent.change(screen.getByLabelText("GPU device index"), { target: { value: "1" } });
    fireEvent.change(screen.getByLabelText("CRF / CQ quality slider"), { target: { value: "18" } });
    await waitFor(() => expect(ipc.setSettings).toHaveBeenLastCalledWith(expect.objectContaining({
      encoding: expect.objectContaining({ encoder: "hevc_nvenc", preset: "p4", gpu: "1", crf: 18 }),
    })));
  });
  it("rejects malformed advanced arguments without persisting them", async () => {
    await open(); fireEvent.click(screen.getByRole("button", { name: "Recording" }));

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
  it("saves a custom FFmpeg executable, explicit CRF, and a 120 fps target", async () => {
    await open(); fireEvent.click(screen.getByRole("button", { name: "Recording" }));
    fireEvent.change(screen.getByLabelText("Frame rate"), { target: { value: "120" } });

    fireEvent.change(screen.getByLabelText("CRF / CQ quality"), { target: { value: "18" } });
    const path = "C:\\Program Files (x86)\\SVP 4\\utils\\ffmpeg.exe";
    fireEvent.blur(screen.getByLabelText("FFmpeg executable"), { target: { value: path } });
    await waitFor(() => expect(ipc.setSettings).toHaveBeenLastCalledWith(expect.objectContaining({ fps: 120, encoding: expect.objectContaining({ ffmpegPath: path, rateControl: "crf", crf: 18 }) })));
    await waitFor(() => expect(ipc.ffmpegInfo).toHaveBeenCalledWith(path));
  });
});

it("tests only on request and invalidates the result after quality changes", async () => {
  await open(); fireEvent.click(screen.getByRole("button", { name: "Recording" }));

  expect(screen.queryByLabelText("Video quality")).toBeNull();
  expect(screen.queryByText("Open editor after recording")).toBeNull();
  await waitFor(() => expect(screen.getByRole("button", { name: "Test encoder" })).toBeEnabled());
  expect(ipc.testEncoder).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "Test encoder" }));
  expect(await screen.findByText(/Test passed:/)).toBeInTheDocument();
  fireEvent.change(screen.getByLabelText("CRF / CQ quality slider"), { target: { value: "19" } });
  expect(screen.getByLabelText("CRF / CQ quality")).toHaveValue(19);
  expect(screen.queryByText(/Test passed:/)).toBeNull();
  expect(screen.getByText("Current settings have not been tested.")).toBeInTheDocument();
  expect(ipc.testEncoder).toHaveBeenCalledTimes(1);
});
it("shows encoder failures with the exact reason", async () => {
  vi.mocked(ipc.testEncoder).mockRejectedValueOnce("No NVENC capable devices found");
  await open(); fireEvent.click(screen.getByRole("button", { name: "Recording" }));

  await waitFor(() => expect(screen.getByRole("button", { name: "Test encoder" })).toBeEnabled());
  fireEvent.click(screen.getByRole("button", { name: "Test encoder" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("No NVENC capable devices found");
});
it("saves screenshot and video selection defaults independently", async () => {
  await open();
  fireEvent.change(screen.getByLabelText("Default selection"), { target: { value: "window" } });
  fireEvent.click(screen.getByRole("button", { name: "Recording" }));
  fireEvent.change(screen.getByLabelText("Default recording selection"), { target: { value: "screen" } });
  await waitFor(() => expect(ipc.setSettings).toHaveBeenLastCalledWith(expect.objectContaining({ captureTarget: "window", recordTarget: "screen" })));
});
