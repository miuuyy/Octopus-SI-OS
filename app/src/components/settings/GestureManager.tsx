import { useEffect, useRef, useState } from "react";
import { clientCommands, type ClientCommandId } from "../../clientActions";
import { useCustomGestures, customGestureCommandLabel, type CustomGesture } from "../../customGestures";
import type { OctopusClientAccessProfile } from "../../electron";
import type { FingerCurls } from "../../airHandEngine";
import { cancelGestureTrace, revealGestureTrace, startGestureTrace, useGestureTraceStatus, type GestureTraceStatus } from "../../gestureTraceRecorder";
import { GESTURE_GUIDE, type GestureGuideId } from "../gestures/gestureGuide";
import { GestureIllustration } from "../gestures/GestureIllustration";
import { claimGesturePreview, GestureLiveStatus, gesturePreviewNote } from "../gestures/GestureLivePreview";
import { GestureSimulateButtons } from "../gestures/GestureSimulateButtons";
import {
  currentGestureState,
  readCameraPermission,
  refreshGestureCameras,
  requestCameraPermission,
  startGestureRecognition,
  useGestureDevices,
  useGestureRecognitionCoarseState,
  type CameraPermission,
} from "../../gestureRecognition";
import { airHandGestures, defaultAirHandGestureCommand, useGestureShortcutPreferences, useSetGestureShortcutPreferences, type AirHandGestureId, type GestureCommandId } from "../../gestureShortcuts";
import { OcButton, OcCard, OcEmptyState, OcGroup, OcIconButton, OcSelect, OcSettingsRow, OcStatus, OcSwitch, OcTextField } from "../../design-system/components";
import { appToast } from "../../toasts";

const CURL_KEYS: readonly (keyof FingerCurls)[] = ["thumb", "index", "middle", "ring", "little"];
const RECORD_COUNTDOWN_STEPS = 3;
const RECORD_COUNTDOWN_STEP_MS = 700;
const RECORD_CAPTURE_MS = 900;
const RECORD_SAMPLE_MS = 40;
const RECORD_MIN_SAMPLES = 6;
const GESTURE_MIN_CAMERA_FPS = 15;
const SPATIAL_GESTURES: readonly { step: GestureGuideId; action: string; spatial: boolean }[] = [
  { step: "point", action: "Moves focus", spatial: false },
  { step: "pinch", action: "Presses the focused control", spatial: false },
  { step: "carry", action: "Moves the focused widget", spatial: true },
  { step: "throw", action: "Hides the carried widget", spatial: true },
  { step: "resize", action: "Resizes the focused widget", spatial: true },
];
const TRACE_DURATIONS: readonly { value: string; label: string }[] = [
  { value: "5000", label: "5 seconds" },
  { value: "10000", label: "10 seconds" },
  { value: "20000", label: "20 seconds" },
  { value: "30000", label: "30 seconds" },
];
const CURL_LABELS: Record<keyof FingerCurls, string> = {
  thumb: "Thumb",
  index: "Index",
  middle: "Middle",
  ring: "Ring",
  little: "Little",
};

