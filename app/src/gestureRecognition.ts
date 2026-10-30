import { useSyncExternalStore } from "react";
import { AirHandGestureEngine, type AirHandFrame, type GestureInteraction, type HandObservation, type HandPose, type TrackedHand } from "./airHandEngine";
import { emitAirHandGesture, emitGestureInteraction, type GestureSensitivity } from "./gestureShortcuts";
import { GESTURE_TRACE_FORMAT, GESTURE_TRACE_VERSION, traceFrame, type GestureTrace, type TraceFrame } from "./gestureTrace";
import type { HandLandmarkReply, HandLandmarkRequest } from "./handLandmarkWorker";

export type GestureCamera = {
  deviceId: string;
  label: string;
};

export type GestureRecognitionPhase = "off" | "requesting" | "loading" | "running" | "denied" | "error";

export type GestureRecognitionState = Readonly<{
  activeDeviceId: string;
  devices: readonly GestureCamera[];
  error: string;
  phase: GestureRecognitionPhase;
  hands: readonly TrackedHand[];
  cameraFps: number | null;
}>;

export type GestureDevicesView = Readonly<{
  devices: readonly GestureCamera[];
  phase: GestureRecognitionPhase;
  error: string;
  activeDeviceId: string;
}>;

export type GestureTraceRecording = Readonly<{ done: Promise<GestureTrace>; cancel: () => void }>;

type ActiveTrace = {
  startedAt: number;
  until: number;
  recordedAt: string;
  label: string;
  options: GestureRecognitionOptions;
  camera: { width: number; height: number };
  frames: TraceFrame[];
  resolve: (trace: GestureTrace) => void;
  reject: (error: Error) => void;
};

export type GestureRecognitionOptions = {
  cameraDeviceId: string;
  mirrorX: boolean;
  sensitivity: GestureSensitivity;
};

const NO_HANDS: readonly TrackedHand[] = Object.freeze([]);

const INITIAL_STATE: GestureRecognitionState = Object.freeze({
  activeDeviceId: "",
  devices: [],
  error: "",
  phase: "off",
  hands: NO_HANDS,
  cameraFps: null,
});

const IDLE_HANDS = Object.freeze({ hands: NO_HANDS }) satisfies Partial<GestureRecognitionState>;
const STALE_FRAME_MS = 400;
const DETECTION_TIMEOUT_MS = 2_000;
const CAMERA_RATE_WINDOW = 24;
const HAND_TINT: Record<TrackedHand["side"], string> = { left: "rgb(125 211 252)", right: "rgb(252 196 98)" };

const HAND_CONNECTIONS: ReadonlyArray<readonly [number, number]> = [
  [0, 1], [1, 2], [2, 3], [3, 4],
  [0, 5], [5, 6], [6, 7], [7, 8],
  [5, 9], [9, 10], [10, 11], [11, 12],
  [9, 13], [13, 14], [14, 15], [15, 16],
  [13, 17], [17, 18], [18, 19], [19, 20],
  [0, 17],
];

class GestureRecognitionController {
  private state = INITIAL_STATE;
  private subscribers = new Set<() => void>();
  private stream: MediaStream | null = null;
  private video: HTMLVideoElement | null = null;
  private frame = 0;
  private generation = 0;
  private configKey = "";
  private worker: Worker | null = null;
  private workerReady: Promise<Worker> | null = null;
  private pending: { generation: number; timestamp: number; options: GestureRecognitionOptions } | null = null;
  private engine = new AirHandGestureEngine();
  private lastVideoTime = -1;
  private lastPublishedAt = 0;
  private preview: HTMLCanvasElement | null = null;
  private lastOptions: GestureRecognitionOptions | null = null;
  private aspect = 16 / 9;
  private wanted: GestureRecognitionOptions | null = null;
  private lastFrameAt = 0;
  private frameTimes: number[] = [];
  private watching = false;
  private trace: ActiveTrace | null = null;

  attachPreview = (canvas: HTMLCanvasElement | null): void => {
    this.preview = canvas;
    this.drawPreview();
  };

  private coarse: GestureRecognitionState = INITIAL_STATE;
  private coarseAt = 0;
  private coarseHands = "";
  private devicesView: GestureDevicesView = {
    devices: INITIAL_STATE.devices,
    phase: INITIAL_STATE.phase,
    error: INITIAL_STATE.error,
    activeDeviceId: INITIAL_STATE.activeDeviceId,
  };

