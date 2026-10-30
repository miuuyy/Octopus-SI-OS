import {
  createContext,
  createElement,
  useCallback,
  useContext,
  useSyncExternalStore,
  type ReactNode,
} from "react";
import type { OctopusClientAccessProfile } from "./electron";

export type SendShortcut = "enter" | "cmd-if-multiline" | "cmd-always";
export type MarkNotifications = "never" | "unfocused" | "always";
export type MarkActivityMode = "attention" | "all" | "custom";
export type NotificationSound = "system" | "silent";
export type WorkerEnvironmentDefaultPanel = "auto" | "browser" | "changes";
export type IncomingAudioArtifactBehavior = "ask" | "play" | "never";
export type DictationProvider = "codex";
export type TechnicalDetailLevel = "detailed" | "concise";

export type DictationHistoryEntry = Readonly<{
  id: string;
  text: string;
  createdAtMs: number;
}>;

export type DictationDictionaryEntry = Readonly<{
  id: string;
  phrase: string;
  replacement: string;
}>;

export type ClientPreferences = Readonly<{
  sendShortcut: SendShortcut;
  showInternalRuns: boolean;
  startupSurfaceId: string;
  secondaryTaskPaneWidthPercent: number;
  spellcheckEnabled: boolean;
  markNotifications: MarkNotifications;
  markActivityMode: MarkActivityMode;
  markActivityTypes: readonly string[] | null;
  notificationSound: NotificationSound;
  notifyOperatorInputRequired: boolean;
  notifyBlockedOrDeliveryFailed: boolean;
  notifyWorkerEnvironmentDisconnected: boolean;
  notifyVoiceSession: boolean;
  keepAwakeDuringRuns: boolean;
  restoreWorkerEnvironmentLayouts: boolean;
  previewWorkerEnvironmentFiles: boolean;
  workerEnvironmentDefaultPanel: WorkerEnvironmentDefaultPanel;
  settingsSection: string;
  settingsSearch: string;
  floatingCallPuck: boolean;
  globalQuickDictation: boolean;
  dictationProvider: DictationProvider;
  audioInputDeviceId: string;
  audioOutputDeviceId: string;
  incomingAudioArtifactBehavior: IncomingAudioArtifactBehavior;
  technicalDetailLevel: TechnicalDetailLevel;
  dictationDictionary: readonly DictationDictionaryEntry[];
  dictationHistory: readonly DictationHistoryEntry[];
  hiddenSurfaceIds: readonly string[];
}>;

export const clientSettingsSections = [
  "general",
  "account",
  "appearance",
  "voice-audio",
  "computer-use",
  "gestures",
  "shortcuts",
  "access",
  "extensions",
  "experiments",
  "connections",
  "surfaces",
  "worker-environments",
  "about",
  "test-lab",
] as const;

export const defaultClientPreferences: ClientPreferences = Object.freeze({
  sendShortcut: "enter",
  showInternalRuns: false,
  startupSurfaceId: "voice",
  secondaryTaskPaneWidthPercent: 34,
  spellcheckEnabled: true,
  markNotifications: "unfocused",
  markActivityMode: "attention",
  markActivityTypes: null,
  notificationSound: "system",
  notifyOperatorInputRequired: true,
  notifyBlockedOrDeliveryFailed: true,
  notifyWorkerEnvironmentDisconnected: true,
  notifyVoiceSession: false,
  keepAwakeDuringRuns: false,
  restoreWorkerEnvironmentLayouts: true,
  previewWorkerEnvironmentFiles: true,
  workerEnvironmentDefaultPanel: "auto",
  settingsSection: "general",
  settingsSearch: "",
  floatingCallPuck: true,
  globalQuickDictation: false,
  dictationProvider: "codex",
  audioInputDeviceId: "default",
  audioOutputDeviceId: "default",
  incomingAudioArtifactBehavior: "ask",
  technicalDetailLevel: "detailed",
  dictationDictionary: [],
  dictationHistory: [],
  hiddenSurfaceIds: [],
});