export function GestureManager({ accessProfile }: { accessProfile: OctopusClientAccessProfile }) {
  const preferences = useGestureShortcutPreferences(accessProfile);
  const setPreferences = useSetGestureShortcutPreferences(accessProfile);
  const devicesView = useGestureDevices();
  const custom = useCustomGestures(accessProfile);
  const [permission, setPermission] = useState<CameraPermission | null>(null);
  const [requesting, setRequesting] = useState(false);
  const [recording, setRecording] = useState(false);
  const [editing, setEditing] = useState<string | null>(null);

  const inlineCanvasRef = useRef<HTMLCanvasElement>(null);
  const pipCanvasRef = useRef<HTMLCanvasElement>(null);
  const previewFrameRef = useRef<HTMLDivElement>(null);
  const [pip, setPip] = useState(false);

  useEffect(() => {
    let disposed = false;
    void readCameraPermission().then((value) => {
      if (!disposed) setPermission(value);
    });
    return () => {
      disposed = true;
    };
  }, []);

  useEffect(() => {
    if (devicesView.phase === "running") void refreshGestureCameras();
  }, [devicesView.phase]);

  const granted = permission?.granted ?? false;
  const cameraLive = devicesView.phase === "running";

  const setMapping = (gesture: AirHandGestureId, command: GestureCommandId) => {
    setPreferences((current) => ({ mappings: { ...current.mappings, [gesture]: command } }));
  };
  const removeBuiltin = (gesture: AirHandGestureId) => {
    const previousMapping = preferences.mappings[gesture];
    const label = airHandGestures.find((candidate) => candidate.id === gesture)?.label ?? gesture;
    setPreferences((current) => ({
      removedBuiltins: [...new Set([...current.removedBuiltins, gesture])],
      mappings: { ...current.mappings, [gesture]: "none" },
    }));
    appToast.info(`Removed ${label}`, 8, {
      dedupeKey: `builtin-gesture-remove:${accessProfile}:${gesture}`,
      action: {
        label: "Undo",
        onSelect: () => setPreferences((current) => current.removedBuiltins.includes(gesture) ? {
          removedBuiltins: current.removedBuiltins.filter((id) => id !== gesture),
          mappings: { ...current.mappings, [gesture]: previousMapping },
        } : {}),
      },
    });
  };
  const restoreBuiltin = (gesture: AirHandGestureId) => {
    setPreferences((current) => ({
      removedBuiltins: current.removedBuiltins.filter((id) => id !== gesture),
      mappings: { ...current.mappings, [gesture]: defaultAirHandGestureCommand(gesture) },
    }));
  };
  const visibleBuiltins = airHandGestures.filter((gesture) => !preferences.removedBuiltins.includes(gesture.id));
  const removedBuiltins = airHandGestures.filter((gesture) => preferences.removedBuiltins.includes(gesture.id));

  useEffect(() => {
    if (!preferences.enabled) {
      setPip(false);
      return;
    }
    const frame = previewFrameRef.current;
    if (!frame) return;
    const observer = new IntersectionObserver(
      ([entry]) => setPip(!entry.isIntersecting),
      { threshold: 0.35 },
    );
    observer.observe(frame);
    return () => observer.disconnect();
  }, [preferences.enabled]);

  useEffect(() => {
    const canvas = pip ? pipCanvasRef.current : inlineCanvasRef.current;
    if (!preferences.enabled || !canvas) return;
    return claimGesturePreview(canvas);
  }, [pip, preferences.enabled]);

  const requestPermission = async () => {
    setRequesting(true);
    try {
      const result = await requestCameraPermission();
      setPermission(result);
      if (result.granted && preferences.enabled) {
        void startGestureRecognition({
          cameraDeviceId: preferences.cameraDeviceId,
          mirrorX: preferences.mirrorX,
          sensitivity: preferences.sensitivity,
        });
      }
    } finally {
      setRequesting(false);
    }
  };

  return (
    <div className="gesture-manager">
      <OcCard>
        <OcSettingsRow
          control={<OcSwitch checked={preferences.enabled} label="Enable gesture control" onCheckedChange={(enabled) => setPreferences({ enabled })} />}
          description="Point to move focus, pinch to select, and use both hands to carry and resize Home widgets. Gestures run client commands; they never send anything to a task on their own."
          label="Gesture control"
        />
        <OcSettingsRow
          control={<OcButton onClick={() => setPreferences({ enabled: true, tourCompleted: false })} size="sm" variant="secondary">Take the tour</OcButton>}
          description="A short walkthrough of pointing, pinching, and swipes."
          label="Getting started"
        />
      </OcCard>

      <OcGroup title="Camera">
        <OcCard>
          <OcSettingsRow
            control={granted ? (
              <OcStatus label="Allowed" tone="success" />
            ) : (
              <div className="settings-inline-actions">
                <OcStatus label={permissionLabel(permission)} tone={permission?.status === "denied" ? "danger" : "neutral"} />
                <OcButton disabled={requesting || permission?.status === "denied"} onClick={() => void requestPermission()} size="sm">
                  {requesting ? "Requesting…" : "Allow camera access"}
                </OcButton>
              </div>
            )}
            description={permission?.status === "denied"
              ? "Camera access was denied. Enable it for Octopus in System Settings → Privacy & Security → Camera."
              : "Frames stay in memory on this device, only long enough to find your hands."}
            label="Camera access"
          />
          <div className="gesture-camera-setting">
            <div className="gesture-camera-head">
              <div className="gesture-camera-copy">
                <span className="gesture-camera-label">Camera</span>
                <span className="gesture-camera-detail">Choose which camera tracks your hands.</span>
              </div>
              <div className="settings-inline-actions">
                <OcSelect<string>
                  disabled={!granted}
                  label="Gesture camera"
                  onChange={(cameraDeviceId) => setPreferences({ cameraDeviceId })}
                  options={[
                    { value: "", label: "Automatic" },
                    ...devicesView.devices.map((device) => ({ value: device.deviceId, label: device.label })),
                  ]}
                  value={preferences.cameraDeviceId}
                />
                <OcIconButton disabled={!granted} glyph="refresh" label="Refresh cameras" onClick={() => void refreshGestureCameras()} variant="ghost" />
              </div>
            </div>
            {preferences.enabled ? (
              <div className="gesture-preview-inline" ref={previewFrameRef}>
                <canvas ref={inlineCanvasRef} />
                <GestureLiveStatus cameraLive={cameraLive} idleNote={gesturePreviewNote(preferences.enabled, granted, devicesView.phase)} mappings={preferences.mappings} />
                {!cameraLive ? (
                  <span className="gesture-preview-note">{gesturePreviewNote(preferences.enabled, granted, devicesView.phase)}</span>
                ) : null}
              </div>
            ) : null}
            {preferences.enabled && cameraLive ? <CameraRateNote /> : null}
          </div>
        </OcCard>
      </OcGroup>

      <OcGroup title="Options">
        <OcCard>
          <OcSettingsRow
            control={(
              <OcSelect
                label="Gesture response"
                onChange={(sensitivity) => setPreferences({ sensitivity })}
                options={[
                  { value: "responsive", label: "Responsive" },
                  { value: "balanced", label: "Balanced" },
                  { value: "deliberate", label: "Deliberate" },
                ]}
                value={preferences.sensitivity}
              />
            )}
            description="Controls how far the hand travels across the window, how long a fist is held, and the distance required for a swipe."
            label="Response"
          />
          <OcSettingsRow
            control={<OcSwitch checked={preferences.mirrorX} label="Mirror horizontal movement" onCheckedChange={(mirrorX) => setPreferences({ mirrorX })} />}
            description="Matches the direction you see in a mirrored camera preview."
            label="Mirroring"
          />
          <OcSettingsRow
            control={<OcSwitch checked={preferences.spatialControl} label="Control Home widgets" onCheckedChange={(spatialControl) => setPreferences({ spatialControl })} />}
            description="Carry, resize, and throw away the focused Home widget by hand. Focus and selection work without it."
            label="Spatial widget control"
          />
        </OcCard>
      </OcGroup>

      <OcGroup
        description="Built in and the same on every screen. The ring shows which control has focus."
        title="Focus and widgets"
      >
        <OcCard>
          {SPATIAL_GESTURES.map((gesture) => (
            <OcSettingsRow
              control={<span className="gesture-action-label">{gesture.spatial && !preferences.spatialControl ? "Off" : gesture.action}</span>}
              description={GESTURE_GUIDE[gesture.step].sentence}
              key={gesture.step}
              label={GESTURE_GUIDE[gesture.step].title}
              leading={<GestureIllustration gesture={GESTURE_GUIDE[gesture.step].art} />}
            />
          ))}
        </OcCard>
      </OcGroup>

      <OcGroup
        actions={<OcButton disabled={!cameraLive || recording} onClick={() => setRecording(true)} size="sm" variant="secondary">Record gesture</OcButton>}
        description="Each gesture points at a client command. Rebind a default or record your own pose."
        title="Command gestures"
      >
        <OcCard>
          {recording ? (
            <GestureRecorder
              onCancel={() => setRecording(false)}
              onMissed={() => {
                setRecording(false);
                appToast.warning("No hand was visible. Keep your hand in the camera preview and record again.");
              }}
              onCaptured={(curls) => {
                setRecording(false);
                const created = custom.add({ name: `Gesture ${custom.gestures.length + 1}`, definition: "", command: "none", curls });
                setEditing(created.id);
              }}
            />
          ) : null}
          {visibleBuiltins.map((gesture) => (
            <OcSettingsRow
              control={(
                <div className="settings-inline-actions">
                  <OcSelect<GestureCommandId>
                    label={`${gesture.label} command`}
                    onChange={(command) => setMapping(gesture.id, command)}
                    options={[
                      { value: "none", label: "No action" },
                      ...clientCommands.map((command) => ({ value: command.id, label: command.label })),
                    ]}
                    value={preferences.mappings[gesture.id]}
                  />
                  <OcIconButton glyph="trash" label={`Delete ${gesture.label}`} onClick={() => removeBuiltin(gesture.id)} variant="ghost" />
                </div>
              )}
              description={gesture.detail}
              key={gesture.id}
              label={gesture.label}
              leading={<GestureIllustration gesture={gesture.id} />}
            />
          ))}
          {custom.gestures.map((gesture) => (
            <CustomGestureRow
              editing={editing === gesture.id}
              gesture={gesture}
              key={gesture.id}
              onEditToggle={() => setEditing(editing === gesture.id ? null : gesture.id)}
              onRemoved={() => {
                if (editing === gesture.id) setEditing(null);
                const removed = custom.remove(gesture.id);
                if (!removed) return;
                appToast.info(`Removed ${gesture.name}`, 8, {
                  action: { label: "Undo", onSelect: () => custom.restore(removed.gesture, removed.index) },
                  dedupeKey: `custom-gesture-remove:${gesture.id}`,
                });
              }}
              onUpdate={(patch) => custom.update(gesture.id, patch)}
            />
          ))}
          {visibleBuiltins.length === 0 && custom.gestures.length === 0 && !recording ? (
            <OcEmptyState className="min-h-0 py-6" detail="Record one, or restore a default below." size="compact" title="No gestures" />
          ) : null}
          {removedBuiltins.length > 0 ? (
            <div className="gesture-restore-row">
              <span>Removed defaults</span>
              <div className="settings-inline-actions">
                {removedBuiltins.map((gesture) => (
                  <OcButton key={gesture.id} onClick={() => restoreBuiltin(gesture.id)} size="sm" variant="secondary">＋ {gesture.label}</OcButton>
                ))}
              </div>
            </div>
          ) : null}
        </OcCard>
      </OcGroup>

      {accessProfile === "live" ? (
        <OcGroup
          description="When a gesture misbehaves, record it. The file can be replayed against the recognizer to tune it to your hands, camera, and light."
          title="Diagnostics"
        >
          <GestureTracePanel cameraLive={cameraLive} />
        </OcGroup>
      ) : null}

      {accessProfile === "lab" ? (
        <OcGroup description="Emit isolated synthetic focus and command gestures without the camera." title="Simulate">
          <OcCard>
            <GestureSimulateButtons disabled={!preferences.enabled} />
          </OcCard>
        </OcGroup>
      ) : null}

      {pip && preferences.enabled ? (
        <div className="gesture-preview-pip">
          <canvas ref={pipCanvasRef} />
          <GestureLiveStatus cameraLive={cameraLive} idleNote={gesturePreviewNote(preferences.enabled, granted, devicesView.phase)} mappings={preferences.mappings} />
        </div>
      ) : null}
    </div>
  );
}