  subscribe = (subscriber: () => void): (() => void) => {
    this.subscribers.add(subscriber);
    return () => {
      this.subscribers.delete(subscriber);
    };
  };

  snapshot = (): GestureRecognitionState => this.state;

  coarseSnapshot = (): GestureRecognitionState => this.coarse;

  devicesSnapshot = (): GestureDevicesView => this.devicesView;

  async start(options: GestureRecognitionOptions): Promise<void> {
    const key = JSON.stringify(options);
    this.lastOptions = options;
    this.wanted = options;
    this.watchEnvironment();
    if (this.configKey === key && (this.state.phase === "requesting" || this.state.phase === "loading" || this.state.phase === "running")) return;
    this.stopCapture();
    this.configKey = key;
    const generation = ++this.generation;
    if (!navigator.mediaDevices?.getUserMedia) {
      this.publish({ phase: "error", error: "Camera capture is unavailable in this client." });
      return;
    }
    if (document.hidden) {
      this.configKey = "";
      this.publish({ ...IDLE_HANDS, phase: "off", error: "" });
      return;
    }
    this.publish({ ...IDLE_HANDS, phase: "requesting", error: "", cameraFps: null });
    try {
      const nativePermission = await window.octopusTransport?.cameraAccessStatus?.();
      if (generation !== this.generation) return;
      if (nativePermission && !nativePermission.granted) {
        this.publish({ phase: "denied", error: `Camera permission is ${nativePermission.status}.` });
        return;
      }
      const stream = await openCamera(options.cameraDeviceId);
      if (generation !== this.generation) {
        stopStream(stream);
        return;
      }
      this.stream = stream;
      stream.getVideoTracks()[0]?.addEventListener("ended", () => {
        if (generation !== this.generation || this.stream !== stream) return;
        this.configKey = "";
        this.stopCapture();
        this.publish({ ...IDLE_HANDS, phase: "off", error: "" });
        if (this.wanted) void this.start(this.wanted);
      }, { once: true });
      const video = document.createElement("video");
      video.autoplay = true;
      video.muted = true;
      video.playsInline = true;
      video.srcObject = stream;
      this.video = video;
      await video.play();
      if (generation !== this.generation) return;
      this.publish({ phase: "loading" });
      const worker = await this.landmarkWorker();
      if (generation !== this.generation) return;
      await this.refreshDevices();
      const activeDeviceId = stream.getVideoTracks()[0]?.getSettings().deviceId ?? options.cameraDeviceId;
      this.publish({ activeDeviceId, phase: "running", error: "" });
      this.tick(generation, worker, options);
    } catch (error) {
      if (generation !== this.generation) return;
      this.discardWorker();
      this.stopCapture();
      const message = error instanceof Error ? error.message : String(error);
      const denied = error instanceof DOMException && (error.name === "NotAllowedError" || error.name === "SecurityError");
      this.publish({ ...IDLE_HANDS, phase: denied ? "denied" : "error", error: message });
    }
  }

  stop(): void {
    this.generation += 1;
    this.configKey = "";
    this.wanted = null;
    this.stopCapture();
    this.discardWorker();
    this.publish({ ...INITIAL_STATE, devices: this.state.devices });
  }

  async refreshDevices(): Promise<void> {
    if (!navigator.mediaDevices?.enumerateDevices) return;
    try {
      const devices = (await navigator.mediaDevices.enumerateDevices())
        .filter((device) => device.kind === "videoinput")
        .map((device, index) => ({ deviceId: device.deviceId, label: device.label || `Camera ${index + 1}` }));
      this.publish({ devices });
    } catch {
      this.publish({ devices: [] });
    }
  }

  private watchEnvironment(): void {
    if (this.watching) return;
    this.watching = true;
    document.addEventListener("visibilitychange", () => {
      if (!this.wanted) return;
      if (!document.hidden) {
        if (this.state.phase === "off") void this.start(this.wanted);
        return;
      }
      if (this.state.phase !== "requesting" && this.state.phase !== "loading" && this.state.phase !== "running") return;
      this.generation += 1;
      this.configKey = "";
      this.stopCapture();
      this.publish({ ...IDLE_HANDS, phase: "off", error: "" });
    });
    navigator.mediaDevices?.addEventListener?.("devicechange", () => {
      if (this.wanted) void this.refreshDevices();
    });
  }

