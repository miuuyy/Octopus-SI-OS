import { useCallback, useSyncExternalStore } from "react";
import type { GestureInteraction } from "./airHandEngine";
import { clientCommands, type ClientCommandId } from "./clientActions";
import type { OctopusClientAccessProfile } from "./electron";

export type AirHandGestureId = "swipe_left" | "swipe_right" | "fist";
export type GestureEventSource = "camera" | "test";
export type GestureCommandId = ClientCommandId | "none";
export type GestureSensitivity = "responsive" | "balanced" | "deliberate";

export type GestureShortcutPreferences = Readonly<{
  enabled: boolean;
  cameraDeviceId: string;
  mirrorX: boolean;
  sensitivity: GestureSensitivity;
  spatialControl: boolean;
  tourCompleted: boolean;
  mappings: Readonly<Record<AirHandGestureId, GestureCommandId>>;
  removedBuiltins: readonly AirHandGestureId[];
}>;

export const airHandGestures: readonly { id: AirHandGestureId; label: string; detail: string }[] = [
  { id: "swipe_left", label: "Swipe left", detail: "Flick an open hand quickly to the left. Handy for going back without looking for a button." },
  { id: "swipe_right", label: "Swipe right", detail: "Flick an open hand quickly to the right. Handy for going forward again." },
  { id: "fist", label: "Fist hold", detail: "Close your hand and hold it still for a moment to run the command you pick here." },
];

const DEFAULT_PREFERENCES: GestureShortcutPreferences = Object.freeze({
  enabled: false,
  cameraDeviceId: "",
  mirrorX: true,
  sensitivity: "balanced",
  spatialControl: true,
  tourCompleted: false,
  mappings: Object.freeze({
    swipe_left: "navigate_back",
    swipe_right: "navigate_forward",
    fist: "none",
  }),
  removedBuiltins: Object.freeze([]),
});

const STORAGE_KEY = "octopus.gesture-shortcuts.v1";
const GESTURE_EVENT = "octopus:airhand-gesture";
const INTERACTION_EVENT = "octopus:airhand-interaction";
const INTERACTION_TYPES: Readonly<Record<GestureInteraction["type"], true>> = { point: true, navigate: true, press: true, grab: true, release: true, cancel: true, transform: true };
const stores = new Map<OctopusClientAccessProfile, GestureStore>();

type GestureStore = {
  current: GestureShortcutPreferences;
  profile: OctopusClientAccessProfile;
  subscribers: Set<() => void>;
};

export function useGestureShortcutPreferences(profile: OctopusClientAccessProfile): GestureShortcutPreferences {
  const store = gestureStore(profile);
  return useSyncExternalStore(
    (subscriber) => subscribeStore(store, subscriber),
    () => store.current,
    () => DEFAULT_PREFERENCES,
  );
}

export function useSetGestureShortcutPreferences(profile: OctopusClientAccessProfile) {
  const store = gestureStore(profile);
  return useCallback((update: Partial<GestureShortcutPreferences> | ((current: GestureShortcutPreferences) => Partial<GestureShortcutPreferences>)) => (
    updateStore(store, typeof update === "function" ? update(store.current) : update)
  ), [store]);
}

export function defaultAirHandGestureCommand(gesture: AirHandGestureId): GestureCommandId {
  return DEFAULT_PREFERENCES.mappings[gesture];
}

export function emitAirHandGesture(gesture: AirHandGestureId, source: GestureEventSource): void {
  window.dispatchEvent(new CustomEvent(GESTURE_EVENT, { detail: { gesture, source } }));
}

export function emitGestureInteraction(interaction: GestureInteraction, source: GestureEventSource): void {
  window.dispatchEvent(new CustomEvent(INTERACTION_EVENT, { detail: { interaction, source } }));
}

export function subscribeGestureInteractions(listener: (interaction: GestureInteraction, source: GestureEventSource) => void): () => void {
  const handle = (event: Event) => {
    if (!(event instanceof CustomEvent)) return;
    const interaction = event.detail?.interaction as GestureInteraction | undefined;
    const source = event.detail?.source;
    if (!interaction || !Object.prototype.hasOwnProperty.call(INTERACTION_TYPES, interaction.type)) return;
    if (source !== "camera" && source !== "test") return;
    listener(interaction, source);
  };
  window.addEventListener(INTERACTION_EVENT, handle);
  return () => window.removeEventListener(INTERACTION_EVENT, handle);
}