function GestureTracePanel({ cameraLive }: { cameraLive: boolean }) {
  const status = useGestureTraceStatus();
  const [duration, setDuration] = useState("10000");
  const [note, setNote] = useState("");
  const [now, setNow] = useState(() => Date.now());
  const active = status.phase === "countdown" || status.phase === "recording" || status.phase === "saving";
  useEffect(() => {
    if (status.phase !== "countdown" && status.phase !== "recording") return;
    setNow(Date.now());
    const timer = window.setInterval(() => setNow(Date.now()), 250);
    return () => window.clearInterval(timer);
  }, [status.phase]);
  return (
    <OcCard>
      <OcSettingsRow
        control={(
          <div className="settings-inline-actions">
            <OcSelect<string> disabled={active} label="Trace length" onChange={setDuration} options={TRACE_DURATIONS} value={duration} />
            {active ? (
              <OcButton onClick={cancelGestureTrace} size="sm" variant="secondary">Cancel</OcButton>
            ) : (
              <OcButton disabled={!cameraLive} onClick={() => startGestureTrace(Number(duration), note.trim())} size="sm">Record gesture trace</OcButton>
            )}
          </div>
        )}
        description="Saves the raw landmarks of both hands and every recognizer decision, frame by frame, in this Home's client folder. No video is stored. Recording starts after a 3-second countdown and continues if you leave Settings."
        label="Record gesture trace"
      />
      <div className="gesture-trace">
        <OcTextField
          disabled={active}
          label="What you will do"
          onChange={(event) => setNote(event.currentTarget.value)}
          placeholder="For example: five pinches on sidebar items, two were missed"
          showLabel
          value={note}
        />
        <GestureTraceStatusLine cameraLive={cameraLive} now={now} status={status} />
      </div>
    </OcCard>
  );
}

