import { useEffect, useLayoutEffect, useRef, useState, type CSSProperties, type PointerEvent, type ReactNode, type Ref } from "react";
import { AnimatePresence, motion } from "motion/react";
import { useShouldReduceMotion } from "../../appearance";
import { Glyph, type GlyphName } from "../IconGlyphs";
import { clientDialogOwnsCommands, formatKeyboardShortcuts, useKeyboardShortcuts } from "../../keyboardShortcuts";
import type { OctopusClientAccessProfile } from "../../electron";
import {
  NATIVE_CONTROL_INSET_DEFAULT_PX,
  TITLEBAR_CLUSTER_GAP_PX,
  TITLEBAR_END_INSET_PX,
  TITLEBAR_SAFE_LEFT_GAP_PX,
  TITLEBAR_TRAY_RIGHT_OFFSET_PX,
} from "../../shell/shellMetrics";
import { ShellTooltip } from "./ShellTooltip";

type Props = {
  accessProfile: OctopusClientAccessProfile;
  chromeRevealed?: boolean;
  dock?: ReactNode;
  fullScreen?: boolean;
  inboxOpen?: boolean;
  nativeInset?: number;
  systemTray?: ReactNode;
  onPrimaryActionsHostChange?: (host: HTMLDivElement | null) => void;
  contentDisabled?: boolean;
  primaryPaneBlocked?: boolean;
  settingsOpen?: boolean;
  canNavigateBack: boolean;
  canNavigateForward: boolean;
  sidebarOpen: boolean;
  sidebarAvailable?: boolean;
  secondaryPaneFocused: boolean;
  secondaryPaneOpen: boolean;
  surfaceActions?: TitlebarSurfaceAction[];
  surfaceContext?: TitlebarSurfaceContext;
  surfaceContextAccessory?: ReactNode;
  surfaceTitle?: string;
  sidebarToggleRef?: Ref<HTMLButtonElement>;
  onSurfaceTitleClick?: () => void;
  onSurfaceContextPointerDown?: (event: PointerEvent<HTMLSpanElement>) => void;
  onNavigateBack: () => void;
  onNavigateForward: () => void;
  onToggleSidebar: () => void;
  onToggleSecondaryPane: () => void;
  onSlotWidthsChange: (widths: { left: number; right: number }) => void;
};

export type TitlebarSurfaceAction = {
  actionId?: string;
  badge?: boolean;
  glyph: GlyphName;
  global?: boolean;
  label: string;
  onClick: () => void;
  pressed?: boolean;
};

export type TitlebarSurfaceContext = {
  glyph?: GlyphName;
  title: string;
};

