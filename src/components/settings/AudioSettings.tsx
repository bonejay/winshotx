import { useEffect, useState } from "react";
import { audioPreviewLevels, listAudioDevices, startAudioPreview, stopAudioPreview } from "../../lib/ipc";
import type { AudioDevice, AudioLevel, AudioLevels, Settings } from "../../lib/types";
import { Switch } from "../ui/Switch";

export function AudioSettings({ settings, patch }: { settings: Settings; patch: (value: Partial<Settings>) => void }) {
  const [devices, setDevices] = useState<AudioDevice[]>([]);
  const [checking, setChecking] = useState(false);
  const [levels, setLevels] = useState<AudioLevels | null>(null);
  const [error, setError] = useState("");
  function refresh() {
    setChecking(false);
    setLevels(null);
    void listAudioDevices().then(value => { setDevices(value ?? []); setError(""); }).catch(e => setError(String(e)));
  }
  useEffect(refresh, []);
  useEffect(() => {
    const hidden = () => { if (document.hidden) setChecking(false); };
    document.addEventListener("visibilitychange", hidden);
    return () => document.removeEventListener("visibilitychange", hidden);
  }, []);
  useEffect(() => {
    if (!checking) return;
    if (!settings.recordAudio && !settings.recordMicrophone) { setChecking(false); return; }
    let closed = false;
    let id: string | null = null;
    let timer: ReturnType<typeof setTimeout> | undefined;
    setLevels(null);
    setError("");
    async function poll() {
      if (closed || !id) return;
      try {
        const current = await audioPreviewLevels(id);
        if (closed) return;
        setLevels(current);
        if (!current.running) { setChecking(false); return; }
        timer = setTimeout(poll, 120);
      } catch (e) { if (!closed) { setError(String(e).includes("Audio level check has ended") ? "" : String(e)); setChecking(false); } }
    }
    void startAudioPreview(settings.recordAudio, settings.recordMicrophone, settings.systemAudioDevice ?? null, settings.microphoneDevice ?? null)
      .then(value => { id = value; if (closed) void stopAudioPreview(value).catch(() => undefined); else void poll(); })
      .catch(e => { if (!closed) { setError(String(e)); setChecking(false); } });
    return () => { closed = true; clearTimeout(timer); if (id) void stopAudioPreview(id).catch(() => undefined); };
  }, [checking, settings.recordAudio, settings.recordMicrophone, settings.systemAudioDevice, settings.microphoneDevice]);

  return <>
    {(["system", "microphone"] as const).map(source => {
      const system = source === "system";
      const name = system ? "System audio" : "Microphone";
      const enabled = system ? settings.recordAudio : settings.recordMicrophone;
      const device = system ? settings.systemAudioDevice : settings.microphoneDevice;
      const options = devices.filter(d => d.kind === (system ? "output" : "input"));
      const defaultDevice = options.find(d => d.isDefault);
      const unavailable = device && !options.some(d => d.id === device);
      return <div key={source} className="audio-source">
        <div className="settings-field"><div><span>{name}</span><small>{system ? "Capture sound from this playback device." : "Add your voice from this input device."}</small></div><Switch label={system ? "Record system audio" : "Record microphone"} checked={enabled} onChange={value => patch(system ? { recordAudio: value } : { recordMicrophone: value })} /></div>
        <div className="settings-field"><div><span>{system ? "Playback device" : "Input device"}</span>{!device && <small>{defaultDevice ? `Currently: ${defaultDevice.name}` : "No default device is available."}</small>}</div><div className="settings-control"><select aria-label={`${name} device`} value={device ?? ""} title={options.find(d => d.id === device)?.name ?? defaultDevice?.name ?? "Windows default"} onChange={e => patch(system ? { systemAudioDevice: e.target.value || null } : { microphoneDevice: e.target.value || null })}>
          <option value="">Windows default</option>
          {unavailable && <option value={device}>Unavailable saved device</option>}
          {options.map(d => <option key={d.id} value={d.id}>{d.name}{d.isDefault ? " (default)" : ""}</option>)}
        </select></div></div>
        <div className="audio-meter-row"><span>{name} level</span><Meter name={name} level={levels?.[source]} checking={checking} enabled={enabled} /></div>
      </div>;
    })}
    {error && <p className="settings-error" role="alert">{error}</p>}
    <div className="audio-check-actions"><button type="button" className="settings-secondary" disabled={!settings.recordAudio && !settings.recordMicrophone} aria-pressed={checking} onClick={() => setChecking(value => !value)}>{checking ? "Stop level check" : "Check levels"}</button><button type="button" className="settings-secondary" onClick={refresh}>Refresh devices</button></div>
    <p className="settings-note">The level check reads audio temporarily; nothing is saved. It stops when you leave this page or start recording. Windows default follows your selected Windows device.</p>
  </>;
}

function Meter({ name, level, checking, enabled }: { name: string; level?: AudioLevel; checking: boolean; enabled: boolean }) {
  if (!enabled) return <span className="audio-meter-caption">Not enabled</span>;
  if (level?.error) return <span className="audio-device-error" role="alert">{level.error}</span>;
  if (!checking) return <span className="audio-meter-caption">Use “Check levels” before recording</span>;
  if (!level || level.state === "connecting") return <span className="audio-meter-caption">Connecting…</span>;
  const db = Math.max(-60, Math.min(0, level.db));
  const percent = ((db + 60) / 60) * 100;
  const caption = level.peak >= 0.98 ? "Near clipping" : level.peak > 0.001 ? "Signal detected" : level.peak > 0 ? "Very quiet" : "No signal";
  return <div className="audio-meter"><div role="meter" aria-label={`${name} level`} aria-valuemin={-60} aria-valuemax={0} aria-valuenow={db} aria-valuetext={level.peak > 0 ? `${level.db.toFixed(1)} dBFS` : "No signal"} className="audio-meter-track"><span style={{ width: `${percent}%`, background: level.peak >= 0.98 ? "#e66d64" : "#5ab881" }} /></div><output>{level.peak > 0 ? `${level.db.toFixed(1)} dBFS` : "−∞ dBFS"}</output><span className="audio-meter-caption">{caption}</span></div>;
}
