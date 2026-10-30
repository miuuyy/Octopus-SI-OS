import { dismissClientAnnouncement, recordClientExperimentSignal, useClientExperiment, useClientExperimentState } from "../../clientExperiments";
import { OcButton, OcIconButton } from "../../design-system/components";
import type { OctopusClientAccessProfile } from "../../electron";
import { useGestureShortcutPreferences, useSetGestureShortcutPreferences } from "../../gestureShortcuts";
import { Glyph, type GlyphName } from "../IconGlyphs";

type Announcement = {
  id: string;
  glyph: GlyphName;
  title: string;
  detail: string;
  actionLabel: string;
};

const GESTURE_ANNOUNCEMENT: Announcement = {
  id: "gesture-control",
  glyph: "hand",
  title: "Control Octopus with your hands",
  detail: "Point to move focus and pinch to select. Tracking stays on this device.",
  actionLabel: "Try it",
};

export function HomeWhatsNew({ accessProfile }: { accessProfile: OctopusClientAccessProfile }) {
  const enabled = useClientExperiment(accessProfile, "home_whats_new");
  const { dismissedAnnouncements } = useClientExperimentState(accessProfile);
  const gestures = useGestureShortcutPreferences(accessProfile);
  const setGestures = useSetGestureShortcutPreferences(accessProfile);
  const announcement = enabled && !gestures.enabled && !dismissedAnnouncements.includes(GESTURE_ANNOUNCEMENT.id) ? GESTURE_ANNOUNCEMENT : null;
  if (!announcement) return null;
  return (
    <section aria-label="What's new" className="home-whats-new">
      <span className="home-whats-new__mark"><Glyph name={announcement.glyph} size={18} /></span>
      <div className="home-whats-new__copy">
        <strong>{announcement.title}</strong>
        <span>{announcement.detail}</span>
      </div>
      <OcButton
        onClick={() => {
          recordClientExperimentSignal(accessProfile, "home_whats_new");
          dismissClientAnnouncement(accessProfile, announcement.id);
          setGestures({ enabled: true });
        }}
        size="sm"
        variant="primary"
      >
        {announcement.actionLabel}
      </OcButton>
      <OcIconButton glyph="close" label="Dismiss" onClick={() => dismissClientAnnouncement(accessProfile, announcement.id)} variant="ghost" />
    </section>
  );
}