export function DesktopTitleBar({
  accessProfile,
  chromeRevealed = true,
  dock,
  fullScreen: controlledFullScreen,
  inboxOpen = false,
  nativeInset: controlledNativeInset,
  systemTray,
  onPrimaryActionsHostChange,
  contentDisabled = false,
  primaryPaneBlocked = false,
  settingsOpen = false,
  canNavigateBack,
  canNavigateForward,
  sidebarOpen,
  sidebarAvailable = true,
  secondaryPaneFocused,
  secondaryPaneOpen,
  surfaceActions = [],
  surfaceContext,
  surfaceContextAccessory,
  surfaceTitle,
  sidebarToggleRef,
  onSurfaceTitleClick,
  onSurfaceContextPointerDown,
  onNavigateBack,
  onNavigateForward,
  onToggleSidebar,
  onToggleSecondaryPane,
  onSlotWidthsChange,
}: Props) {
  const shortcuts = useKeyboardShortcuts(accessProfile);
  const reduceMotion = useShouldReduceMotion();
  const isElectron = Boolean(window.octopusTransport);
  const [uncontrolledFullScreen, setUncontrolledFullScreen] = useState(false);
  const [uncontrolledNativeInset, setUncontrolledNativeInset] = useState(isElectron ? NATIVE_CONTROL_INSET_DEFAULT_PX : 0);
  const fullScreen = controlledFullScreen ?? uncontrolledFullScreen;
  const nativeInset = controlledNativeInset ?? uncontrolledNativeInset;
  const hasControlledWindowState = controlledFullScreen !== undefined && controlledNativeInset !== undefined;
  const headerRef = useRef<HTMLElement>(null);
  const controlsRef = useRef<HTMLDivElement>(null);
  const paneActionsRef = useRef<HTMLDivElement>(null);
  const systemTrayRef = useRef<HTMLDivElement>(null);
  const [trayWidth, setTrayWidth] = useState(0);
  const hasSystemTray = Boolean(systemTray);
  const systemOverlayOpen = inboxOpen || settingsOpen;
  const context = systemOverlayOpen || primaryPaneBlocked ? undefined : surfaceContext;
  const contextTransfer = systemOverlayOpen ? undefined : onSurfaceContextPointerDown;

  useEffect(() => {
    const transport = window.octopusTransport;
    if (hasControlledWindowState || !transport) return;
    let cancelled = false;
    void transport.windowState().then((state) => {
      if (!cancelled) {
        setUncontrolledFullScreen(state.fullScreen);
        setUncontrolledNativeInset(state.nativeControlInset);
      }
    });
    const unsubscribe = transport.onWindowStateChanged((state) => {
      setUncontrolledFullScreen(state.fullScreen);
      setUncontrolledNativeInset(state.nativeControlInset);
    });
    return () => {
      cancelled = true;
      unsubscribe();
    };
  }, [hasControlledWindowState]);

  useEffect(() => {
    const transport = window.octopusTransport;
    if (!transport) return;
    const blocked = systemOverlayOpen || clientDialogOwnsCommands();
    transport.setHistoryNavigationState({ canNavigateBack: !blocked && canNavigateBack, canNavigateForward: !blocked && canNavigateForward });
    const unsubscribeBack = transport.onNavigateBack(() => { if (!systemOverlayOpen && !clientDialogOwnsCommands()) onNavigateBack(); });
    const unsubscribeForward = transport.onNavigateForward(() => { if (!systemOverlayOpen && !clientDialogOwnsCommands()) onNavigateForward(); });
    return () => {
      unsubscribeBack();
      unsubscribeForward();
    };
  }, [canNavigateBack, canNavigateForward, onNavigateBack, onNavigateForward, systemOverlayOpen]);

  useLayoutEffect(() => {
    const controls = controlsRef.current;
    const paneActions = paneActionsRef.current;
    if (!controls || !paneActions) return;

    const measure = () => {
      const headerLeft = headerRef.current?.getBoundingClientRect().left ?? 0;
      const controlsRect = controls.getBoundingClientRect();
      const paneActionsRect = paneActions.getBoundingClientRect();
      const nextTrayWidth = systemTrayRef.current
        ? Math.ceil(systemTrayRef.current.getBoundingClientRect().width) + TITLEBAR_TRAY_RIGHT_OFFSET_PX
        : 0;
      setTrayWidth((previous) => (previous === nextTrayWidth ? previous : nextTrayWidth));
      onSlotWidthsChange({
        left: Math.ceil(controlsRect.right - headerLeft) + TITLEBAR_CLUSTER_GAP_PX,
        right: Math.ceil(paneActionsRect.width + nextTrayWidth + TITLEBAR_END_INSET_PX),
      });
    };

    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(controls);
    observer.observe(paneActions);
    if (systemTrayRef.current) observer.observe(systemTrayRef.current);
    window.addEventListener("resize", measure);
    return () => {
      observer.disconnect();
      window.removeEventListener("resize", measure);
    };
  }, [
    fullScreen,
    hasSystemTray,
    nativeInset,
    onSlotWidthsChange,
    primaryPaneBlocked,
    secondaryPaneFocused,
    secondaryPaneOpen,
    sidebarAvailable,
    systemOverlayOpen,
  ]);

  return (
    <header
      aria-label="Window navigation"
      className={`app-titlebar${chromeRevealed ? " is-chrome-revealed" : " is-chrome-hidden"}${isElectron ? " is-electron" : ""}${fullScreen ? " is-fullscreen" : ""}${secondaryPaneOpen ? " has-secondary-pane" : ""}`}
      ref={headerRef}
      style={{
        "--app-titlebar-safe-left": `${(fullScreen ? 0 : nativeInset) + TITLEBAR_SAFE_LEFT_GAP_PX}px`,
        "--app-titlebar-tray-width": `${trayWidth}px`,
      } as CSSProperties}
    >
      {!fullScreen ? <div aria-hidden="true" className="app-titlebar-drag-region" /> : null}
      <div className="app-titlebar-controls no-drag" ref={controlsRef}>
        {!systemOverlayOpen ? (
          <span className="app-titlebar-history">
            <TitlebarButton
              disabled={!canNavigateBack}
              glyph="navigateBack"
              label="Back"
              onClick={onNavigateBack}
              shortcut={formatKeyboardShortcuts(shortcuts.navigate_back ?? [])}
            />
            <TitlebarButton
              disabled={!canNavigateForward}
              glyph="navigateBack"
              glyphClassName="-scale-x-100"
              label="Forward"
              onClick={onNavigateForward}
              shortcut={formatKeyboardShortcuts(shortcuts.navigate_forward ?? [])}
            />
          </span>
        ) : null}
        {!systemOverlayOpen && sidebarAvailable ? <TitlebarButton
          buttonRef={sidebarToggleRef}
          glyph={sidebarOpen ? "sidebarOpen" : "sidebarClosed"}
          label={`${sidebarOpen ? "Hide" : "Show"} sidebar`}
          onClick={onToggleSidebar}
          shortcut={formatKeyboardShortcuts(shortcuts.toggle_sidebar ?? [])}
        /> : null}
      </div>
      {context ? (
        <div className="app-titlebar-surface-context-slot">
          <span
            className={`app-titlebar-surface-context${contextTransfer ? " is-transferable no-drag" : ""}`}
            onPointerDown={contextTransfer}
          >
            {context.glyph ? <Glyph className="icon-xs shrink-0" name={context.glyph} size={15} /> : null}
            <span title={context.title}>{context.title}</span>
            {surfaceContextAccessory}
          </span>
        </div>
      ) : null}
      {!systemOverlayOpen && !primaryPaneBlocked && surfaceTitle ? (
        <div className="app-titlebar-surface-title-inline">
          <button
            aria-label={onSurfaceTitleClick ? `Close ${surfaceTitle}` : surfaceTitle}
            className="app-titlebar-surface-title no-drag"
            onClick={onSurfaceTitleClick}
            type="button"
          >
            {onSurfaceTitleClick ? <Glyph className="icon-2xs" name="chevronUp" size={12} /> : null}
            <span>{surfaceTitle}</span>
          </button>
        </div>
      ) : null}
      <AnimatePresence initial={false}>
        {dock ? (
          <motion.div
            animate={{ opacity: 1, y: 0 }}
            className="app-titlebar-dock-slot"
            exit={{ opacity: 0, y: -4 }}
            initial={{ opacity: 0, y: -4 }}
            key="top-dock"
            transition={reduceMotion ? { duration: 0 } : { duration: 0.16, ease: [0.23, 1, 0.32, 1] }}
          >
            {dock}
          </motion.div>
        ) : null}
      </AnimatePresence>
      <div
        className="app-titlebar-pane-actions no-drag"
        data-pane-role="primary"
        hidden={systemOverlayOpen || primaryPaneBlocked}
        inert={contentDisabled || systemOverlayOpen || primaryPaneBlocked}
        ref={paneActionsRef}
        style={{ right: trayWidth + TITLEBAR_END_INSET_PX }}
      >
        <div className="workspace-pane-header-actions no-drag" ref={onPrimaryActionsHostChange} />
        {(systemOverlayOpen ? [] : surfaceActions).map((surfaceAction) => (
          <TitlebarButton
            actionId={surfaceAction.actionId}
            badge={surfaceAction.badge}
            glyph={surfaceAction.glyph}
            key={surfaceAction.label}
            label={surfaceAction.label}
            onClick={surfaceAction.onClick}
            open={surfaceAction.pressed}
            surfaceAction
          />
        ))}
        {!systemOverlayOpen ? (
          <TitlebarButton
            glyph="splitPane"
            label={secondaryPaneOpen ? "Close secondary pane" : "Open secondary pane"}
            onClick={onToggleSecondaryPane}
            open={secondaryPaneOpen}
            shortcut={formatKeyboardShortcuts(shortcuts.toggle_split ?? [])}
          />
        ) : null}
      </div>
      {systemTray ? (
        <div className="app-titlebar-system-tray no-drag" ref={systemTrayRef}>
          {systemTray}
        </div>
      ) : null}
    </header>
  );
}

