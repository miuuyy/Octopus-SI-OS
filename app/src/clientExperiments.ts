import { useCallback, useSyncExternalStore } from "react";
import type { OctopusClientAccessProfile } from "./electron";

export type ClientExperimentId = "gesture_feedback" | "home_whats_new" | "shortcut_coach";

export type ClientExperiment = Readonly<{
  id: ClientExperimentId;
  title: string;
  description: string;
  question: string;
  signalLabel: string;
}>;

export const clientExperiments: readonly ClientExperiment[] = [
  {
    id: "gesture_feedback",
    title: "Gesture feedback",
    description: "Briefly shows which command a swipe or fist just ran.",
    question: "Does gesture control feel steadier when every command is visible?",
    signalLabel: "commands shown",
  },
  {
    id: "home_whats_new",
    title: "What's new on Home",
    description: "Introduces one new capability at a time with a card on Home.",
    question: "Do people try a new capability sooner when Home offers it?",
    signalLabel: "cards acted on",
  },
  {
    id: "shortcut_coach",
    title: "Shortcut coach",
    description: "After a command is opened from the menu three times, shows its keyboard shortcut.",
    question: "Do menu commands move to the keyboard after one quiet hint?",
    signalLabel: "hints shown",
  },
];

export type ClientExperimentState = Readonly<{
  disabled: readonly ClientExperimentId[];
  signals: Readonly<Partial<Record<ClientExperimentId, number>>>;
  dismissedAnnouncements: readonly string[];
  menuCommandUses: Readonly<Record<string, number>>;
}>;

const EMPTY_STATE: ClientExperimentState = Object.freeze({ disabled: [], signals: {}, dismissedAnnouncements: [], menuCommandUses: {} });
const STORAGE_KEY = "octopus.client-experiments.v1";
const stores = new Map<OctopusClientAccessProfile, ExperimentStore>();

type ExperimentStore = {
  current: ClientExperimentState;
  profile: OctopusClientAccessProfile;
  subscribers: Set<() => void>;
};

export function useClientExperimentState(profile: OctopusClientAccessProfile): ClientExperimentState {
  const store = experimentStore(profile);
  return useSyncExternalStore(
    (subscriber) => {
      store.subscribers.add(subscriber);
      return () => {
        store.subscribers.delete(subscriber);
      };
    },
    () => store.current,
    () => EMPTY_STATE,
  );
}

export function useClientExperiment(profile: OctopusClientAccessProfile, id: ClientExperimentId): boolean {
  return !useClientExperimentState(profile).disabled.includes(id);
}

export function clientExperimentEnabled(profile: OctopusClientAccessProfile, id: ClientExperimentId): boolean {
  return !experimentStore(profile).current.disabled.includes(id);
}

export function useSetClientExperiment(profile: OctopusClientAccessProfile) {
  return useCallback((id: ClientExperimentId, enabled: boolean) => {
    const store = experimentStore(profile);
    updateStore(store, {
      disabled: enabled ? store.current.disabled.filter((candidate) => candidate !== id) : [...new Set([...store.current.disabled, id])],
    });
  }, [profile]);
}

export function recordClientExperimentSignal(profile: OctopusClientAccessProfile, id: ClientExperimentId): void {
  const store = experimentStore(profile);
  updateStore(store, { signals: { ...store.current.signals, [id]: (store.current.signals[id] ?? 0) + 1 } });
}

export function dismissClientAnnouncement(profile: OctopusClientAccessProfile, id: string): void {
  const store = experimentStore(profile);
  updateStore(store, { dismissedAnnouncements: [...new Set([...store.current.dismissedAnnouncements, id])] });
}

export function recordMenuCommandUse(profile: OctopusClientAccessProfile, commandId: string): number {
  const store = experimentStore(profile);
  const uses = (store.current.menuCommandUses[commandId] ?? 0) + 1;
  updateStore(store, { menuCommandUses: { ...store.current.menuCommandUses, [commandId]: uses } });
  return uses;
}

function experimentStore(profile: OctopusClientAccessProfile): ExperimentStore {
  const existing = stores.get(profile);
  if (existing) return existing;
  const created: ExperimentStore = { current: readState(profile), profile, subscribers: new Set() };
  stores.set(profile, created);
  return created;
}

function updateStore(store: ExperimentStore, partial: Partial<ClientExperimentState>): void {
  store.current = Object.freeze({ ...store.current, ...partial });
  try {
    window.localStorage.setItem(`${STORAGE_KEY}:${store.profile}`, JSON.stringify(store.current));
  } catch {
    void 0;
  }
  for (const subscriber of [...store.subscribers]) subscriber();
}

function readState(profile: OctopusClientAccessProfile): ClientExperimentState {
  try {
    const raw = window.localStorage.getItem(`${STORAGE_KEY}:${profile}`);
    if (!raw) return EMPTY_STATE;
    const value = JSON.parse(raw) as Partial<ClientExperimentState>;
    return Object.freeze({
      disabled: Array.isArray(value.disabled) ? value.disabled.filter(isExperimentId) : [],
      signals: isRecord(value.signals) ? value.signals as ClientExperimentState["signals"] : {},
      dismissedAnnouncements: Array.isArray(value.dismissedAnnouncements) ? value.dismissedAnnouncements.filter((id) => typeof id === "string") : [],
      menuCommandUses: isRecord(value.menuCommandUses) ? value.menuCommandUses as ClientExperimentState["menuCommandUses"] : {},
    });
  } catch {
    return EMPTY_STATE;
  }
}

function isExperimentId(value: unknown): value is ClientExperimentId {
  return clientExperiments.some((experiment) => experiment.id === value);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