const STORAGE_KEY = "octopus.client-preferences.v1";
const sendShortcuts: readonly SendShortcut[] = ["enter", "cmd-if-multiline", "cmd-always"];
const markNotificationModes: readonly MarkNotifications[] = [
  "never",
  "unfocused",
  "always",
];
const notificationSounds: readonly NotificationSound[] = ["system", "silent"];
const workerEnvironmentDefaultPanels: readonly WorkerEnvironmentDefaultPanel[] = ["auto", "browser", "changes"];
const incomingAudioArtifactBehaviors: readonly IncomingAudioArtifactBehavior[] = ["ask", "play", "never"];
const dictationProviders: readonly DictationProvider[] = ["codex"];
const technicalDetailLevels: readonly TechnicalDetailLevel[] = ["detailed", "concise"];
type ClientPreferencesStore = {
  readonly profile: OctopusClientAccessProfile;
  current: ClientPreferences;
  readonly subscribers: Set<() => void>;
};

const stores = new Map<OctopusClientAccessProfile, ClientPreferencesStore>();
let clientPreferencesStorageListenerReady = false;
const ClientPreferencesContext = createContext<ClientPreferencesStore | null>(null);

export function normalizeSettingsSection(value: unknown): string {
  return clientSettingsSections.includes(value as (typeof clientSettingsSections)[number])
    ? (value as string)
    : defaultClientPreferences.settingsSection;
}

function normalizeChoice<T extends string>(
  value: unknown,
  allowed: readonly T[],
  fallback: T,
): T {
  return allowed.includes(value as T) ? (value as T) : fallback;
}

function normalizeStartupSurfaceId(value: unknown, fallback: string): string {
  const surfaceId = typeof value === "string" ? value.trim() : "";
  return surfaceId ? surfaceId.slice(0, 256) : fallback;
}

function normalizeBoolean(value: unknown, fallback: boolean): boolean {
  return typeof value === "boolean" ? value : fallback;
}

function normalizeSecondaryTaskPaneWidth(value: unknown): number {
  return typeof value === "number" && Number.isFinite(value)
    ? Math.round(Math.max(28, Math.min(68, value)))
    : defaultClientPreferences.secondaryTaskPaneWidthPercent;
}

function normalizeStringList(value: unknown): readonly string[] {
  if (!Array.isArray(value)) return [];
  return Object.freeze([...new Set(value.flatMap((item) => (
    typeof item === "string" && item.trim() ? [item.trim()] : []
  )))]);
}

function normalizeDictationHistory(value: unknown): readonly DictationHistoryEntry[] {
  if (!Array.isArray(value)) return [];
  const seen = new Set<string>();
  return Object.freeze(value.flatMap((item) => {
    if (!item || typeof item !== "object" || Array.isArray(item)) return [];
    const candidate = item as Partial<DictationHistoryEntry>;
    const id = typeof candidate.id === "string" ? candidate.id.trim() : "";
    const text = typeof candidate.text === "string" ? candidate.text.trim() : "";
    const createdAtMs = typeof candidate.createdAtMs === "number" && Number.isFinite(candidate.createdAtMs)
      ? Math.max(0, Math.round(candidate.createdAtMs))
      : 0;
    if (!id || !text || !createdAtMs || seen.has(id)) return [];
    seen.add(id);
    return [Object.freeze({ id, text, createdAtMs })];
  }).slice(0, 30));
}

function normalizeDictationDictionary(value: unknown): readonly DictationDictionaryEntry[] {
  if (!Array.isArray(value)) return [];
  const seen = new Set<string>();
  return Object.freeze(value.flatMap((item) => {
    if (!item || typeof item !== "object" || Array.isArray(item)) return [];
    const candidate = item as Partial<DictationDictionaryEntry>;
    const id = typeof candidate.id === "string" ? candidate.id.trim() : "";
    const phrase = typeof candidate.phrase === "string" ? candidate.phrase.trim().slice(0, 120) : "";
    const replacement = typeof candidate.replacement === "string" ? candidate.replacement.trim().slice(0, 120) : "";
    const identity = phrase.toLocaleLowerCase();
    if (!id || !phrase || !replacement || seen.has(identity)) return [];
    seen.add(identity);
    return [Object.freeze({ id, phrase, replacement })];
  }).slice(0, 100));
}