function GestureTraceStatusLine({ cameraLive, now, status }: { cameraLive: boolean; now: number; status: GestureTraceStatus }) {
  if (status.phase === "countdown") {
    return <span className="gesture-trace-status" role="status">Starting in {Math.max(1, Math.ceil((status.startsAt - now) / 1000))} s. Get your hands ready.</span>;
  }
  if (status.phase === "recording") {
    return <span className="gesture-trace-status is-live" role="status">Recording… {Math.max(0, Math.ceil((status.endsAt - now) / 1000))} s left. Do the gesture that misbehaves.</span>;
  }
  if (status.phase === "saving") return <span className="gesture-trace-status" role="status">Saving…</span>;
  if (status.phase === "saved") {
    return (
      <span className="gesture-trace-status" role="status">
        Saved {status.filename} ({status.frames} frames).
        <OcButton
          onClick={() => {
            void revealGestureTrace(status.path).catch((error: unknown) => appToast.warning(error instanceof Error ? error.message : String(error)));
          }}
          size="sm"
          variant="secondary"
        >
          Reveal in Finder
        </OcButton>
      </span>
    );
  }
  if (status.phase === "error") return <span className="gesture-trace-status is-error" role="status">{status.message}</span>;
  if (!cameraLive) return <span className="gesture-trace-status">Turn on gesture control and allow the camera to record a trace.</span>;
  return null;
}

