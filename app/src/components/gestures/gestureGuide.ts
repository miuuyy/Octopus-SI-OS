import type { GestureArtId } from "./GestureIllustration";

export type GestureGuideId = "point" | "pinch" | "carry" | "throw" | "resize";

export type GestureGuideEntry = Readonly<{ title: string; sentence: string; art: GestureArtId }>;

export const GESTURE_GUIDE: Readonly<Record<GestureGuideId, GestureGuideEntry>> = {
  point: {
    title: "Point and move",
    sentence: "Point with your index finger. Where your hand is in the camera view is where focus goes; a comfortable area around your hand covers the whole window.",
    art: "point",
  },
  pinch: {
    title: "Pinch to select",
    sentence: "Tap your thumb and index finger together, then let go, to press whatever has focus, like a click.",
    art: "pinch",
  },
  carry: {
    title: "Pinch and move",
    sentence: "Pinch, keep holding, and move your hand to drag a Home widget to a new spot.",
    art: "carry",
  },
  throw: {
    title: "Throw it away",
    sentence: "While carrying a widget, flick your hand and let go mid-motion to hide it.",
    art: "throw",
  },
  resize: {
    title: "Two-hand resize",
    sentence: "Pinch with both hands and pull them apart to make a widget bigger, or push them together to shrink it.",
    art: "resize",
  },
};
