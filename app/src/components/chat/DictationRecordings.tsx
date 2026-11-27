import { useCallback, useEffect, useRef, useState } from "react";
import type { OctopusDictationRecording, OctopusDictationRecordingState } from "../../electron";
import { DICTATION_RECORDINGS_CHANGED, dictationAudioBlob, dictationAudioRequest, dictationHost, dictationMessage, dictationResult, notifyDictationRecordingsChanged } from "../../clientDictationRecordings";
import { transcribeDictation } from "../../projectionClient";
import { copyText } from "../../runtimeFormat";
import { applyDictationDictionary, useClientPreferences } from "../../clientPreferences";
import { OcButton, OcMenuItem, OcSettingsRow } from "../../design-system/components";
import { OcDropdown } from "../../design-system/OcDropdown";
import { Glyph } from "../IconGlyphs";
import { ShellTooltip } from "../shell/ShellTooltip";
import "./dictationRecordings.css";

type PlaybackState = { id: string; status: "loading" | "playing" };

export function DictationRecordings({ recordingId = "", onShowAll }: { recordingId?: string; onShowAll?: () => void }) {
  const preferences = useClientPreferences();
  const [state, setState] = useState<OctopusDictationRecordingState | null>(null);
  const [error, setError] = useState("");
  const [action, setAction] = useState<{ id: string; type: "transcribe" | "download" | "delete" } | null>(null);
  const [playback, setPlayback] = useState<PlaybackState | null>(null);
  const alive = useRef(true);
  const requestRef = useRef("");
  const playbackTokenRef = useRef(0);
  const audioRef = useRef<{ id: string; audio: HTMLAudioElement; url: string } | null>(null);

  const stopPlayback = useCallback(() => {
    playbackTokenRef.current += 1;
    const current = audioRef.current;
    audioRef.current = null;
    if (current) {
      current.audio.onended = null;
      current.audio.onerror = null;
      try { current.audio.pause(); } catch { /* ignore */ }
      try { current.audio.src = ""; } catch { /* ignore */ }
      URL.revokeObjectURL(current.url);
    }
    if (alive.current) setPlayback(null);
  }, []);

  async function refresh() {
    try {
      const next = await dictationResult(dictationHost().readDictationRecordings());
      if (alive.current) setState(next);
    } catch (failure) { if (alive.current) setError(dictationMessage(failure)); }
  }

  useEffect(() => {
    alive.current = true;
    void refresh();
    const changed = () => void refresh();
    window.addEventListener(DICTATION_RECORDINGS_CHANGED, changed);
    return () => {
      alive.current = false;
      window.removeEventListener(DICTATION_RECORDINGS_CHANGED, changed);
      stopPlayback();
      if (requestRef.current) void dictationHost().cancelDictation(requestRef.current).catch(() => undefined);
    };
  }, [stopPlayback]);

  async function togglePlayback(entry: OctopusDictationRecording) {
    if (playback?.id === entry.id) {
      stopPlayback();
      return;
    }
    stopPlayback();
    const token = ++playbackTokenRef.current;
    setError("");
    setPlayback({ id: entry.id, status: "loading" });
    try {
      const { bytes } = await dictationResult(dictationHost().readDictationRecording(entry.id));
      if (!alive.current || playbackTokenRef.current !== token) return;
      const url = URL.createObjectURL(dictationAudioBlob(bytes));
      const audio = new Audio(url);
      audioRef.current = { id: entry.id, audio, url };
      audio.onended = () => {
        if (playbackTokenRef.current === token) stopPlayback();
      };
      audio.onerror = () => {
        if (playbackTokenRef.current !== token) return;
        stopPlayback();
        if (alive.current) setError("Couldn't play this recording");
      };
      await audio.play();
      if (!alive.current || playbackTokenRef.current !== token) {
        stopPlayback();
        return;
      }
      setPlayback({ id: entry.id, status: "playing" });
    } catch {
      if (playbackTokenRef.current !== token) return;
      stopPlayback();
      if (alive.current) setError("Couldn't play this recording");
    }
  }

  async function act(entry: OctopusDictationRecording, type: "transcribe" | "download" | "delete") {
    setAction({ id: entry.id, type });
    setError("");
    try {
      if (type === "delete") {
        if (playback?.id === entry.id || audioRef.current?.id === entry.id) stopPlayback();
        await dictationResult(dictationHost().deleteDictationRecording(entry.id));
      } else {
        const { bytes } = await dictationResult(dictationHost().readDictationRecording(entry.id));
        if (!alive.current) return;
        if (type === "download") {
          const url = URL.createObjectURL(dictationAudioBlob(bytes));
          const link = document.createElement("a");
          link.href = url;
          link.download = `dictation-${entry.id}.wav`;
          link.click();
          window.setTimeout(() => URL.revokeObjectURL(url), 1_000);
        } else {
          const request = dictationAudioRequest(new Uint8Array(bytes).buffer, entry.durationMs);
          requestRef.current = request.attemptId;
          const response = await dictationResult(transcribeDictation(request));
          if (!alive.current) return;
          const text = applyDictationDictionary(response.text, preferences.dictationDictionary).trim();
          if (!text) throw new Error("Didn't quite catch that. Please try again.");
          await dictationResult(dictationHost().updateDictationRecording({ id: entry.id, text, status: "transcribed" }));
        }
      }
      if (alive.current) { await refresh(); notifyDictationRecordingsChanged(); }
    } catch (failure) { if (alive.current) setError(dictationMessage(failure)); }
    finally { requestRef.current = ""; if (alive.current) setAction(null); }
  }

  const entries = recordingId ? state?.recordings.filter((entry) => entry.id === recordingId) : state?.recordings.slice(0, 20);
  return <div className="dictation-recordings">
    <OcSettingsRow label="Recent recordings" description="Your last 20 recordings are saved on this device" control={null} />
    {recordingId ? <OcButton onClick={onShowAll} size="sm" variant="ghost">Show all recordings</OcButton> : null}
    {error ? <p className="dictation-recordings-error" role="status">{error}</p> : null}
    {entries?.map((entry) => <DictationRecordingRow key={entry.id} entry={entry} selected={entry.id === recordingId}
      disabled={action !== null} transcribing={action?.id === entry.id && action.type === "transcribe"}
      playbackStatus={playback?.id === entry.id ? playback.status : "idle"}
      onTogglePlayback={() => void togglePlayback(entry)}
      onCopy={async () => {
        try { if (await copyText(entry.text)) return true; } catch {}
        setError("Couldn't copy dictation. Try again.");
        return false;
      }}
      onAction={(type) => void act(entry, type)} />)}
  </div>;
}