function CameraRateNote() {
  const { cameraFps } = useGestureRecognitionCoarseState();
  if (cameraFps == null || cameraFps >= GESTURE_MIN_CAMERA_FPS) return null;
  return (
    <span className="gesture-camera-rate" role="status">
      The camera is delivering {cameraFps} frames per second; steady focus, pinches, and swipes need at least {GESTURE_MIN_CAMERA_FPS}. More light usually brings the rate back, and a phone used as a camera slows down when it is hot.
    </span>
  );
}

function GestureRecorder({
  onCancel,
  onCaptured,
  onMissed,
}: {
  onCancel: () => void;
  onCaptured: (curls: FingerCurls) => void;
  onMissed: () => void;
}) {
  const [countdown, setCountdown] = useState(RECORD_COUNTDOWN_STEPS);
  const recognition = useGestureRecognitionCoarseState();
  const missedRef = useRef(onMissed);
  missedRef.current = onMissed;
  const capturedRef = useRef(onCaptured);
  capturedRef.current = onCaptured;

  useEffect(() => {
    const samples: FingerCurls[] = [];
    let sampler = 0;
    const ticker = window.setInterval(() => setCountdown((value) => Math.max(0, value - 1)), RECORD_COUNTDOWN_STEP_MS);
    const capture = window.setTimeout(() => {
      window.clearInterval(ticker);
      setCountdown(0);
      sampler = window.setInterval(() => {
        const curls = currentGestureState().hands[0]?.curls;
        if (curls) samples.push(curls);
      }, RECORD_SAMPLE_MS);
    }, RECORD_COUNTDOWN_STEPS * RECORD_COUNTDOWN_STEP_MS);
    const done = window.setTimeout(() => {
      window.clearInterval(sampler);
      const captured = steadyCurls(samples);
      if (captured) capturedRef.current(captured);
      else missedRef.current();
    }, RECORD_COUNTDOWN_STEPS * RECORD_COUNTDOWN_STEP_MS + RECORD_CAPTURE_MS);
    return () => {
      window.clearInterval(ticker);
      window.clearInterval(sampler);
      window.clearTimeout(capture);
      window.clearTimeout(done);
    };
  }, []);

  return (
    <div className="gesture-recorder">
      <div className="gesture-recorder-copy">
        <strong>Hold the pose</strong>
        <span>{countdown > 0 ? `Capturing in ${countdown}…` : "Capturing…"}</span>
      </div>
      <div className="gesture-recorder-live">
        {recognition.hands[0] ? <CurlBars curls={recognition.hands[0].curls} /> : <span className="gesture-preview-note">Show one hand to the camera</span>}
      </div>
      <OcButton onClick={onCancel} size="sm" variant="secondary">Cancel</OcButton>
    </div>
  );
}