  private tick(generation: number, worker: Worker, options: GestureRecognitionOptions): void {
    if (generation !== this.generation || !this.video) return;
    const now = performance.now();
    const video = this.video;
    if (this.pending && now - this.pending.timestamp > DETECTION_TIMEOUT_MS) {
      this.fail("Hand tracking stopped answering camera frames.");
      return;
    }
    if (!this.pending && video.readyState >= HTMLMediaElement.HAVE_CURRENT_DATA && video.currentTime !== this.lastVideoTime && video.videoWidth > 0 && video.videoHeight > 0) {
      this.lastVideoTime = video.currentTime;
      this.lastFrameAt = now;
      this.aspect = video.videoWidth / video.videoHeight;
      this.frameTimes.push(now);
      if (this.frameTimes.length > CAMERA_RATE_WINDOW) this.frameTimes.shift();
      const cameraFps = this.frameTimes.length >= CAMERA_RATE_WINDOW / 2
        ? Math.round((this.frameTimes.length - 1) * 1000 / (now - (this.frameTimes[0] ?? now)))
        : null;
      if (cameraFps !== this.state.cameraFps) this.publish({ cameraFps });
      let frame: VideoFrame;
      try {
        frame = new VideoFrame(video, { timestamp: Math.round(now * 1000) });
      } catch (error) {
        this.fail(`The camera frame could not be read: ${error instanceof Error ? error.message : String(error)}`);
        return;
      }
      this.pending = { generation, timestamp: now, options };
      const request: HandLandmarkRequest = { type: "frame", frame, timestamp: now };
      worker.postMessage(request, [frame]);
    } else if (!this.pending && this.lastFrameAt > 0 && now - this.lastFrameAt > STALE_FRAME_MS && this.state.hands.length > 0) {
      const frame = this.engine.process([], now, this.engineOptions(options));
      this.capture(now, [], frame);
      this.consume(frame, now);
    }
    this.finishTrace(now);
    this.drawPreview();
    this.frame = window.requestAnimationFrame(() => this.tick(generation, worker, options));
  }

  private receive(reply: HandLandmarkReply): void {
    if (reply.type === "ready") return;
    if (reply.type === "error") {
      this.fail(reply.message);
      return;
    }
    const pending = this.pending;
    if (!pending || pending.timestamp !== reply.timestamp) return;
    this.pending = null;
    if (pending.generation !== this.generation) return;
    const frame = this.engine.process(reply.hands, reply.timestamp, this.engineOptions(pending.options));
    this.capture(reply.timestamp, reply.hands, frame);
    this.consume(frame, reply.timestamp);
  }

  private fail(message: string): void {
    this.generation += 1;
    this.configKey = "";
    this.stopCapture();
    this.discardWorker();
    this.publish({ ...IDLE_HANDS, phase: "error", error: message });
  }

  recordTrace(durationMs: number, label: string): GestureTraceRecording {
    const video = this.video;
    const options = this.lastOptions;
    if (this.state.phase !== "running" || !video || !options) {
      return { done: Promise.reject(new Error("Start the camera in Settings → Gestures before recording a gesture trace.")), cancel: () => undefined };
    }
    if (this.trace) return { done: Promise.reject(new Error("A gesture trace is already recording.")), cancel: () => undefined };
    if (!Number.isFinite(durationMs) || durationMs <= 0) return { done: Promise.reject(new Error("Gesture trace duration must be positive.")), cancel: () => undefined };
    const startedAt = performance.now();
    let resolveTrace: (trace: GestureTrace) => void = () => undefined;
    let rejectTrace: (error: Error) => void = () => undefined;
    const done = new Promise<GestureTrace>((resolve, reject) => {
      resolveTrace = resolve;
      rejectTrace = reject;
    });
    const trace: ActiveTrace = {
      startedAt,
      until: startedAt + durationMs,
      recordedAt: new Date().toISOString(),
      label,
      options,
      camera: { width: video.videoWidth, height: video.videoHeight },
      frames: [],
      resolve: resolveTrace,
      reject: rejectTrace,
    };
    this.trace = trace;
    return {
      done,
      cancel: () => {
        if (this.trace !== trace) return;
        this.trace = null;
        trace.reject(new Error("Gesture trace recording was cancelled."));
      },
    };
  }

