import { useEffect, useState } from "react";
import { clientCommands, type ClientCommandId } from "../../clientActions";
import type { HandPose } from "../../airHandEngine";
import { useGestureFocusView, type GestureFocusActivity } from "../../gestureFocus";
import { attachGesturePreview, useGestureRecognitionCoarseState, type GestureRecognitionPhase } from "../../gestureRecognition";
import { airHandGestures, subscribeAirHandGestures, type AirHandGestureId, type GestureCommandId } from "../../gestureShortcuts";
import "./gesturePreview.css";

const RECOGNIZED_GESTURE_MS = 2_200;
const previewClaims: HTMLCanvasElement[] = [];

export function claimGesturePreview(canvas: HTMLCanvasElement): () => void {
  previewClaims.push(canvas);
  attachGesturePreview(canvas);
  return () => {
    previewClaims.splice(previewClaims.indexOf(canvas), 1);
    attachGesturePreview(previewClaims[previewClaims.length - 1] ?? null);
  };
}

export function GestureLiveStatus({ cameraLive, idleNote, mappings }: {
  cameraLive: boolean;
  idleNote: string;
  mappings: Readonly<Record<AirHandGestureId, GestureCommandId>>;
}) {
  const recognition = useGestureRecognitionCoarseState();
  const focus = useGestureFocusView();
  const [command, setCommand] = useState<{ gesture: AirHandGestureId; at: number } | null>(null);
  const [now, setNow] = useState(0);
  useEffect(() => subscribeAirHandGestures((gesture) => setCommand({ gesture, at: Date.now() })), []);
  const latestAt = Math.max(command?.at ?? 0, focus.last?.at ?? 0);
  useEffect(() => {
    if (!latestAt) return;
    setNow(Date.now());
    const timer = window.setTimeout(() => setNow(Date.now()), Math.max(0, latestAt + RECOGNIZED_GESTURE_MS - Date.now()));
    return () => window.clearTimeout(timer);
  }, [latestAt]);
  const activity = recentActivity(command, focus.last, mappings, now);
  const hands = [...recognition.hands].sort((left, right) => (left.side === right.side ? 0 : left.side === "left" ? -1 : 1));
  const summary = hands.map((hand) => `${hand.side === "left" ? "Left" : "Right"} · ${poseLabel(hand.pose)}`).join("  ·  ");
  return (
    <>
      <div className={`gesture-preview-badge${activity ? " is-recognized" : ""}`}>
        {cameraLive ? summary || "Show one or both hands" : idleNote}
      </div>
      {cameraLive && (activity || focus.target) ? (
        <div className="gesture-preview-activity" role="status">
          {activity ? <span className="gesture-preview-activity__event">{activity}</span> : null}
          {focus.target ? <span className="gesture-preview-activity__focus">Focus · {focus.target}</span> : null}
        </div>
      ) : null}
    </>
  );
}

export function gestureCommandActivity(gesture: AirHandGestureId, mapped: GestureCommandId): string {
  return `${gestureLabel(gesture)} → ${mapped === "none" ? "No action" : commandLabel(mapped)}`;
}

export function gesturePreviewNote(enabled: boolean, granted: boolean, phase: GestureRecognitionPhase): string {
  if (!enabled) return "Enable gesture control";
  if (!granted) return "Allow camera access";
  return gestureRecognitionLabel(phase);
}

export function gestureRecognitionLabel(phase: GestureRecognitionPhase): string {
  if (phase === "requesting") return "Requesting camera";
  if (phase === "loading") return "Loading recognition";
  if (phase === "running") return "Camera active";
  if (phase === "denied") return "Permission denied";
  if (phase === "error") return "Unavailable";
  return "Off";
}

function recentActivity(
  command: { gesture: AirHandGestureId; at: number } | null,
  focus: GestureFocusActivity | null,
  mappings: Readonly<Record<AirHandGestureId, GestureCommandId>>,
  now: number,
): string {
  if (command && command.at >= (focus?.at ?? 0)) {
    if (now - command.at > RECOGNIZED_GESTURE_MS) return "";
    return `${gestureCommandActivity(command.gesture, mappings[command.gesture])} · runs when Settings is closed`;
  }
  if (!focus || now - focus.at > RECOGNIZED_GESTURE_MS) return "";
  return `${focus.gesture} → ${focus.outcome}`;
}

function gestureLabel(gesture: AirHandGestureId): string {
  return airHandGestures.find((candidate) => candidate.id === gesture)?.label ?? gesture;
}

function commandLabel(command: ClientCommandId): string {
  return clientCommands.find((candidate) => candidate.id === command)?.label ?? command;
}

function poseLabel(pose: HandPose): string {
  if (pose === "point") return "Point";
  if (pose === "pinch") return "Pinch";
  if (pose === "open") return "Open hand";
  if (pose === "two") return "Two fingers";
  if (pose === "fist") return "Fist";
  return "Hand";
}
