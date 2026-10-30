import { useEffect, useRef, useState } from "react";
import { OcIconButton, OcMenuItem, OcStatus } from "../../design-system/components";
import { OcDropdown } from "../../design-system/OcDropdown";
import type { OctopusClientAccessProfile } from "../../electron";
import { useGestureDevices, type GestureRecognitionPhase } from "../../gestureRecognition";
import { useGestureShortcutPreferences, useSetGestureShortcutPreferences } from "../../gestureShortcuts";
import { MessageTooltip } from "../chat/MessageTooltip";
import { claimGesturePreview, GestureLiveStatus, gesturePreviewNote, gestureRecognitionLabel } from "./GestureLivePreview";

export function GestureRailIndicator({ accessProfile, onOpenSettings }: {
  accessProfile: OctopusClientAccessProfile;
  onOpenSettings: () => void;
}) {
  const preferences = useGestureShortcutPreferences(accessProfile);
  const setPreferences = useSetGestureShortcutPreferences(accessProfile);
  const { phase } = useGestureDevices();
  const trigger = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  const [canvas, setCanvas] = useState<HTMLCanvasElement | null>(null);
  useEffect(() => canvas ? claimGesturePreview(canvas) : undefined, [canvas]);
  if (!preferences.enabled) return null;
  const label = gestureRecognitionLabel(phase);
  return (
    <div className="navigation-rail-item" data-gesture-phase={phase}>
      <MessageTooltip label={`Gesture control · ${label}`} side="right" delayMs={300} disabled={open}>
        <OcIconButton appearance="rail" buttonRef={trigger} glyph="hand" label={`Gesture control, ${label}`} onClick={() => setOpen(!open)} selected={open} title="" />
      </MessageTooltip>
      <span aria-hidden="true" className="navigation-rail-gesture-state" />
      <OcDropdown align="end" alignOffset={-4} anchorRef={trigger} aria-label="Gesture control" className="gesture-rail-menu" contentWidth="sidebar" onOpenChange={setOpen} open={open} side="right">
        <div className="gesture-rail-menu__head">
          <span>Gesture control</span>
          <OcStatus label={label} tone={gesturePhaseTone(phase)} />
        </div>
        <div className="gesture-preview-menu">
          <canvas ref={setCanvas} />
          <GestureLiveStatus cameraLive={phase === "running"} idleNote={gesturePreviewNote(true, phase !== "denied", phase)} mappings={preferences.mappings} />
        </div>
        <div className="account-menu-separator" role="separator" />
        <OcMenuItem label="Gesture settings" leadingIcon="settings" onClick={() => { setOpen(false); onOpenSettings(); }} role="menuitem" />
        <OcMenuItem label="Turn off gesture control" leadingIcon="close" onClick={() => { setOpen(false); setPreferences({ enabled: false }); }} role="menuitem" />
      </OcDropdown>
    </div>
  );
}

function gesturePhaseTone(phase: GestureRecognitionPhase): "success" | "danger" | "neutral" {
  if (phase === "running") return "success";
  return phase === "denied" || phase === "error" ? "danger" : "neutral";
}