  private capture(now: number, observations: readonly HandObservation[], frame: AirHandFrame): void {
    const trace = this.trace;
    if (!trace || now > trace.until) return;
    trace.frames.push(traceFrame(now - trace.startedAt, this.aspect, observations, frame));
  }

  private finishTrace(now: number): void {
    const trace = this.trace;
    if (!trace || now < trace.until) return;
    this.trace = null;
    trace.resolve({
      format: GESTURE_TRACE_FORMAT,
      version: GESTURE_TRACE_VERSION,
      recordedAt: trace.recordedAt,
      label: trace.label,
      options: { mirrorX: trace.options.mirrorX, sensitivity: trace.options.sensitivity },
      camera: trace.camera,
      frames: trace.frames,
    });
  }

  private engineOptions(options: GestureRecognitionOptions) {
    return { aspect: this.aspect, mirrorX: options.mirrorX, sensitivity: options.sensitivity };
  }

  private consume(frame: AirHandFrame, now: number): void {
    for (const command of frame.commands) emitAirHandGesture(command, "camera");
    emitInteractions(frame.interactions);
    if (frame.hands.length > 0 || this.state.hands.length > 0 || now - this.lastPublishedAt >= 100) {
      this.lastPublishedAt = now;
      this.publish({ hands: frame.hands.length > 0 ? frame.hands : NO_HANDS });
    }
  }

  private drawPreview(): void {
    const canvas = this.preview;
    if (!canvas) return;
    const width = canvas.clientWidth;
    const height = canvas.clientHeight;
    if (!width || !height) return;
    const ratio = Math.min(window.devicePixelRatio || 1, 2);
    const deviceWidth = Math.round(width * ratio);
    const deviceHeight = Math.round(height * ratio);
    if (canvas.width !== deviceWidth || canvas.height !== deviceHeight) {
      canvas.width = deviceWidth;
      canvas.height = deviceHeight;
    }
    const context = canvas.getContext("2d");
    if (!context) return;
    context.clearRect(0, 0, canvas.width, canvas.height);
    const video = this.video;
    if (!video || video.readyState < HTMLMediaElement.HAVE_CURRENT_DATA || !video.videoWidth) return;
    const frameRatio = (video.videoWidth / video.videoHeight).toFixed(4);
    const frame = canvas.parentElement;
    if (frame && frame.style.getPropertyValue("--gesture-camera-ratio") !== frameRatio) {
      frame.style.setProperty("--gesture-camera-ratio", frameRatio);
    }
    const mirror = this.lastOptions?.mirrorX !== false;
    const scale = Math.min(canvas.width / video.videoWidth, canvas.height / video.videoHeight);
    const drawWidth = video.videoWidth * scale;
    const drawHeight = video.videoHeight * scale;
    const offsetX = (canvas.width - drawWidth) / 2;
    const offsetY = (canvas.height - drawHeight) / 2;
    context.save();
    if (mirror) {
      context.translate(canvas.width, 0);
      context.scale(-1, 1);
    }
    context.drawImage(video, offsetX, offsetY, drawWidth, drawHeight);
    context.restore();
    const place = (x: number, y: number) => {
      const px = offsetX + x * drawWidth;
      return { x: mirror ? canvas.width - px : px, y: offsetY + y * drawHeight };
    };
    context.lineCap = "round";
    for (const hand of this.state.hands) {
      const landmarks = hand.landmarks;
      const pointFor = (index: number) => {
        const mark = landmarks[index];
        return mark ? place(mark.x, mark.y) : null;
      };
      const tint = HAND_TINT[hand.side];
      context.globalAlpha = hand.coasting ? 0.45 : 1;
      context.lineWidth = 2 * ratio;
      context.strokeStyle = "rgb(255 255 255 / 82%)";
      for (const [from, to] of HAND_CONNECTIONS) {
        const start = pointFor(from);
        const end = pointFor(to);
        if (!start || !end) continue;
        context.beginPath();
        context.moveTo(start.x, start.y);
        context.lineTo(end.x, end.y);
        context.stroke();
      }
      context.fillStyle = tint;
      landmarks.forEach((_, index) => {
        const point = pointFor(index);
        if (!point) return;
        context.beginPath();
        context.arc(point.x, point.y, (index === 4 || index === 8 ? 3.4 : 2.1) * ratio, 0, Math.PI * 2);
        context.fill();
      });
      const thumb = pointFor(4);
      const index = pointFor(8);
      if (thumb && index && hand.pinch > 0.35) {
        context.strokeStyle = tint;
        context.lineWidth = (hand.pose === "pinch" ? 3.5 : 1.5) * ratio;
        context.beginPath();
        context.moveTo(thumb.x, thumb.y);
        context.lineTo(index.x, index.y);
        context.stroke();
      }
      const wrist = pointFor(0);
      if (wrist) {
        const label = `${hand.side === "left" ? "Left" : "Right"} · ${previewPoseLabel(hand.pose)}`;
        context.font = `${11 * ratio}px system-ui, sans-serif`;
        const textWidth = context.measureText(label).width;
        const padX = 6 * ratio;
        const boxHeight = 18 * ratio;
        const boxX = Math.min(Math.max(wrist.x - textWidth / 2 - padX, 0), canvas.width - textWidth - padX * 2);
        const boxY = Math.min(wrist.y + 8 * ratio, canvas.height - boxHeight);
        context.fillStyle = "rgb(0 0 0 / 62%)";
        context.beginPath();
        context.roundRect(boxX, boxY, textWidth + padX * 2, boxHeight, 9 * ratio);
        context.fill();
        context.fillStyle = tint;
        context.textBaseline = "middle";
        context.fillText(label, boxX + padX, boxY + boxHeight / 2);
      }
    }
    context.globalAlpha = 1;
  }

