import type { KeyboardEvent } from "react";
import "../icon-glyphs/navigation";
import "../icon-glyphs/settings";
import { OcIconButton } from "../../design-system/components";
import { Glyph } from "../IconGlyphs";
import { formatKeyboardShortcuts, useKeyboardShortcuts } from "../../keyboardShortcuts";
import { clientNavigationDestinations, type ClientNavigationSection } from "../../shell/navigation";
import type { ClientSurfaceDescriptor } from "../../shell/surfaceTypes";
import type { RuntimeHostView } from "../../runtimeHost";
import { AccountRailButton } from "./AccountRailButton";
import { GestureRailIndicator } from "../gestures/GestureRailIndicator";
import { MessageTooltip } from "../chat/MessageTooltip";
import { ShellTooltip } from "./ShellTooltip";

export function NavigationRail({
  disabled = false,
  activeSection,
  surfaces,
  onNavigate,
}: {
  disabled?: boolean;
  activeSection: ClientNavigationSection | null;
  surfaces: readonly ClientSurfaceDescriptor[];
  onNavigate: (section: ClientNavigationSection) => void;
}) {
  const destinations = clientNavigationDestinations.filter((destination) =>
    destination.id !== "inbox"
    && surfaces.some((surface) => surface.id === destination.surfaceId),
  );

  return (
    <nav
      aria-hidden={disabled || undefined}
      aria-label="App navigation"
      className="navigation-rail"
      data-app-navigation-rail="true"
      inert={disabled || undefined}
      onKeyDown={moveRailFocus}
    >
      <div className="navigation-rail-destinations">
        {destinations.map((destination) => {
          const active = destination.id === activeSection;
          return (
            <div className="navigation-rail-item" key={destination.id}>
              <MessageTooltip delayMs={300} label={destination.label} side="bottom">
                <OcIconButton
                  appearance="rail"
                  aria-current={active ? "page" : undefined}
                  data-navigation-destination={destination.id}
                  glyph={active ? destination.selectedGlyph : destination.glyph}
                  label={destination.label}
                  onClick={() => onNavigate(destination.id)}
                  selected={active}
                  title=""
                />
              </MessageTooltip>
            </div>
          );
        })}
      </div>
    </nav>
  );
}

export function TopSystemTray({
  accessProfile,
  arrivalId,
  hasInbox = true,
  inboxOpen = false,
  runtimeHost,
  settingsOpen = false,
  unreadCount = 0,
  onCloseSettings,
  onLogOut,
  onOpenSettings,
  onToggleInbox,
}: {
  accessProfile: "live" | "lab";
  arrivalId?: string;
  hasInbox?: boolean;
  inboxOpen?: boolean;
  runtimeHost: RuntimeHostView;
  settingsOpen?: boolean;
  unreadCount?: number;
  onCloseSettings?: () => void;
  onLogOut: () => void;
  onOpenSettings: (section?: string) => void;
  onToggleInbox?: () => void;
}) {
  const shortcuts = useKeyboardShortcuts(accessProfile);
  const settingsShortcut = formatKeyboardShortcuts(shortcuts.open_settings ?? []);
  const settingsLabel = settingsOpen ? "Close Settings" : "Settings";
  const inboxLabel = unreadCount > 0 ? `Inbox, ${unreadCount} unread` : "Inbox";
  const inboxActionLabel = inboxOpen ? "Close Inbox" : inboxLabel;

  return (
    <div className="top-system-tray no-drag">
      <GestureRailIndicator accessProfile={accessProfile} onOpenSettings={() => onOpenSettings("gestures")} />
      <ShellTooltip label={settingsLabel} shortcut={settingsShortcut}>
        <button
          aria-label={settingsLabel}
          aria-pressed={settingsOpen}
          className="app-titlebar-action no-drag"
          data-state={settingsOpen ? "open" : "closed"}
          onClick={() => (settingsOpen && onCloseSettings ? onCloseSettings() : onOpenSettings())}
          type="button"
        >
          <Glyph className="icon-xs" name="settings" size={16} />
        </button>
      </ShellTooltip>
      <AccountRailButton accessProfile={accessProfile} runtimeHost={runtimeHost} onLogOut={onLogOut} onOpenSettings={onOpenSettings} />
      {hasInbox ? (
        <div className="navigation-rail-item">
          <ShellTooltip label={inboxActionLabel}>
            <button
              aria-label={inboxActionLabel}
              aria-pressed={inboxOpen}
              className="app-titlebar-action no-drag"
              data-navigation-destination="inbox"
              data-state={inboxOpen ? "open" : "closed"}
              onClick={() => onToggleInbox?.()}
              type="button"
            >
              <Glyph className="icon-xs" name={inboxOpen ? "bellFilled" : "bell"} size={16} />
            </button>
          </ShellTooltip>
          {unreadCount > 0 ? <span className="navigation-rail-unread top-tray-unread" aria-hidden="true" /> : null}
          {unreadCount > 0 && arrivalId ? <span className="navigation-rail-arrival" key={arrivalId} aria-hidden="true" /> : null}
        </div>
      ) : null}
    </div>
  );
}

function moveRailFocus(event: KeyboardEvent<HTMLElement>): void {
  if (!["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown", "Home", "End"].includes(event.key) || !(event.target instanceof HTMLButtonElement)) return;
  const buttons = Array.from(event.currentTarget.querySelectorAll<HTMLButtonElement>("button:not(:disabled)"));
  const index = buttons.indexOf(event.target);
  if (index < 0 || !buttons.length) return;
  event.preventDefault();
  const forward = event.key === "ArrowRight" || event.key === "ArrowDown";
  const next = event.key === "Home" ? 0 : event.key === "End" ? buttons.length - 1
    : (index + (forward ? 1 : -1) + buttons.length) % buttons.length;
  buttons[next].focus();
}
