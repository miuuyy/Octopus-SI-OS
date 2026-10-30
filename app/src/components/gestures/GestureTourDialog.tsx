import { useEffect, useId, useState, type ReactNode } from "react";
import type { OctopusClientAccessProfile } from "../../electron";
import { OcButton, OcDialog, OcIconButton, OcStatus } from "../../design-system/components";
import { readCameraPermission, requestCameraPermission, type CameraPermission } from "../../gestureRecognition";
import { useGestureShortcutPreferences, useSetGestureShortcutPreferences } from "../../gestureShortcuts";
import { GESTURE_GUIDE } from "./gestureGuide";
import { GestureIllustration, type GestureArtId } from "./GestureIllustration";

type TourStep = "welcome" | "focus" | "commands";

const STEP_ART: Readonly<Record<TourStep, GestureArtId>> = { welcome: "point", focus: "pinch", commands: "swipe_left" };
const STEP_TITLE: Readonly<Record<TourStep, string>> = {
  welcome: "Welcome to gesture control",
  focus: "Point, then pinch",
  commands: "Swipes and Home widgets",
};
const STEP_PRIMARY: Readonly<Record<TourStep, string>> = { welcome: "Take 2-step tour", focus: "Last step", commands: "Start using gestures" };

export function GestureTourHost({ accessProfile, onOpenSettings }: {
  accessProfile: OctopusClientAccessProfile;
  onOpenSettings: () => void;
}) {
  const preferences = useGestureShortcutPreferences(accessProfile);
  const setPreferences = useSetGestureShortcutPreferences(accessProfile);
  const open = preferences.enabled && !preferences.tourCompleted;
  return (
    <GestureTourDialog
      onComplete={() => setPreferences({ tourCompleted: true })}
      onOpenSettings={() => {
        setPreferences({ tourCompleted: true });
        onOpenSettings();
      }}
      open={open}
    />
  );
}

function GestureTourDialog({ onComplete, onOpenSettings, open }: { onComplete: () => void; onOpenSettings: () => void; open: boolean }) {
  const titleId = useId();
  const [step, setStep] = useState<TourStep>("welcome");
  useEffect(() => {
    if (open) setStep("welcome");
  }, [open]);
  const advance = () => {
    if (step === "welcome") setStep("focus");
    else if (step === "focus") setStep("commands");
    else onComplete();
  };
  return (
    <OcDialog ariaLabelledBy={titleId} onClose={onComplete} open={open} size="feature">
      {open ? (
        <div className="relative flex min-h-0 flex-col gap-6 overflow-y-auto pb-8">
          <OcIconButton appearance="dialog-close" className="absolute top-3.5 right-3.5 z-10" glyph="close" label="Close" onClick={onComplete} />
          <div className="flex h-[266px] w-full shrink-0 items-center justify-center bg-surface-secondary">
            <GestureIllustration gesture={STEP_ART[step]} key={step} size="large" />
          </div>
          <div className="flex flex-col items-center gap-6 px-8 text-center">
            <div className="flex flex-col items-center gap-2">
              <h2 className="heading-dialog m-0 font-semibold text-default" id={titleId}>{STEP_TITLE[step]}</h2>
              <TourBody step={step} />
            </div>
            <div className="flex flex-wrap items-center justify-center gap-3">
              {step === "welcome" ? <OcButton onClick={onComplete} variant="secondary">Skip</OcButton> : null}
              {step === "commands" ? <OcButton onClick={onOpenSettings} variant="secondary">Settings</OcButton> : null}
              <OcButton onClick={advance} variant="primary">{STEP_PRIMARY[step]}</OcButton>
            </div>
          </div>
        </div>
      ) : null}
    </OcDialog>
  );
}

function TourBody({ step }: { step: TourStep }) {
  if (step === "welcome") {
    return (
      <div className="flex w-full flex-col gap-4">
        <TourCopy>Your hands become a second way to move through Octopus. Tracking runs on this device, and camera frames never reach a task.</TourCopy>
        <TourCameraAccess />
      </div>
    );
  }
  if (step === "focus") {
    return (
      <div className="flex w-full flex-col gap-2">
        <TourCopy>{GESTURE_GUIDE.point.sentence}</TourCopy>
        <TourCopy>{GESTURE_GUIDE.pinch.sentence}</TourCopy>
      </div>
    );
  }
  return (
    <div className="flex w-full flex-col gap-2">
      <TourCopy>Flick an open hand to go back or forward. Pick what each swipe does in Settings.</TourCopy>
      <TourCopy>{GESTURE_GUIDE.carry.sentence}</TourCopy>
    </div>
  );
}

function TourCopy({ children }: { children: ReactNode }) {
  return <p className="m-0 text-sm leading-5 text-secondary">{children}</p>;
}

function TourCameraAccess() {
  const [permission, setPermission] = useState<CameraPermission | null>(null);
  const [requesting, setRequesting] = useState(false);
  useEffect(() => {
    let disposed = false;
    void readCameraPermission().then((value) => {
      if (!disposed) setPermission(value);
    });
    return () => {
      disposed = true;
    };
  }, []);
  if (!permission) return null;
  if (permission.granted) {
    return <div className="flex justify-center"><OcStatus label="Camera is ready" tone="success" /></div>;
  }
  const denied = permission.status === "denied";
  return (
    <div className="flex flex-col gap-3">
      <div className="flex flex-col gap-1 text-start">
        <span className="text-sm font-medium text-default">Allow the camera</span>
        <span className="text-xs leading-4 text-secondary">
          {denied
            ? "Camera access was denied. Enable it for Octopus in System Settings → Privacy & Security → Camera."
            : "Octopus needs the camera to see your hands."}
        </span>
      </div>
      {denied ? null : (
        <OcButton
          className="justify-center self-center"
          disabled={requesting}
          onClick={() => {
            setRequesting(true);
            void requestCameraPermission().then(setPermission).finally(() => setRequesting(false));
          }}
          variant="secondary"
        >
          {requesting ? "Requesting…" : "Allow camera access"}
        </OcButton>
      )}
    </div>
  );
}
