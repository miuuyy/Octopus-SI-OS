import type { HTMLAttributes } from "react";
import type { OctopusClientAccessProfile } from "../../electron";
import type { GlyphName } from "../IconGlyphs";

export type SettingsSectionId =
  | "general"
  | "account"
  | "appearance"
  | "voice-audio"
  | "computer-use"
  | "gestures"
  | "shortcuts"
  | "access"
  | "extensions"
  | "experiments"
  | "connections"
  | "surfaces"
  | "worker-environments"
  | "about"
  | "test-lab";

export type SettingsSectionGroup = "Personal" | "Interaction" | "Octopus" | "Test";
export type SettingsDestinationKind = "setting" | "action" | "destination";

export type SettingsDestination = {
  id: string;
  sectionId: SettingsSectionId;
  label: string;
  description: string;
  kind: SettingsDestinationKind;
  terms?: readonly string[];
  profile?: OctopusClientAccessProfile;
  targetId?: string;
};

export type SettingsSectionDefinition = {
  id: SettingsSectionId;
  label: string;
  glyph: GlyphName;
  group: SettingsSectionGroup;
  profile?: OctopusClientAccessProfile;
};

export type SettingsSearchResult = {
  destination: SettingsDestination;
  section: SettingsSectionDefinition;
  score: number;
};

export type SettingsTargetAttributes = HTMLAttributes<HTMLElement> & {
  "data-settings-target": string;
};

export const SETTINGS_SECTION_GROUPS: readonly SettingsSectionGroup[] = ["Personal", "Interaction", "Octopus", "Test"];

export const SETTINGS_SECTIONS: readonly SettingsSectionDefinition[] = [
  { id: "general", label: "General", glyph: "settings", group: "Personal" },
  { id: "account", label: "Account", glyph: "user", group: "Personal", profile: "live" },
  { id: "appearance", label: "Appearance", glyph: "sun", group: "Personal" },
  { id: "voice-audio", label: "Voice", glyph: "microphone", group: "Interaction" },
  { id: "computer-use", label: "Computer use", glyph: "desktop", group: "Interaction", profile: "live" },
  { id: "gestures", label: "Gestures", glyph: "hand", group: "Interaction" },
  { id: "shortcuts", label: "Keyboard shortcuts", glyph: "keyboard", group: "Interaction" },
  { id: "access", label: "Access & Privacy", glyph: "dataControls", group: "Octopus" },
  { id: "extensions", label: "Extensions", glyph: "widgets", group: "Octopus" },
  { id: "experiments", label: "Experiments", glyph: "flask", group: "Octopus" },
  { id: "connections", label: "Connections", glyph: "interfaces", group: "Octopus", profile: "live" },
  { id: "surfaces", label: "Surfaces", glyph: "descriptor", group: "Octopus" },
  { id: "worker-environments", label: "Worker Environments", glyph: "ee", group: "Octopus" },
  { id: "about", label: "About & Diagnostics", glyph: "bug", group: "Octopus" },
  { id: "test-lab", label: "Test Lab", glyph: "flask", group: "Test", profile: "lab" },
];

