import { useEffect, useRef, useState, type ReactNode } from "react";
import { Camera, Video, SlidersHorizontal, FolderOpen, Check } from "lucide-react";
import { getSettings, setSettings, shortcutStatus, pickDirectory, ffmpegInfo, testEncoder, openFolder, trayMenuAction } from "../../lib/ipc";
import type { Settings, EncodingSettings } from "../../lib/types";
import { aplicarTema } from "../../lib/tema";
import { Switch } from "../ui/Switch";
import { ShortcutField } from "./ShortcutField";
import { AudioSettings } from "./AudioSettings";

export const DEFAULT_ENCODING: EncodingSettings = {
  engine: "native", encoder: "libx264", quality: 85, bitrateMbps: 0,
  preset: "medium", gpu: "", extraArgs: "[]", ffmpegPath: "ffmpeg", rateControl: "crf", crf: 20,
};
const ENCODERS = [
  ["libx264", "H.264 · CPU (x264)"], ["libx265", "HEVC · CPU (x265)"],
  ["libaom-av1", "AV1 - CPU (libaom)"], ["libsvtav1", "AV1 · CPU (SVT-AV1)"], ["h264_nvenc", "H.264 · NVIDIA NVENC"],
  ["hevc_nvenc", "HEVC · NVIDIA NVENC"], ["av1_nvenc", "AV1 · NVIDIA NVENC"],
  ["h264_qsv", "H.264 · Intel Quick Sync"], ["hevc_qsv", "HEVC · Intel Quick Sync"],
  ["h264_amf", "H.264 · AMD AMF"], ["hevc_amf", "HEVC · AMD AMF"],
];