export function applyDictationDictionary(
  transcript: string,
  dictionary: readonly DictationDictionaryEntry[],
): string {
  return [...dictionary]
    .sort((left, right) => right.phrase.length - left.phrase.length)
    .reduce((current, entry) => {
      const escaped = entry.phrase.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
      const pattern = new RegExp(`(^|[^\\p{L}\\p{N}_])${escaped}(?=$|[^\\p{L}\\p{N}_])`, "giu");
      return current.replace(pattern, (_match, prefix: string) => `${prefix}${entry.replacement}`);
    }, transcript);
}

function normalizePreferences(value: unknown, transientSearch = ""): ClientPreferences {
  const candidate =
    typeof value === "object" && value !== null && !Array.isArray(value)
      ? (value as Partial<ClientPreferences>)
      : {};
  const rawCandidate = candidate as Record<string, unknown>;
  const rawSendShortcut = rawCandidate.sendShortcut;

  return Object.freeze({
    sendShortcut: normalizeChoice(
      rawSendShortcut === "cmd-enter" ? "cmd-always" : rawSendShortcut,
      sendShortcuts,
      defaultClientPreferences.sendShortcut,
    ),
    showInternalRuns: normalizeBoolean(
      candidate.showInternalRuns,
      defaultClientPreferences.showInternalRuns,
    ),
    startupSurfaceId: normalizeStartupSurfaceId(candidate.startupSurfaceId, defaultClientPreferences.startupSurfaceId),
    secondaryTaskPaneWidthPercent: normalizeSecondaryTaskPaneWidth(candidate.secondaryTaskPaneWidthPercent),
    spellcheckEnabled: normalizeBoolean(
      candidate.spellcheckEnabled,
      defaultClientPreferences.spellcheckEnabled,
    ),
    markNotifications: normalizeChoice(
      candidate.markNotifications,
      markNotificationModes,
      defaultClientPreferences.markNotifications,
    ),
    markActivityTypes: candidate.markActivityTypes == null
      ? candidate.markActivityMode === "custom" ? [] : null
      : normalizeStringList(candidate.markActivityTypes),
    markActivityMode: normalizeChoice(candidate.markActivityMode, ["attention", "all", "custom"] as const,
      Array.isArray(candidate.markActivityTypes) ? "custom" : "attention"),
    notificationSound: normalizeChoice(
      candidate.notificationSound,
      notificationSounds,
      defaultClientPreferences.notificationSound,
    ),
    notifyOperatorInputRequired: normalizeBoolean(
      candidate.notifyOperatorInputRequired,
      defaultClientPreferences.notifyOperatorInputRequired,
    ),
    notifyBlockedOrDeliveryFailed: normalizeBoolean(
      candidate.notifyBlockedOrDeliveryFailed,
      defaultClientPreferences.notifyBlockedOrDeliveryFailed,
    ),
    notifyWorkerEnvironmentDisconnected: normalizeBoolean(
      candidate.notifyWorkerEnvironmentDisconnected,
      defaultClientPreferences.notifyWorkerEnvironmentDisconnected,
    ),
    notifyVoiceSession: normalizeBoolean(
      candidate.notifyVoiceSession,
      defaultClientPreferences.notifyVoiceSession,
    ),
    keepAwakeDuringRuns: normalizeBoolean(
      candidate.keepAwakeDuringRuns,
      defaultClientPreferences.keepAwakeDuringRuns,
    ),
    restoreWorkerEnvironmentLayouts: normalizeBoolean(
      candidate.restoreWorkerEnvironmentLayouts,
      defaultClientPreferences.restoreWorkerEnvironmentLayouts,
    ),
    previewWorkerEnvironmentFiles: normalizeBoolean(
      candidate.previewWorkerEnvironmentFiles,
      defaultClientPreferences.previewWorkerEnvironmentFiles,
    ),
    workerEnvironmentDefaultPanel: normalizeChoice(
      candidate.workerEnvironmentDefaultPanel,
      workerEnvironmentDefaultPanels,
      defaultClientPreferences.workerEnvironmentDefaultPanel,
    ),
    settingsSection: normalizeSettingsSection(candidate.settingsSection),
    settingsSearch:
      typeof candidate.settingsSearch === "string" ? candidate.settingsSearch : transientSearch,
    floatingCallPuck: normalizeBoolean(
      candidate.floatingCallPuck,
      defaultClientPreferences.floatingCallPuck,
    ),
    globalQuickDictation: normalizeBoolean(
      candidate.globalQuickDictation,
      defaultClientPreferences.globalQuickDictation,
    ),
    dictationProvider: normalizeChoice(
      candidate.dictationProvider,
      dictationProviders,
      defaultClientPreferences.dictationProvider,
    ),
    audioInputDeviceId: typeof candidate.audioInputDeviceId === "string" && candidate.audioInputDeviceId.trim()
      ? candidate.audioInputDeviceId.trim()
      : defaultClientPreferences.audioInputDeviceId,
    audioOutputDeviceId: typeof candidate.audioOutputDeviceId === "string" && candidate.audioOutputDeviceId.trim()
      ? candidate.audioOutputDeviceId.trim()
      : defaultClientPreferences.audioOutputDeviceId,
    incomingAudioArtifactBehavior: normalizeChoice(
      candidate.incomingAudioArtifactBehavior,
      incomingAudioArtifactBehaviors,
      defaultClientPreferences.incomingAudioArtifactBehavior,
    ),
    technicalDetailLevel: normalizeChoice(
      candidate.technicalDetailLevel,
      technicalDetailLevels,
      defaultClientPreferences.technicalDetailLevel,
    ),
    dictationDictionary: normalizeDictationDictionary(candidate.dictationDictionary),
    dictationHistory: normalizeDictationHistory(candidate.dictationHistory),
    hiddenSurfaceIds: normalizeStringList(
      candidate.hiddenSurfaceIds ?? (candidate as Record<string, unknown>).hiddenOsSurfaceIds,
    ),
  });
}

