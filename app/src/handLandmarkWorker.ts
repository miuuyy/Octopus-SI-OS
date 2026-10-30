import { FilesetResolver, HandLandmarker, type HandLandmarkerResult } from "@mediapipe/tasks-vision";
import type { HandObservation } from "./airHandEngine";

export type HandLandmarkRequest =
  | Readonly<{ type: "start"; wasmRoot: string; modelPath: string }>
  | Readonly<{ type: "frame"; frame: VideoFrame; timestamp: number }>;

export type HandLandmarkReply =
  | Readonly<{ type: "ready" }>
  | Readonly<{ type: "result"; timestamp: number; detectMs: number; hands: HandObservation[] }>
  | Readonly<{ type: "error"; message: string }>;

let landmarker: HandLandmarker | null = null;

self.addEventListener("message", (event: MessageEvent<HandLandmarkRequest>) => {
  const request = event.data;
  if (request.type === "start") void start(request.wasmRoot, request.modelPath);
  else detect(request.frame, request.timestamp);
});

async function start(wasmRoot: string, modelPath: string): Promise<void> {
  try {
    const fileset = await FilesetResolver.forVisionTasks(wasmRoot, true);
    landmarker = await HandLandmarker.createFromOptions(fileset, {
      baseOptions: { delegate: "GPU", modelAssetPath: modelPath },
      minHandDetectionConfidence: 0.55,
      minHandPresenceConfidence: 0.45,
      minTrackingConfidence: 0.35,
      numHands: 2,
      runningMode: "VIDEO",
    });
    reply({ type: "ready" });
  } catch (error) {
    reply({ type: "error", message: `Hand tracking could not start on the GPU: ${errorMessage(error)}` });
  }
}

function detect(frame: VideoFrame, timestamp: number): void {
  try {
    if (!landmarker) throw new Error("Hand tracking received a frame before it started.");
    const startedAt = performance.now();
    const result = landmarker.detectForVideo(frame, timestamp);
    reply({ type: "result", timestamp, detectMs: performance.now() - startedAt, hands: observations(result) });
  } catch (error) {
    reply({ type: "error", message: `Hand tracking failed on a camera frame: ${errorMessage(error)}` });
  } finally {
    frame.close();
  }
}

function observations(result: HandLandmarkerResult): HandObservation[] {
  return result.landmarks.map((image, index) => {
    const category = result.handedness[index]?.[0];
    return {
      image: image.map(({ x, y, z }) => ({ x, y, z })),
      world: (result.worldLandmarks[index] ?? []).map(({ x, y, z }) => ({ x, y, z })),
      label: category?.categoryName ?? "",
      score: category?.score ?? 0,
    };
  });
}

function reply(message: HandLandmarkReply): void {
  self.postMessage(message);
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