export const SETTINGS_DESTINATIONS: readonly SettingsDestination[] = [
  destination("account.identity", "account", "ChatGPT account", "Review the native account email and subscription plan.", "destination", ["identity", "email", "subscription", "plan"], "live"),
  destination("account.usage", "account", "Usage limits", "Review remaining account limits, credits, and reset times.", "destination", ["rate limits", "weekly", "5-hour", "credits"], "live"),
  destination("account.logout", "account", "Log out of ChatGPT", "Sign out of the shared native account and require sign-in to continue.", "action", ["sign out", "sign in", "login"], "live"),
  destination("general.first-surface", "general", "First surface", "Choose the launch destination in Octopus.", "setting", ["launch", "startup", "open"]),
  destination("general.internal-runs", "general", "Internal runs", "Include read-only runs that have no exact client interface.", "setting", ["show internal runs"]),
  destination("general.task-pane-width", "general", "Task pane width", "Set the default width of the secondary task pane.", "setting", ["split", "resize"]),
  destination("general.send-shortcut", "general", "Send shortcut", "Choose when Enter sends a prompt or inserts a new line.", "setting", ["composer", "enter"]),
  destination("general.spell-check", "general", "Spell check", "Use the operating system dictionary in the task composer.", "setting", ["composer", "dictionary"]),
  destination("general.active-reply", "general", "While a run is active", "Send steers immediately. Right-click Send or use the queue shortcut to queue a message.", "setting", ["steer", "queue", "follow-up"]),
  destination("general.work-mode", "general", "Work mode", "Choose detailed or concise projected activity in task chats.", "setting", ["technical detail", "reasoning", "tools"]),
  destination("general.provider", "general", "Provider", "Choose the default declared provider interface for new tasks.", "setting", ["model", "codex", "interface"]),
  destination("general.on-launch", "general", "On launch", "Restore the last active surface when the client opens.", "destination", ["startup"]),
  destination("general.open-at-login", "general", "Open at login", "Launch the packaged Octopus client after operating-system sign-in.", "setting", ["startup", "macos"]),
  destination("general.keep-awake", "general", "Keep awake", "Prevent the device from sleeping while a run is executing.", "setting", ["sleep", "run"]),
  destination("notifications.mark-activity", "general", "Inbox notifications", "Notify about new messages, requests, and marks selected in Inbox.", "setting", ["notifications", "messages", "requests", "marks", "worker", "unfocused"]),
  destination("notifications.sound", "general", "Notification sound", "Choose the sound played with desktop notifications.", "setting", ["system", "silent", "activity"]),

  destination("appearance.theme", "appearance", "Theme", "Choose system, light, or dark appearance and edit its colors.", "setting", ["accent", "background", "foreground", "contrast"]),
  destination("appearance.preferences", "appearance", "Appearance preferences", "Configure motion, density, fonts, diff colors, and sidebar translucency.", "setting", ["font", "motion", "density", "translucent", "diff"], undefined, "appearance.theme"),
  destination("appearance.transfer", "appearance", "Copy or import theme", "Move a theme configuration between clients.", "action", ["export", "share"], undefined, "appearance.theme"),

  destination("voice.microphone", "voice-audio", "Microphone", "Choose the device used for dictation and Voice.", "setting", ["input", "recording"]),
  destination("voice.speaker", "voice-audio", "Speaker", "Choose the device used for voice replies and audio artifacts.", "setting", ["output", "playback"]),
  destination("voice.agent-voice", "voice-audio", "Agent voice", "Review voice availability declared by the speech interface.", "destination", ["tts", "speech"]),
  destination("voice.transcription-provider", "voice-audio", "Transcription provider", "Choose the client-local dictation provider.", "setting", ["speech to text", "codex"]),
  destination("voice.toggle-dictation", "voice-audio", "Toggle dictation", "Review the shortcut that starts or stops dictation.", "action", ["shortcut", "microphone"]),
  destination("voice.global-dictation", "voice-audio", "Global quick dictation", "Enable the desktop shortcut that opens the active composer and starts dictation.", "setting", ["hotkey"]),
  destination("voice.hold-dictate", "voice-audio", "Hold to dictate", "Review press-and-hold composer dictation behavior.", "destination", ["microphone"]),
  destination("voice.dictionary", "voice-audio", "Dictation dictionary", "Add replacements applied before a transcript enters the composer.", "setting", ["phrase", "replacement"]),
  destination("voice.floating-call", "voice-audio", "Floating call widget", "Choose whether an active call follows you outside Home.", "setting", ["voice", "home"]),
  destination("voice.incoming-audio", "voice-audio", "Incoming audio artifacts", "Choose how audio artifacts behave while a voice surface is open.", "setting", ["autoplay"]),
  destination("voice.recordings", "voice-audio", "Recent recordings", "Your last 20 recordings are saved on this device.", "destination", ["dictation", "history", "audio"]),

  destination("experiments.list", "experiments", "Experiments", "Turn the product experiments running in this client on or off.", "setting", ["beta", "labs", "try"]),

  destination("gestures.controls", "gestures", "Gesture controls", "Configure local camera-based gesture mappings and sensitivity.", "setting", ["camera", "hand", "mapping", "sensitivity"]),

  destination("shortcuts.bindings", "shortcuts", "Keyboard shortcut bindings", "Search, edit, reset, and resolve conflicts between client commands.", "setting", ["keys", "commands", "hotkey"]),
  destination("shortcuts.search-keys", "shortcuts", "Search by keystrokes", "Find commands by pressing their current shortcut.", "action", ["key capture"], undefined, "shortcuts.bindings"),

  destination("access.administrator", "access", "Administrator password", "Change the local Administrator credential and revoke other sessions.", "action", ["security", "login"]),
  destination("access.touch-id", "access", "Touch ID", "Enable or disable Touch ID for Administrator unlock on this Mac.", "action", ["security", "biometric", "fingerprint"]),
  destination("access.test", "access", "Test password", "Set the credential for the isolated Test profile.", "action", ["lab", "security"]),
  destination("access.camera", "access", "Camera", "Review or request camera access for gestures and client surfaces.", "action", ["permission"]),
  destination("access.microphone", "access", "Microphone", "Review or request microphone access for dictation and Voice.", "action", ["permission"]),
  destination("access.browser-permissions", "access", "Saved browser permissions", "Remove saved Always choices for file uploads and browsing history.", "action", ["browser", "upload", "history", "approval"], "live"),
  destination("computer-use.setup", "computer-use", "Computer use", "Enable application access and set up macOS permissions.", "action", ["computer use", "screen recording", "accessibility", "permission"], "live"),
  destination("access.application-permissions", "computer-use", "Always-allowed apps", "Remove saved Always access through the current Computer Use host.", "action", ["computer use", "application", "approval"], "live"),
  destination("access.clear", "access", "Clear local client data", "Remove profile-local preferences, layouts, drafts, and Test fixtures.", "action", ["reset", "delete"]),
  destination("access.export", "access", "Export local settings", "Save profile-owned client settings as versioned JSON.", "action", ["backup", "json"]),
  destination("access.import", "access", "Import local settings", "Validate and merge a settings export for the exact current profile.", "action", ["restore", "json"]),

  destination("extensions.catalog", "extensions", "Extension catalog", "Search installed and available client widgets, tabs, and panels.", "destination", ["widgets", "tabs", "panels"]),
  destination("extensions.sources", "extensions", "Extension sources", "Configure explicit client-readable sources for an extension.", "setting", ["filesystem", "projection", "device"], undefined, "extensions.catalog"),
  destination("extensions.install", "extensions", "Install or remove an extension", "Review detected packages and manage client-only installation.", "action", ["uninstall", "setup"], undefined, "extensions.catalog"),

  destination("connections.endpoint", "connections", "Runtime endpoint", "Validate and save the endpoint used for chat artifact submission.", "setting", ["connection", "localhost", "url"], "live"),
  destination("connections.providers", "connections", "Declared provider interfaces", "Review provider descriptors projected into this client.", "destination", ["model", "registry"], "live"),
  destination("connections.facts", "connections", "Application facts", "Review paths and process facts reported by the desktop client.", "destination", ["home", "registry", "ledger"], "live"),

  destination("surfaces.sidebar", "surfaces", "Dashboard sidebar", "Show or hide built-in and admitted system surfaces.", "setting", ["navigation", "visibility"]),
  destination("surfaces.registry", "surfaces", "Client surface registry", "Review admitted client documents and their provenance.", "destination", ["schema", "revision", "digest"], undefined, "surfaces.sidebar"),
  destination("surfaces.transport", "surfaces", "Document transport", "Review the source directory or projection used for client documents.", "destination", ["path", "copy"], undefined, "surfaces.sidebar"),

  destination("worker-environments.default", "worker-environments", "Default surface", "Choose the first projected Worker Environment surface.", "setting", ["browser", "changes"]),
  destination("worker-environments.restore", "worker-environments", "Restore tabs and layout", "Remember tabs, ordering, preview state, and split size per Worker Environment.", "setting", ["persistence"]),
  destination("worker-environments.preview", "worker-environments", "File preview tabs", "Reuse a preview tab for a single-clicked file.", "setting", ["single click", "pin"]),
  destination("worker-environments.browser-control", "worker-environments", "Manual browser control", "Review the confirmation boundary for live CDP control.", "destination", ["cdp", "safety"]),
  destination("worker-environments.direct-viewers", "worker-environments", "Direct viewers", "Review descriptor-bounded filesystem, terminal, and desktop viewers.", "destination", ["files", "terminal", "desktop"]),

  destination("about.update-channel", "about", "Update channel", "Review whether this client is a packaged or development build.", "destination", ["version", "release"]),
  destination("about.licenses", "about", "Open-source licenses", "Open production dependency licenses and notices.", "action", ["dependencies", "notices"]),
  destination("about.diagnostics", "about", "Diagnostics report", "Review and save diagnostics or report a problem on GitHub.", "action", ["support", "debug", "help"]),
  destination("about.logs", "about", "Logs", "Open the local rotating client log directory.", "action", ["debug", "jsonl"]),
  destination("about.welcome", "about", "Welcome window", "Show the welcome flow again on the next launch.", "action", ["onboarding", "reset"]),

  destination("test.fixtures", "test-lab", "Projection fixtures", "Review isolated Test projection fixtures.", "destination", ["fake", "local"], "lab"),
  destination("test.widgets", "test-lab", "Built-in widget documents", "Review Test-only admitted widget documents.", "destination", ["surface"], "lab"),
  destination("test.voice", "test-lab", "Synthetic voice activity", "Review deterministic Test voice activity.", "destination", ["speaking", "thinking"], "lab"),
  destination("test.channel", "test-lab", "Lab release channel", "Review admission of Test-only client documents.", "destination", ["surface", "schema"], "lab"),
  destination("test.worker-environments", "test-lab", "Worker environment fixtures", "Review deterministic projected Worker Environment records.", "destination", ["browser", "terminal", "desktop"], "lab"),
  destination("test.reset", "test-lab", "Reset test state", "Clear Test-only layouts, preferences, schedules, and gestures.", "action", ["delete", "fixtures"], "lab"),
];