function storageKey(profile: OctopusClientAccessProfile): string {
  return profile === "live" ? STORAGE_KEY : `${STORAGE_KEY}.test`;
}

function loadClientPreferences(profile: OctopusClientAccessProfile): ClientPreferences {
  try {
    if (typeof window === "undefined") return defaultClientPreferences;
    const raw = window.localStorage.getItem(storageKey(profile));
    if (!raw) return defaultClientPreferences;
    const parsed = JSON.parse(raw) as unknown;
    const loaded = normalizePreferences(parsed);
    if (typeof parsed === "object" && parsed !== null && !Array.isArray(parsed)) {
      const stored = parsed as Record<string, unknown>;
      if ("startupSurface" in stored || "startupAgentSurface" in stored || "hiddenOsSurfaceIds" in stored) {
        const { startupSurface, startupAgentSurface, hiddenOsSurfaceIds, ...current } = stored;
        void startupSurface;
        void startupAgentSurface;
        void hiddenOsSurfaceIds;
        try {
          window.localStorage.setItem(storageKey(profile), JSON.stringify({
            ...current,
            startupSurfaceId: loaded.startupSurfaceId,
            hiddenSurfaceIds: loaded.hiddenSurfaceIds,
          }));
        } catch {
          void 0;
        }
      }
    }
    return Object.freeze({ ...loaded, settingsSearch: "" });
  } catch {
    return defaultClientPreferences;
  }
}

function clientPreferencesStore(profile: OctopusClientAccessProfile): ClientPreferencesStore {
  ensureClientPreferencesStorageListener();
  const existing = stores.get(profile);
  if (existing) return existing;
  const created: ClientPreferencesStore = {
    profile,
    current: loadClientPreferences(profile),
    subscribers: new Set(),
  };
  stores.set(profile, created);
  return created;
}