function DictationRecordingRow({ entry, selected, disabled, transcribing, playbackStatus, onTogglePlayback, onCopy, onAction }: {
  entry: OctopusDictationRecording; selected: boolean; disabled: boolean; transcribing: boolean;
  playbackStatus: "idle" | "loading" | "playing";
  onTogglePlayback: () => void;
  onCopy: () => Promise<boolean>; onAction: (type: "transcribe" | "download" | "delete") => void;
}) {
  const trigger = useRef<HTMLButtonElement>(null);
  const [menuOpen, setMenuOpen] = useState(false);
  const [copied, setCopied] = useState(false);
  const copyTimer = useRef<number | null>(null);
  useEffect(() => () => { if (copyTimer.current !== null) window.clearTimeout(copyTimer.current); }, []);
  const timestamp = new Date(entry.createdAtMs).toLocaleString(undefined, { day: "numeric", hour: "numeric", minute: "2-digit", month: "short" });
  const playing = playbackStatus !== "idle";
  const loadingPlayback = playbackStatus === "loading";
  const canPlay = entry.byteLength > 0 && (entry.status as string) !== "recording";
  const playbackLabel = playing
    ? `Stop playing recording from ${timestamp}`
    : `Play recording from ${timestamp}`;
  return <OcSettingsRow className="dictation-recording-row"
    label={<span className={`dictation-recording-text${selected ? " is-selected" : ""}`}>{entry.text || "Recording saved"}</span>}
    description={<time className="tabular-nums" dateTime={new Date(entry.createdAtMs).toISOString()}>{timestamp}</time>}
    control={<>
      {canPlay ? <ShellTooltip label={playbackLabel}><OcButton aria-busy={loadingPlayback || undefined} aria-label={playbackLabel} aria-pressed={playing} className="dictation-recording-playback" disabled={disabled} onClick={onTogglePlayback} size="sm" variant="ghost"><Glyph name={playing ? "stop" : "play"} size={14} /></OcButton></ShellTooltip> : null}
      {entry.text ? <ShellTooltip label="Copy dictation"><OcButton aria-label="Copy dictation" disabled={disabled} onClick={() => void onCopy().then((success) => {
        if (!success) return;
        setCopied(true);
        if (copyTimer.current !== null) window.clearTimeout(copyTimer.current);
        copyTimer.current = window.setTimeout(() => setCopied(false), 1_200);
      })} size="sm" variant="ghost"><Glyph name={copied ? "check" : "copy"} size={16} /></OcButton></ShellTooltip> : null}
      {!entry.text && entry.byteLength > 0 ? <OcButton aria-label="Retry transcription" disabled={disabled} onClick={() => onAction("transcribe")} size="sm">{transcribing ? "Transcribing…" : "Retry"}</OcButton> : null}
      {selected && entry.byteLength > 0 ? <OcButton disabled={disabled} onClick={() => onAction("download")} size="sm">Download audio</OcButton> : null}
      <ShellTooltip label="Recording actions"><button aria-label="Recording actions" aria-haspopup="menu" aria-expanded={menuOpen} className="dictation-recording-actions" disabled={disabled} onClick={() => setMenuOpen(true)} ref={trigger} type="button"><Glyph name="more" size={16} /></button></ShellTooltip>
      <OcDropdown align="end" anchorRef={trigger} contentWidth="menu" open={menuOpen} onOpenChange={setMenuOpen}>
        {entry.byteLength > 0 ? <OcMenuItem label="Download recording" leadingIcon="download" onClick={() => { setMenuOpen(false); onAction("download"); }} role="menuitem" /> : null}
        <OcMenuItem label="Delete recording" leadingIcon="trash" onClick={() => { setMenuOpen(false); onAction("delete"); }} role="menuitem" />
      </OcDropdown>
    </>} />;
}