const SECTION_BY_ID = new Map(SETTINGS_SECTIONS.map((section) => [section.id, section]));
const DESTINATION_BY_ID = new Map(SETTINGS_DESTINATIONS.map((destination) => [destination.id, destination]));

export function availableSettingsSections(profile: OctopusClientAccessProfile): SettingsSectionDefinition[] {
  return SETTINGS_SECTIONS.filter((section) => section.profile === undefined || section.profile === profile);
}

export function searchSettings(query: string, profile: OctopusClientAccessProfile): SettingsSearchResult[] {
  const normalizedQuery = normalize(query);
  if (!normalizedQuery) return [];
  const tokens = normalizedQuery.split(" ").filter(Boolean);
  return SETTINGS_DESTINATIONS.flatMap((destination) => {
    if (destination.profile && destination.profile !== profile) return [];
    const section = SECTION_BY_ID.get(destination.sectionId);
    if (!section || (section.profile && section.profile !== profile)) return [];
    const label = normalize(destination.label);
    const sectionLabel = normalize(section.label);
    const body = normalize([destination.label, destination.description, section.label, ...(destination.terms ?? [])].join(" "));
    if (!tokens.every((token) => body.includes(token))) return [];
    const score = (label === normalizedQuery ? 1_000 : 0)
      + (label.startsWith(normalizedQuery) ? 500 : 0)
      + (label.includes(normalizedQuery) ? 250 : 0)
      + (sectionLabel.includes(normalizedQuery) ? 100 : 0)
      + tokens.reduce((total, token) => total + (label.includes(token) ? 20 : 5), 0);
    return [{ destination, section, score }];
  }).sort((left, right) => right.score - left.score
    || sectionOrder(left.section.id) - sectionOrder(right.section.id)
    || left.destination.label.localeCompare(right.destination.label));
}

export function settingsDestination(id: string): SettingsDestination {
  const found = DESTINATION_BY_ID.get(id);
  if (!found) throw new Error(`Unknown settings destination: ${id}`);
  return found;
}

export function settingsTargetProps(id: string): SettingsTargetAttributes {
  settingsDestination(id);
  return {
    "data-settings-target": id,
    id: settingsTargetDomId(id),
    tabIndex: -1,
  };
}

export function settingsTargetDomId(id: string): string {
  return `settings-target-${id.replace(/[^a-z0-9_-]+/gi, "-")}`;
}

function destination(
  id: string,
  sectionId: SettingsSectionId,
  label: string,
  description: string,
  kind: SettingsDestinationKind,
  terms?: readonly string[],
  profile?: OctopusClientAccessProfile,
  targetId?: string,
): SettingsDestination {
  return { id, sectionId, label, description, kind, terms, profile, targetId };
}

function normalize(value: string): string {
  return value.normalize("NFKD").toLocaleLowerCase().replace(/[^\p{L}\p{N}]+/gu, " ").trim();
}

function sectionOrder(id: SettingsSectionId): number {
  return SETTINGS_SECTIONS.findIndex((section) => section.id === id);
}
