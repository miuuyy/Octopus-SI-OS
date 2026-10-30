export type ClientCommandId =
  | "new_chat"
  | "open_voice"
  | "open_settings"
  | "open_command_menu"
  | "focus_search"
  | "focus_composer"
  | "composer.queue"
  | "composer.steer"
  | "scroll_to_latest"
  | "navigate_back"
  | "navigate_forward"
  | "previous_surface"
  | "next_surface"
  | "toggle_sidebar"
  | "toggle_split"
  | "toggle_task_summary"
  | "open_terminal"
  | "open_browser"
  | "open_changes"
  | "review_changes"
  | "start_dictation"
  | "attach_files"
  | "attach_projected_file"
  | "copy_task_markdown"
  | "copy_run_id"
  | "copy_working_directory"
  | "toggle_pin_task"
  | "open_model_picker"
  | "open_side_task"
  | "previous_operator_message"
  | "next_operator_message"
  | "voice_primary"
  | "voice_end"
  | "toggle_gesture_control";

export type ClientCommandSource = "keyboard" | "gesture" | "surface";

export type ClientCommand = {
  id: ClientCommandId;
  label: string;
  detail: string;
};

export const clientCommands: readonly ClientCommand[] = [
  { id: "new_chat", label: "New task", detail: "Start a new run in the selected Octopus OS scope" },
  { id: "open_voice", label: "Open Home", detail: "Open the persistent Home surface of the current Octopus OS" },
  { id: "open_settings", label: "Open settings", detail: "Open application settings" },
  { id: "open_command_menu", label: "Open command menu", detail: "Search commands, surfaces, and recent tasks" },
  { id: "focus_search", label: "Focus search", detail: "Focus search on the current surface" },
  { id: "focus_composer", label: "Focus composer", detail: "Move input focus to the active task composer" },
  { id: "composer.queue", label: "Queue message", detail: "Queue the active composer's message until the current reply finishes" },
  { id: "composer.steer", label: "Steer current reply", detail: "Send the active composer's message immediately without interrupting the current reply" },
  { id: "scroll_to_latest", label: "Scroll to latest", detail: "Return to the newest projected task activity" },
  { id: "navigate_back", label: "Back", detail: "Go back in surface history" },
  { id: "navigate_forward", label: "Forward", detail: "Go forward in surface history" },
  { id: "previous_surface", label: "Previous surface", detail: "Move to the previous surface" },
  { id: "next_surface", label: "Next surface", detail: "Move to the next surface" },
  { id: "toggle_sidebar", label: "Toggle sidebar", detail: "Show or hide the sidebar" },
  { id: "toggle_split", label: "Toggle split view", detail: "Open or close the secondary workspace" },
  { id: "toggle_task_summary", label: "Toggle task summary", detail: "Open or close the projected task summary panel" },
  { id: "open_terminal", label: "Open terminal", detail: "Open a new user terminal in the secondary workspace" },
  { id: "open_browser", label: "Open browser tab", detail: "Open the first projected Worker Environment browser" },
  { id: "open_changes", label: "Open review tab", detail: "Open the first projected Worker Environment changes surface" },
  { id: "review_changes", label: "Review changes in a new task", detail: "Open a read-only reviewer run with the active projected changes attached" },
  { id: "start_dictation", label: "Start dictation", detail: "Start or stop dictation in the active composer" },
  { id: "attach_files", label: "Attach files", detail: "Choose files for the active composer" },
  { id: "attach_projected_file", label: "Attach projected file", detail: "Choose exact bytes from a readable filesystem projected by the active run" },
  { id: "copy_task_markdown", label: "Copy task as Markdown", detail: "Copy the projected task transcript with provenance" },
  { id: "copy_run_id", label: "Copy run ID", detail: "Copy the active projected run identifier" },
  { id: "copy_working_directory", label: "Copy working directories", detail: "Copy every exact filesystem root projected by the active run or selected room" },
  { id: "toggle_pin_task", label: "Pin or unpin task", detail: "Toggle the active task in profile-local pinned organization" },
  { id: "open_model_picker", label: "Open model picker", detail: "Choose a declared provider interface, model, and reasoning effort" },
  { id: "open_side_task", label: "Open side task", detail: "Open the active projected run in the existing split workspace" },
  { id: "previous_operator_message", label: "Previous operator message", detail: "Jump to the previous operator turn in the active task" },
  { id: "next_operator_message", label: "Next operator message", detail: "Jump to the next operator turn in the active task" },
  { id: "voice_primary", label: "Start or mute Home voice", detail: "Start, mute, or unmute voice on the open Home surface" },
  { id: "voice_end", label: "End Home voice", detail: "End the active voice connection while Home is open" },
  { id: "toggle_gesture_control", label: "Turn gesture control on or off", detail: "Start or stop camera hand tracking in this client" },
];

export const CLIENT_COMMAND_EVENT = "octopus:client-command";

export type ClientSettingsDestination = { sectionId?: string; dictationRecordingId?: string };

export function dispatchClientCommand(id: ClientCommandId, source: ClientCommandSource, settingsDestination?: ClientSettingsDestination): void {
  window.dispatchEvent(new CustomEvent(CLIENT_COMMAND_EVENT, { detail: { id, source, settingsDestination } }));
}

export function subscribeClientCommands(listener: (id: ClientCommandId, source: ClientCommandSource, settingsDestination?: ClientSettingsDestination) => void): () => void {
  const handle = (event: Event) => {
    if (!(event instanceof CustomEvent)) return;
    const id = event.detail?.id;
    const source = event.detail?.source;
    if (!clientCommands.some((command) => command.id === id)) return;
    if (source !== "keyboard" && source !== "gesture" && source !== "surface") return;
    listener(id, source, id === "open_settings" ? event.detail?.settingsDestination : undefined);
  };
  window.addEventListener(CLIENT_COMMAND_EVENT, handle);
  return () => window.removeEventListener(CLIENT_COMMAND_EVENT, handle);
}