function ensureClientPreferencesStorageListener(): void {
  if (clientPreferencesStorageListenerReady || typeof window === "undefined") return;
  clientPreferencesStorageListenerReady = true;
  window.addEventListener("storage", (event) => {
    if (event.storageArea !== window.localStorage || !event.key) return;
    for (const store of stores.values()) {
      if (event.key !== storageKey(store.profile)) continue;
      const loaded = loadClientPreferences(store.profile);
      const next = Object.freeze({ ...loaded, settingsSearch: store.current.settingsSearch });
      if (JSON.stringify(next) === JSON.stringify(store.current)) continue;
      store.current = next;
      for (const subscriber of [...store.subscribers]) subscriber();
    }
  });
}

function updateClientPreferences(
  store: ClientPreferencesStore,
  partial: Partial<ClientPreferences>,
): ClientPreferences {
  store.current = normalizePreferences(
    { ...store.current, ...partial },
    store.current.settingsSearch,
  );

  try {
    if (typeof window !== "undefined") {
      window.localStorage.setItem(
        storageKey(store.profile),
        JSON.stringify({
          sendShortcut: store.current.sendShortcut,
          showInternalRuns: store.current.showInternalRuns,
          startupSurfaceId: store.current.startupSurfaceId,
          secondaryTaskPaneWidthPercent: store.current.secondaryTaskPaneWidthPercent,
          spellcheckEnabled: store.current.spellcheckEnabled,
          markNotifications: store.current.markNotifications,
          markActivityMode: store.current.markActivityMode,
          markActivityTypes: store.current.markActivityTypes,
          notificationSound: store.current.notificationSound,
          notifyOperatorInputRequired: store.current.notifyOperatorInputRequired,
          notifyBlockedOrDeliveryFailed: store.current.notifyBlockedOrDeliveryFailed,
          notifyWorkerEnvironmentDisconnected: store.current.notifyWorkerEnvironmentDisconnected,
          notifyVoiceSession: store.current.notifyVoiceSession,
          keepAwakeDuringRuns: store.current.keepAwakeDuringRuns,
          restoreWorkerEnvironmentLayouts: store.current.restoreWorkerEnvironmentLayouts,
          previewWorkerEnvironmentFiles: store.current.previewWorkerEnvironmentFiles,
          workerEnvironmentDefaultPanel: store.current.workerEnvironmentDefaultPanel,
          settingsSection: store.current.settingsSection,
          floatingCallPuck: store.current.floatingCallPuck,
          globalQuickDictation: store.current.globalQuickDictation,
          dictationProvider: store.current.dictationProvider,
          audioInputDeviceId: store.current.audioInputDeviceId,
          audioOutputDeviceId: store.current.audioOutputDeviceId,
          incomingAudioArtifactBehavior: store.current.incomingAudioArtifactBehavior,
          technicalDetailLevel: store.current.technicalDetailLevel,
          dictationDictionary: store.current.dictationDictionary,
          dictationHistory: store.current.dictationHistory,
          hiddenSurfaceIds: store.current.hiddenSurfaceIds,
        }),
      );
    }
  } catch {
    void 0;
  }

  for (const subscriber of [...store.subscribers]) subscriber();
  return store.current;
}

function subscribeClientPreferences(
  store: ClientPreferencesStore,
  subscriber: () => void,
): () => void {
  store.subscribers.add(subscriber);
  return () => {
    store.subscribers.delete(subscriber);
  };
}

export function ClientPreferencesProvider({
  children,
  profile,
}: {
  children: ReactNode;
  profile: OctopusClientAccessProfile;
}) {
  return createElement(
    ClientPreferencesContext.Provider,
    { value: clientPreferencesStore(profile) },
    children,
  );
}

function useClientPreferencesStore(): ClientPreferencesStore {
  const store = useContext(ClientPreferencesContext);
  if (!store) throw new Error("ClientPreferencesProvider is required for authenticated client surfaces.");
  return store;
}

export function useClientPreferences(): ClientPreferences {
  const store = useClientPreferencesStore();
  return useSyncExternalStore(
    (subscriber) => subscribeClientPreferences(store, subscriber),
    () => store.current,
    () => defaultClientPreferences,
  );
}

export function useSetClientPreferences(): (
  partial: Partial<ClientPreferences>,
) => ClientPreferences {
  const store = useClientPreferencesStore();
  return useCallback((partial) => updateClientPreferences(store, partial), [store]);
}