export function SettingsApp(_props: { onVerBienvenida?: () => void; arrancarTour?: boolean }) {
  const [settings, setLocal] = useState<Settings | null>(null);
  const latest = useRef<Settings | null>(null);
  const queue = useRef(Promise.resolve());
  const [tab, setTab] = useState("capture");
  const [error, setError] = useState("");
  const [saved, setSaved] = useState(false);
  const [ffmpeg, setFfmpeg] = useState<import("../../lib/ipc").FfmpegInfo | null>(null);
  const [testing, setTesting] = useState(false);
  const [testResult, setTestResult] = useState<{ key: string; ok: boolean; message: string } | null>(null);
  const testKey = JSON.stringify([settings?.encoding, settings?.fps]);
  const testKeyRef = useRef(testKey);
  testKeyRef.current = testKey;
  async function checkEncoder() {
    if (!latest.current || testing) return;
    const key = testKeyRef.current;
    const selected = { ...DEFAULT_ENCODING, ...latest.current.encoding };
    const fps = latest.current.fps;
    setTesting(true); setTestResult(null);
    try {
      const result = await testEncoder(selected, fps);
      if (testKeyRef.current === key) setTestResult({ key, ok: true, message: `Test passed: ${result.width} × ${result.height} at ${result.fps} fps with AAC audio. This checks encoder compatibility; full-resolution capture may run slower.` });
    } catch (e) {
      if (testKeyRef.current === key) setTestResult({ key, ok: false, message: String(e) });
    } finally { setTesting(false); }
  }
  const [argsError, setArgsError] = useState("");
  const [shortcuts, setShortcuts] = useState({ capture: true, record: true });
  function refreshShortcuts() {
    void shortcutStatus().then(status => { if (status) setShortcuts(status); }).catch(() => undefined);
  }
  useEffect(() => {
    void getSettings().then(s => { latest.current = s; setLocal(s); aplicarTema(s.theme); }).catch(e => setError(String(e)));
    refreshShortcuts();
  }, []);
  useEffect(() => {
    if (!settings) return;
    let closed = false;
    setFfmpeg(null);
    const timer = setTimeout(() => {
      void ffmpegInfo(settings.encoding?.ffmpegPath).then(value => { if (!closed) setFfmpeg(value); }).catch(e => { if (!closed) setFfmpeg({ available: false, version: "", encoders: [], error: String(e) }); });
    }, 250);
    return () => { closed = true; clearTimeout(timer); };
  }, [settings?.encoding?.ffmpegPath, !!settings]);
  function patch(change: Partial<Settings>) {
    if (!latest.current) return;
    const next = { ...latest.current, ...change, language: "en" as const, replayEnabled: false };
    latest.current = next;
    setLocal(next);
    setSaved(false);
    setError("");
    queue.current = queue.current.then(async () => {
      try {
        const result = await setSettings(next);
        refreshShortcuts();
        if (latest.current === next) { latest.current = result; setLocal(result); setSaved(true); setError(""); }
      } catch (e) { setError(String(e)); }
    });
  }
  function encoding(change: Partial<EncodingSettings>) {
    patch({ encoding: { ...DEFAULT_ENCODING, ...latest.current?.encoding, ...change } });
  }
  if (!settings) return <div className="settings-shell"><p role="status">{error || "Loading settings…"}</p></div>;
  const enc = { ...DEFAULT_ENCODING, ...settings.encoding };
  return <div className="settings-shell">
    <aside className="settings-sidebar">
      <div className="settings-brand"><Camera size={24} /><div>WinShotX</div></div>
      <nav aria-label="Settings">
        {[["capture", Camera, "Screenshots"], ["record", Video, "Recording"], ["general", SlidersHorizontal, "Preferences"]].map(([id, Icon, label]) => {
          const Symbol = Icon as typeof Camera;
          return <button key={String(id)} aria-current={tab === id ? "page" : undefined} onClick={() => setTab(String(id))}><Symbol size={18} />{String(label)}</button>;
        })}
      </nav>
      <div className="settings-sidebar-bottom">Version {__VERSION__}</div>
    </aside>
    <main className="settings-main">
      <header className="settings-page-header"><div><h1>{tab === "capture" ? "Screenshots" : tab === "record" ? "Recording" : "Preferences"}</h1><p>{tab === "capture" ? "Shortcuts and capture options." : tab === "record" ? "Recording and video encoding options." : "Files, appearance, and startup."}</p></div><span className="settings-save" role="status">{saved && <><Check size={14} /> Saved</>}</span></header>
      {error && <p className="settings-error" role="alert">{error}</p>}
      {tab === "capture" && <>
        <Card title="Capture" detail="Select a region, a window, or a full screen.">
          <Field label="Screenshot shortcut"><ShortcutField value={settings.captureShortcut} active={shortcuts.capture} onChange={v => patch({ captureShortcut: v })} /></Field>
          <Field label="Default selection"><TargetSelect value={settings.captureTarget ?? "rectangle"} onChange={v => patch({ captureTarget: v })} /></Field>
          <Field label="After selecting a region"><select aria-label="After selecting a region" value={settings.captureFlow} onChange={e => patch({ captureFlow: e.target.value as Settings["captureFlow"] })}><option value="toolbar">Show capture toolbar</option><option value="instant">Copy immediately</option></select></Field>
          <Field label="Capture delay"><select aria-label="Capture delay" value={settings.captureDelaySeconds} onChange={e => patch({ captureDelaySeconds: Number(e.target.value) })}>{[0, 2, 3, 5, 10].map(v => <option key={v} value={v}>{v ? `${v} seconds` : "No delay"}</option>)}</select></Field>
          <BooleanField label="Include the mouse cursor" value={settings.captureCursor} onChange={v => patch({ captureCursor: v })} />
          <BooleanField label="Show magnifier" value={settings.showMagnifier} onChange={v => patch({ showMagnifier: v })} />
          <BooleanField label="Hide desktop icons during capture" value={settings.hideDesktopIcons} onChange={v => patch({ hideDesktopIcons: v })} />
        </Card>
        <button className="settings-primary" onClick={() => void trayMenuAction("capture").catch(e => setError(String(e)))}><Camera size={16} /> Take a screenshot</button>
      </>}
      {tab === "record" && <>
        <Card title="Video capture" detail="Recording starts and stops with your shortcut. No background recording.">
          <Field label="Start / stop shortcut"><ShortcutField value={settings.recordShortcut} active={shortcuts.record} onChange={v => patch({ recordShortcut: v })} /></Field>
          <Field label="Default recording selection"><TargetSelect recording value={settings.recordTarget ?? "rectangle"} onChange={v => patch({ recordTarget: v })} /></Field>
          <Field label="Frame rate"><select aria-label="Frame rate" value={settings.fps} onChange={e => patch({ fps: Number(e.target.value) })}>{[15, 24, 25, 30, 50, 60, 90, 120].map(v => <option key={v} value={v}>{v} fps</option>)}</select></Field>
          {settings.fps > 60 && <p className="settings-note">{settings.fps} fps is a target. The source refresh rate, Windows compositor, encoder, and disk speed can limit the actual rate. The recording bar shows measured capture fps.</p>}
        </Card>
        <Card title="Audio sources" detail="Choose the devices to record and check their signal first."><AudioSettings settings={settings} patch={patch} /></Card>
        <Card title="Encoding" detail="Choose the encoder and quality for saved videos.">
          <Field label="Encoding engine"><select aria-label="Encoding engine" value={enc.engine} onChange={e => encoding({ engine: e.target.value as EncodingSettings["engine"] })}><option value="native">Built-in H.264</option><option value="ffmpeg">FFmpeg · custom encoder</option></select></Field>
          {enc.engine === "ffmpeg" && <>
            <Field label="FFmpeg executable" hint="Full path to ffmpeg.exe, or ffmpeg to search PATH."><input className="ffmpeg-path" aria-label="FFmpeg executable" defaultValue={enc.ffmpegPath ?? "ffmpeg"} title={enc.ffmpegPath} onBlur={e => encoding({ ffmpegPath: e.target.value.trim() || "ffmpeg" })} /></Field>
            <p className={ffmpeg && !ffmpeg.available ? "settings-error" : "settings-note"}>{ffmpeg ? (ffmpeg.available ? ffmpeg.version : ffmpeg.error ?? "FFmpeg was not found.") : "Checking FFmpeg..."}</p>
            <Field label="Rate control"><select aria-label="Rate control" value={enc.rateControl ?? "crf"} onChange={e => encoding({ rateControl: e.target.value as "crf" | "bitrate", bitrateMbps: e.target.value === "bitrate" ? (enc.bitrateMbps || 15) : 0 })}><option value="crf">Constant quality (CRF / CQ / QP)</option><option value="bitrate">Target bitrate</option></select></Field>
            {(enc.rateControl ?? "crf") === "crf" && <Field label="Quality value" hint="Lower values give higher quality and larger files. Start at 20."><input aria-label="CRF / CQ quality slider" type="range" min={0} max={51} value={enc.crf ?? 20} onChange={e => encoding({ crf: Number(e.target.value) })} /><input aria-label="CRF / CQ quality" type="number" min={0} max={51} value={enc.crf ?? 20} onChange={e => encoding({ crf: Number(e.target.value) })} /></Field>}
            <div className="audio-check-actions"><button className="settings-secondary" onClick={() => encoding({ engine: "ffmpeg", encoder: "libx264", preset: "medium", rateControl: "crf", crf: 20, bitrateMbps: 0, extraArgs: "[]", gpu: "" })}>Use balanced settings</button></div>
          </>}
          {enc.engine === "native" && <Field label="Video quality" hint="Higher quality uses more disk space. Used when target bitrate is 0."><input aria-label="Video quality" type="range" min={10} max={100} value={enc.quality} onChange={e => encoding({ quality: Number(e.target.value) })} /><output>{enc.quality}%</output></Field>}
          {(enc.engine === "native" || enc.rateControl === "bitrate") && <Field label="Target bitrate" hint="Enter a target in Mbps; 0 uses native quality-based encoding."><input aria-label="Target bitrate" type="number" min={0} max={200} step={0.5} value={enc.bitrateMbps} onChange={e => encoding({ bitrateMbps: Number(e.target.value) })} /><span>Mbps</span></Field>}
          {enc.engine === "ffmpeg" && <>
            <Field label="Video encoder"><select aria-label="Video encoder" value={enc.encoder} onChange={e => encoding({ encoder: e.target.value, preset: e.target.value.includes("nvenc") ? "p4" : /amf|svtav1|libaom/.test(e.target.value) ? "" : "medium", gpu: "" })}>{ENCODERS.map(([v,l]) => <option key={v} value={v} disabled={!!ffmpeg?.encoders.length && !ffmpeg.encoders.includes(v)}>{l}{ffmpeg?.encoders.length && !ffmpeg.encoders.includes(v) ? " - not in this FFmpeg build" : ""}</option>)}</select></Field>
            <Field label="Encoder preset" hint="FFmpeg preset name; leave blank for the encoder default."><input aria-label="Encoder preset" value={enc.preset} onChange={e => encoding({ preset: e.target.value })} /></Field>
            {enc.encoder.includes("nvenc") && <Field label="GPU device index" hint="Leave blank for automatic selection. NVIDIA indices start at 0."><input aria-label="GPU device index" placeholder="Automatic" value={enc.gpu} onChange={e => encoding({ gpu: e.target.value })} /></Field>}
            <p className="settings-note">The encoder must be supported by your FFmpeg build and hardware. An unavailable encoder reports an error.</p>
            <div className="encoder-check">
              <div className="audio-check-actions"><button className="settings-secondary" disabled={testing || !ffmpeg?.available || !!argsError} onClick={() => void checkEncoder()}>{testing ? "Testing encoder…" : "Test encoder"}</button><span>Generates a half-second 720p clip. No screen or microphone capture.</span></div>
              {testResult?.key === testKey ? <p role={testResult.ok ? "status" : "alert"} className={testResult.ok ? "settings-note" : "settings-error"}>{testResult.message}</p> : !testing && <p className="settings-note">Current settings have not been tested.</p>}
            </div>
            <details className="settings-advanced"><summary>Advanced FFmpeg arguments</summary><p>Add output arguments as a JSON array. They are passed directly to FFmpeg without a shell, after the generated encoding options.</p><textarea aria-label="Advanced FFmpeg arguments" defaultValue={enc.extraArgs} spellCheck={false} placeholder={'["-g", "120", "-bf", "2"]'} onBlur={e => { try { const value: unknown = JSON.parse(e.target.value); if (!Array.isArray(value) || !value.every(v => typeof v === "string")) throw Error(); setArgsError(""); encoding({ extraArgs: e.target.value }); } catch { setArgsError('Enter a JSON array of strings, for example ["-g", "120"].'); } }} />{argsError && <p role="alert" className="settings-error">{argsError}</p>}<p>Input, trimming, audio selection, and the output path are managed by WinShotX. Advanced arguments can override codec options.</p></details>
          </>}
        </Card>
        <button className="settings-primary" onClick={() => void trayMenuAction("record").catch(e => setError(String(e)))}><Video size={16} /> Select a region to record</button>
      </>}
      {tab === "general" && <Card title="Files & appearance">
        <Field label="Save folder" hint={settings.saveDirectory}><button onClick={() => void pickDirectory().then(v => v && patch({ saveDirectory: v })).catch(e => setError(String(e)))}><FolderOpen size={15} /> Choose folder</button><button onClick={() => void openFolder(settings.saveDirectory).catch(e => setError(String(e)))}>Open</button></Field>
        <Field label="Theme"><select aria-label="Theme" value={settings.theme} onChange={e => { const v = e.target.value as Settings["theme"]; aplicarTema(v); patch({ theme: v }); }}><option value="sistema">Follow Windows</option><option value="oscuro">Dark</option><option value="claro">Light</option></select></Field>
        <BooleanField label="Start with Windows" value={settings.startWithWindows} onChange={v => patch({ startWithWindows: v })} />
        <BooleanField label="Play capture sound" value={settings.playSound} onChange={v => patch({ playSound: v })} />
      </Card>}
    </main>
  </div>;
}
function Card({ title, detail, children }: { title: string; detail?: string; children: ReactNode }) { return <section className="settings-card"><header><h2>{title}</h2>{detail && <p>{detail}</p>}</header>{children}</section>; }
function Field({ label, hint, children }: { label: string; hint?: string; children: ReactNode }) { return <div className="settings-field"><div><span>{label}</span>{hint && <small>{hint}</small>}</div><div className="settings-control">{children}</div></div>; }
function BooleanField({ label, value, onChange }: { label: string; value: boolean; onChange: (v: boolean) => void }) { return <Field label={label}><Switch label={label} checked={value} onChange={onChange} /></Field>; }

function TargetSelect({ value, onChange, recording = false }: { value: import("../../lib/types").CaptureTarget; onChange: (v: import("../../lib/types").CaptureTarget) => void; recording?: boolean }) {
  return <select aria-label={recording ? "Default recording selection" : "Default selection"} value={value} onChange={e => onChange(e.target.value as typeof value)}><option value="rectangle">Rectangle</option><option value="window">Window</option><option value="screen">Full screen</option></select>;
}