function CustomGestureRow({
  editing,
  gesture,
  onEditToggle,
  onRemoved,
  onUpdate,
}: {
  editing: boolean;
  gesture: CustomGesture;
  onEditToggle: () => void;
  onRemoved: () => void;
  onUpdate: (patch: Partial<Omit<CustomGesture, "id">>) => void;
}) {
  return (
    <div className={`gesture-custom-row${editing ? " is-editing" : ""}`}>
      <div className="gesture-custom-main">
        <div className="gesture-custom-pose"><CurlBars curls={gesture.curls} /></div>
        <div className="gesture-custom-copy">
          <span className="gesture-custom-name">{gesture.name}</span>
          <span className="gesture-custom-command">{customGestureCommandLabel(gesture.command)}</span>
          {gesture.definition ? <span className="gesture-custom-definition">{gesture.definition}</span> : null}
        </div>
        <div className="settings-inline-actions">
          <OcButton onClick={onEditToggle} size="sm" variant="secondary">{editing ? "Done" : "Edit"}</OcButton>
          <OcButton onClick={onRemoved} size="sm" variant="secondary" tone="danger">Delete</OcButton>
        </div>
      </div>
      {editing ? (
        <div className="gesture-custom-edit">
          <OcTextField label="Name" onChange={(event) => onUpdate({ name: event.currentTarget.value })} showLabel value={gesture.name} />
          <OcTextField label="Definition" onChange={(event) => onUpdate({ definition: event.currentTarget.value })} placeholder="What this pose means to you" showLabel value={gesture.definition} />
          <div className="gesture-field">
            <span>Command</span>
            <OcSelect<ClientCommandId | "none">
              label="Command"
              onChange={(command) => onUpdate({ command })}
              options={[
                { value: "none", label: "No action" },
                ...clientCommands.map((command) => ({ value: command.id, label: command.label })),
              ]}
              value={gesture.command}
            />
          </div>
          <OcButton
            onClick={() => {
              const curls = currentGestureState().hands[0]?.curls;
              if (curls) onUpdate({ curls });
            }}
            size="sm"
            variant="secondary"
          >
            Re-capture current pose
          </OcButton>
        </div>
      ) : null}
    </div>
  );
}

function CurlBars({ curls }: { curls: FingerCurls }) {
  return (
    <div className="gesture-curl-bars" role="img" aria-label="Finger pose">
      {CURL_KEYS.map((key) => (
        <span className="gesture-curl-bar" key={key} title={CURL_LABELS[key]}>
          <span style={{ height: `${Math.round(clampUnit(curls[key]) * 100)}%` }} />
        </span>
      ))}
    </div>
  );
}

function steadyCurls(samples: FingerCurls[]): FingerCurls | null {
  if (samples.length < RECORD_MIN_SAMPLES) return null;
  const median = (key: keyof FingerCurls) => {
    const values = samples.map((sample) => sample[key]).sort((left, right) => left - right);
    const middle = Math.floor(values.length / 2);
    return values.length % 2 ? values[middle] : (values[middle - 1] + values[middle]) / 2;
  };
  return { thumb: median("thumb"), index: median("index"), middle: median("middle"), ring: median("ring"), little: median("little") };
}

function clampUnit(value: number): number {
  return Math.max(0, Math.min(1, value));
}

function permissionLabel(permission: CameraPermission | null): string {
  if (!permission) return "Not requested";
  if (permission.status === "denied") return "Denied";
  if (permission.status === "restricted") return "Restricted";
  return "Not requested";
}
