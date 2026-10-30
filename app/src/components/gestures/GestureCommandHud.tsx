import { useEffect, useState } from "react";
import { createPortal } from "react-dom";
import { recordClientExperimentSignal, useClientExperiment } from "../../clientExperiments";
import type { OctopusClientAccessProfile } from "../../electron";
import { subscribeAirHandGestures, useGestureShortcutPreferences } from "../../gestureShortcuts";
import { gestureCommandActivity } from "./GestureLivePreview";
import "./gesturePreview.css";

const HUD_MS = 1_600;

export function GestureCommandHud({ accessProfile }: { accessProfile: OctopusClientAccessProfile }) {
  const preferences = useGestureShortcutPreferences(accessProfile);
  const enabled = useClientExperiment(accessProfile, "gesture_feedback") && preferences.enabled;
  const [shown, setShown] = useState<{ label: string; at: number } | null>(null);
  useEffect(() => {
    if (!enabled) return;
    return subscribeAirHandGestures((gesture, source) => {
      if (source !== "camera") return;
      setShown({ label: gestureCommandActivity(gesture, preferences.mappings[gesture]), at: Date.now() });
      recordClientExperimentSignal(accessProfile, "gesture_feedback");
    });
  }, [accessProfile, enabled, preferences.mappings]);
  useEffect(() => {
    if (!shown) return;
    const timer = window.setTimeout(() => setShown(null), HUD_MS);
    return () => window.clearTimeout(timer);
  }, [shown]);
  if (!enabled || !shown) return null;
  return createPortal(<div className="gesture-command-hud" key={shown.at} role="status">{shown.label}</div>, document.body);
}
