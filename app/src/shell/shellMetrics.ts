export const SIDEBAR_OPEN_STORAGE_KEY = "octopus.sidebar.open.v1";
export const SIDEBAR_WIDTH_STORAGE_KEY = "octopus.sidebar.width.v1";
export const SIDEBAR_MIN_WIDTH = 238;
export const SIDEBAR_MAX_WIDTH = 468;
export const SIDEBAR_DEFAULT_WIDTH = 288;
export const MAIN_CONTENT_MIN_WIDTH = 320;
export const SIDEBAR_AUTO_COLLAPSE_WIDTH = 768;

export const TITLEBAR_HEIGHT_PX = 48;
export const NATIVE_CONTROL_INSET_DEFAULT_PX = 76;
export const TITLEBAR_SAFE_LEFT_GAP_PX = 6;
export const TITLEBAR_CLUSTER_GAP_PX = 8;
export const TITLEBAR_END_INSET_PX = 6;
export const TITLEBAR_TRAY_RIGHT_OFFSET_PX = 10;

export const CHROME_GUTTER_PX = 6;
export const CHROME_RADIUS_PX = 12;
export const CHROME_TOP_TRIGGER_PX = 12;
export const CHROME_FULLSCREEN_TOP_TRIGGER_PX = 24;
export const CHROME_LEFT_TRIGGER_PX = 18;
export const CHROME_TOP_APPROACH_PX = 34;
export const CHROME_FULLSCREEN_TOP_APPROACH_PX = 52;
export const CHROME_LEFT_APPROACH_PX = 46;
export const CHROME_TOP_FLICK_SPEED = 0.65;
export const CHROME_LEFT_FLICK_SPEED = 0.55;
export const CHROME_AIM_SPEED = 0.22;
export const CHROME_TOP_KEEP_PX = 68;
export const CHROME_LEFT_KEEP_PX = 48;
export const CHROME_LEFT_KEEP_CUSHION_PX = 32;
export const CHROME_REVEAL_GRACE_MS = 360;
export const CHROME_DISMISS_COOLDOWN_MS = 240;
export const CHROME_HIDE_FAST_MS = 160;
export const CHROME_HIDE_DELAY_MS = 280;
export const CHROME_LEAVE_WINDOW_DELAY_MS = 260;
export const CHROME_VELOCITY_WINDOW_MS = 80;
export const CHROME_DOM_POINTER_PRIORITY_MS = 60;
export const CHROME_INTERACTIVE_SELECTOR =
  '.app-titlebar, .app-shell-sidebar-slot, [data-oc-dropdown], [role="menu"], [role="dialog"]';

export function clampSidebarWidth(width: number, maximum = SIDEBAR_MAX_WIDTH): number {
  return Math.max(SIDEBAR_MIN_WIDTH, Math.min(maximum, Math.round(width)));
}

let shellSecondaryPaneOpen = false;
const shellSecondaryPaneListeners = new Set<() => void>();

export function getShellSecondaryPaneOpen(): boolean {
  return shellSecondaryPaneOpen;
}

export function setShellSecondaryPaneOpen(open: boolean): void {
  if (shellSecondaryPaneOpen === open) return;
  shellSecondaryPaneOpen = open;
  for (const listener of shellSecondaryPaneListeners) listener();
}

export function subscribeShellSecondaryPaneOpen(listener: () => void): () => void {
  shellSecondaryPaneListeners.add(listener);
  return () => shellSecondaryPaneListeners.delete(listener);
}