export function subscribeAirHandGestures(listener: (gesture: AirHandGestureId, source: GestureEventSource) => void): () => void {
  const handle = (event: Event) => {
    if (!(event instanceof CustomEvent)) return;
    const gesture = event.detail?.gesture;
    const source = event.detail?.source;
    if (!airHandGestures.some((candidate) => candidate.id === gesture)) return;
    if (source !== "camera" && source !== "test") return;
    listener(gesture, source);
  };
  window.addEventListener(GESTURE_EVENT, handle);
  return () => window.removeEventListener(GESTURE_EVENT, handle);
}

function gestureStore(profile: OctopusClientAccessProfile): GestureStore {
  const existing = stores.get(profile);
  if (existing) return existing;
  const created = { current: readPreferences(profile), profile, subscribers: new Set<() => void>() };
  stores.set(profile, created);
  return created;
}

function subscribeStore(store: GestureStore, subscriber: () => void): () => void {
  store.subscribers.add(subscriber);
  return () => {
    store.subscribers.delete(subscriber);
  };
}

function updateStore(store: GestureStore, partial: Partial<GestureShortcutPreferences>): GestureShortcutPreferences {
  const mappings = normalizeMappings(partial.mappings ?? store.current.mappings);
  store.current = Object.freeze({
    enabled: typeof partial.enabled === "boolean" ? partial.enabled : store.current.enabled,
    cameraDeviceId: typeof partial.cameraDeviceId === "string" ? partial.cameraDeviceId : store.current.cameraDeviceId,
    mirrorX: typeof partial.mirrorX === "boolean" ? partial.mirrorX : store.current.mirrorX,
    sensitivity: normalizeSensitivity(partial.sensitivity ?? store.current.sensitivity),
    spatialControl: typeof partial.spatialControl === "boolean" ? partial.spatialControl : store.current.spatialControl,
    tourCompleted: typeof partial.tourCompleted === "boolean" ? partial.tourCompleted : store.current.tourCompleted,
    mappings,
    removedBuiltins: Object.freeze(normalizeRemoved(partial.removedBuiltins ?? store.current.removedBuiltins)),
  });
  try {
    window.localStorage.setItem(storageKey(store.profile), JSON.stringify(store.current));
  } catch {
    void 0;
  }
  for (const subscriber of [...store.subscribers]) subscriber();
  return store.current;
}

function readPreferences(profile: OctopusClientAccessProfile): GestureShortcutPreferences {
  try {
    const raw = window.localStorage.getItem(storageKey(profile));
    if (!raw) return DEFAULT_PREFERENCES;
    const value = JSON.parse(raw) as Partial<GestureShortcutPreferences>;
    return Object.freeze({
      enabled: value.enabled === true,
      cameraDeviceId: typeof value.cameraDeviceId === "string" ? value.cameraDeviceId : "",
      mirrorX: value.mirrorX !== false,
      sensitivity: normalizeSensitivity(value.sensitivity),
      spatialControl: value.spatialControl !== false,
      tourCompleted: value.tourCompleted === true,
      mappings: normalizeMappings(value.mappings),
      removedBuiltins: Object.freeze(normalizeRemoved(value.removedBuiltins)),
    });
  } catch {
    return DEFAULT_PREFERENCES;
  }
}

function normalizeRemoved(value: unknown): AirHandGestureId[] {
  if (!Array.isArray(value)) return [];
  const allowed = new Set(airHandGestures.map((gesture) => gesture.id));
  return value.filter((id): id is AirHandGestureId => typeof id === "string" && allowed.has(id as AirHandGestureId));
}

function normalizeSensitivity(value: unknown): GestureSensitivity {
  return value === "responsive" || value === "deliberate" ? value : "balanced";
}

function normalizeMappings(value: unknown): GestureShortcutPreferences["mappings"] {
  const candidate = value && typeof value === "object" && !Array.isArray(value)
    ? value as Partial<Record<AirHandGestureId, GestureCommandId>>
    : {};
  const allowed = new Set<GestureCommandId>(["none", ...clientCommands.map((command) => command.id)]);
  return Object.freeze(Object.fromEntries(airHandGestures.map(({ id }) => {
    const mapping = candidate[id];
    return [id, mapping && allowed.has(mapping) ? mapping : DEFAULT_PREFERENCES.mappings[id]];
  })) as Record<AirHandGestureId, GestureCommandId>);
}

function storageKey(profile: OctopusClientAccessProfile): string {
  return profile === "live" ? STORAGE_KEY : `${STORAGE_KEY}.test`;
}
