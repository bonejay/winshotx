import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { AudioSettings } from "./AudioSettings";
import * as ipc from "../../lib/ipc";
import type { Settings, AudioLevels } from "../../lib/types";

vi.mock("../../lib/ipc", () => ({ listAudioDevices: vi.fn(), startAudioPreview: vi.fn(), audioPreviewLevels: vi.fn(), stopAudioPreview: vi.fn(async () => {}) }));
const settings = { recordAudio: true, recordMicrophone: false, systemAudioDevice: "speaker-1", microphoneDevice: null } as Settings;
const levels: AudioLevels = { id: "check-1", running: true, system: { peak: 0.5, db: -6.0206, state: "ready", error: null }, microphone: { peak: 0, db: -100, state: "disabled", error: null } };
beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(ipc.listAudioDevices).mockResolvedValue([
    { id: "speaker-1", name: "Speakers", kind: "output", isDefault: true },
    { id: "headphones", name: "Headphones", kind: "output", isDefault: false },
    { id: "mic-1", name: "USB microphone", kind: "input", isDefault: true },
  ]);
  vi.mocked(ipc.startAudioPreview).mockResolvedValue("check-1");
  vi.mocked(ipc.audioPreviewLevels).mockResolvedValue(levels);
});
describe("audio device setup", () => {
  it("lists playback and input devices separately and persists endpoint IDs", async () => {
    const patch = vi.fn(); render(<AudioSettings settings={settings} patch={patch} />);
    await screen.findByRole("option", { name: "Headphones" });
    fireEvent.change(screen.getByLabelText("System audio device"), { target: { value: "headphones" } });
    expect(patch).toHaveBeenCalledWith({ systemAudioDevice: "headphones" });
    fireEvent.change(screen.getByLabelText("Microphone device"), { target: { value: "mic-1" } });
    expect(patch).toHaveBeenCalledWith({ microphoneDevice: "mic-1" });
  });
  it("opens only enabled sources and displays measured signal in dBFS", async () => {
    const view = render(<AudioSettings settings={settings} patch={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "Check levels" }));
    const meter = await screen.findByRole("meter", { name: "System audio level" });
    expect(meter).toHaveAttribute("aria-valuenow", "-6.0206");
    expect(screen.getByText("Signal detected")).toBeInTheDocument();
    expect(ipc.startAudioPreview).toHaveBeenCalledWith(true, false, "speaker-1", null);
    view.unmount();
    await waitFor(() => expect(ipc.stopAudioPreview).toHaveBeenCalledWith("check-1"));
  });
  it("distinguishes silence from an unavailable device", async () => {
    vi.mocked(ipc.audioPreviewLevels).mockResolvedValue({ ...levels, system: { ...levels.system, peak: 0, db: -100 } });
    render(<AudioSettings settings={settings} patch={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "Check levels" }));
    expect(await screen.findByText("No signal")).toBeInTheDocument();
    expect(screen.queryByRole("alert")).toBeNull();
  });
  it("shows device errors rather than presenting a silent meter", async () => {
    vi.mocked(ipc.audioPreviewLevels).mockResolvedValue({ ...levels, system: { ...levels.system, state: "error", error: "Selected device was disconnected" } });
    render(<AudioSettings settings={settings} patch={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "Check levels" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("disconnected");
  });
  it("cleans up a monitor whose start completes after the page closes", async () => {
    let complete!: (id: string) => void;
    vi.mocked(ipc.startAudioPreview).mockReturnValue(new Promise(resolve => { complete = resolve; }));
    const view = render(<AudioSettings settings={settings} patch={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "Check levels" }));
    view.unmount(); complete("late-check");
    await waitFor(() => expect(ipc.stopAudioPreview).toHaveBeenCalledWith("late-check"));
  });
  it("ends normally when recording stops the level check", async () => {
    vi.mocked(ipc.audioPreviewLevels).mockRejectedValue("Audio level check has ended.");
    render(<AudioSettings settings={settings} patch={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "Check levels" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "Check levels" })).toBeInTheDocument());
    expect(screen.queryByRole("alert")).toBeNull();
  });
});