function TitlebarButton({
  actionId,
  badge = false,
  buttonRef,
  disabled = false,
  glyph,
  glyphClassName,
  label,
  onClick,
  open = false,
  surfaceAction = false,
  shortcut,
}: {
  actionId?: string;
  badge?: boolean;
  buttonRef?: Ref<HTMLButtonElement>;
  disabled?: boolean;
  glyph: GlyphName;
  glyphClassName?: string;
  label: string;
  onClick: () => void;
  open?: boolean;
  surfaceAction?: boolean;
  shortcut?: string;
}) {
  return (
    <ShellTooltip label={label} shortcut={shortcut} disabled={disabled}>
      <button
        aria-label={label}
        className="app-titlebar-action no-drag"
        data-state={open ? "open" : "closed"}
        data-titlebar-action={actionId}
        data-titlebar-surface-action={surfaceAction || undefined}
        disabled={disabled}
        onClick={(event) => {
          event.stopPropagation();
          onClick();
        }}
        onPointerDown={(event) => event.stopPropagation()}
        ref={buttonRef}
        type="button"
      >
        <Glyph className={`icon-xs${glyphClassName ? ` ${glyphClassName}` : ""}`} name={glyph} size={16} />
        {badge ? <i aria-hidden="true" className="app-titlebar-action-badge" /> : null}
      </button>
    </ShellTooltip>
  );
}