  private landmarkWorker(): Promise<Worker> {
    if (this.workerReady) return this.workerReady;
    const worker = new Worker(new URL("./handLandmarkWorker.ts", import.meta.url), { type: "module", name: "hand-landmarks" });
    this.worker = worker;
    const ready = new Promise<Worker>((resolve, reject) => {
      const settle = (event: MessageEvent<HandLandmarkReply>) => {
        if (event.data.type === "result") return;
        worker.removeEventListener("message", settle);
        if (event.data.type === "ready") {
          worker.addEventListener("message", (message: MessageEvent<HandLandmarkReply>) => this.receive(message.data));
          resolve(worker);
        } else {
          reject(new Error(event.data.message));
        }
      };
      worker.addEventListener("message", settle);
      worker.addEventListener("error", (event) => {
        const message = event.message || "The hand tracking worker stopped.";
        if (this.worker === worker && this.state.phase === "running") this.fail(message);
        reject(new Error(message));
      });
    });
    this.workerReady = ready;
    const start: HandLandmarkRequest = {
      type: "start",
      wasmRoot: new URL("mediapipe/wasm", document.baseURI).toString().replace(/\/$/, ""),
      modelPath: new URL("models/hand_landmarker.task", document.baseURI).toString(),
    };
    worker.postMessage(start);
    return ready;
  }

  private discardWorker(): void {
    this.worker?.terminate();
    this.worker = null;
    this.workerReady = null;
    this.pending = null;
  }

  private stopCapture(): void {
    if (this.frame) window.cancelAnimationFrame(this.frame);
    this.frame = 0;
    this.pending = null;
    this.lastVideoTime = -1;
    this.lastFrameAt = 0;
    this.lastPublishedAt = 0;
    this.frameTimes = [];
    if (this.trace) {
      const trace = this.trace;
      this.trace = null;
      trace.reject(new Error("Gesture recognition stopped before the trace finished."));
    }
    emitInteractions(this.engine.reset());
    if (this.state.hands.length > 0) this.publish(IDLE_HANDS);
    if (this.video) {
      this.video.pause();
      this.video.srcObject = null;
      this.video.remove();
      this.video = null;
    }
    if (this.stream) {
      stopStream(this.stream);
      this.stream = null;
    }
    this.drawPreview();
  }

  private publish(patch: Partial<GestureRecognitionState>): void {
    this.state = Object.freeze({ ...this.state, ...patch });
    this.updateCoarse();
    this.updateDevicesView();
    for (const subscriber of [...this.subscribers]) subscriber();
  }

  private updateCoarse(): void {
    const state = this.state;
    const cached = this.coarse;
    const hands = state.hands.map((hand) => `${hand.id}:${hand.side}:${hand.pose}:${hand.engaged}`).join("|");
    const structural = state.phase !== cached.phase
      || hands !== this.coarseHands
      || state.cameraFps !== cached.cameraFps
      || state.error !== cached.error
      || state.devices !== cached.devices
      || state.activeDeviceId !== cached.activeDeviceId;
    const now = performance.now();
    if (structural || now - this.coarseAt >= 150) {
      this.coarse = state;
      this.coarseAt = now;
      this.coarseHands = hands;
    }
  }

  private updateDevicesView(): void {
    const state = this.state;
    const cached = this.devicesView;
    if (state.devices === cached.devices
      && state.phase === cached.phase
      && state.error === cached.error
      && state.activeDeviceId === cached.activeDeviceId) {
      return;
    }
    this.devicesView = {
      devices: state.devices,
      phase: state.phase,
      error: state.error,
      activeDeviceId: state.activeDeviceId,
    };
  }
}

const controller = new GestureRecognitionController();
import.meta.hot?.dispose(() => {
  controller.stop();
  window.location.reload();
});

export function useGestureRecognitionCoarseState(): GestureRecognitionState {
  return useSyncExternalStore(controller.subscribe, controller.coarseSnapshot, () => INITIAL_STATE);
}

const INITIAL_DEVICES_VIEW: GestureDevicesView = {
  devices: INITIAL_STATE.devices,
  phase: INITIAL_STATE.phase,
  error: INITIAL_STATE.error,
  activeDeviceId: INITIAL_STATE.activeDeviceId,
};

export function useGestureDevices(): GestureDevicesView {
  return useSyncExternalStore(controller.subscribe, controller.devicesSnapshot, () => INITIAL_DEVICES_VIEW);
}

export function subscribeGestureState(listener: (state: GestureRecognitionState) => void): () => void {
  return controller.subscribe(() => listener(controller.snapshot()));
}

export function currentGestureState(): GestureRecognitionState {
  return controller.snapshot();
}

export function startGestureRecognition(options: GestureRecognitionOptions): Promise<void> {
  return controller.start(options);
}

export function stopGestureRecognition(): void {
  controller.stop();
}

export function recordGestureTrace(durationMs: number, label: string): GestureTraceRecording {
  return controller.recordTrace(durationMs, label);
}

export function refreshGestureCameras(): Promise<void> {
  return controller.refreshDevices();
}

export function attachGesturePreview(canvas: HTMLCanvasElement | null): void {
  controller.attachPreview(canvas);
}

export type CameraPermission = { granted: boolean; status: string };

export async function readCameraPermission(): Promise<CameraPermission> {
  const transport = window.octopusTransport;
  if (transport?.cameraAccessStatus) return transport.cameraAccessStatus();
  return { granted: true, status: "browser-managed" };
}

export async function requestCameraPermission(): Promise<CameraPermission> {
  const transport = window.octopusTransport;
  if (transport?.requestCameraAccess) return transport.requestCameraAccess();
  return { granted: true, status: "browser-managed" };
}

async function openCamera(deviceId: string): Promise<MediaStream> {
  const request = (exactDeviceId: string) => navigator.mediaDevices.getUserMedia({
    audio: false,
    video: {
      deviceId: exactDeviceId ? { exact: exactDeviceId } : undefined,
      facingMode: "user",
      frameRate: { ideal: 30, max: 30 },
      height: { ideal: 720 },
      width: { ideal: 1280 },
    },
  });
  try {
    return await request(deviceId);
  } catch (error) {
    const missing = error instanceof DOMException && (error.name === "OverconstrainedError" || error.name === "NotFoundError");
    if (!deviceId || !missing) throw error;
    return request("");
  }
}

function stopStream(stream: MediaStream): void {
  for (const track of stream.getTracks()) track.stop();
}

function emitInteractions(interactions: readonly GestureInteraction[]): void {
  for (const interaction of interactions) emitGestureInteraction(interaction, "camera");
}

function previewPoseLabel(pose: HandPose): string {
  if (pose === "point") return "Point";
  if (pose === "pinch") return "Pinch";
  if (pose === "open") return "Open";
  if (pose === "two") return "Two fingers";
  if (pose === "fist") return "Fist";
  return "Hand";
}
