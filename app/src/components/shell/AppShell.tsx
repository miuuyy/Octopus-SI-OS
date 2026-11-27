import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type ReactNode,
  type DragEvent, type PointerEvent,
} from "react";
import { AnimatePresence, animate, motion, useMotionValue, useTransform, type MotionStyle } from "motion/react";
import { useShouldReduceMotion } from "../../appearance";
import { restoreModalFocus } from "../../design-system/modalFocus";
import type { ClientSurfaceDescriptor } from "../../shell/surfaceTypes";
import type { ChatExecutionContext, ChatRun } from "../../chatRuns";
import {
  profileStorageKey,
} from "../../shell/clientWorkspaces";
import { Sidebar } from "../Sidebar";
import { NavigationRail, TopSystemTray } from "./NavigationRail";
import { DashboardViewMenu } from "./DashboardViewMenu";
import { WorkspacePaneHeaderActionsProvider } from "./WorkspacePaneHeaderActions";
import { SurfacePage } from "./SurfacePage";
import { TaskActionsController } from "../task/TaskActionsController";
import { NAVIGATION_RAIL_WIDTH, clientNavigationDestinations, type ClientNavigationSection } from "../../shell/navigation";
import { DesktopTitleBar } from "./DesktopTitleBar";
import { OverlaySidebarResizeProvider, PanelContent, PanelResizeHandle, useAnimatedPanelSize } from "./PanelResizeHandle";
import { WorkspacePaneManager } from "./WorkspacePaneManager";
import { subscribeClientCommands } from "../../clientActions";
import type { RuntimeHostView } from "../../runtimeHost";
import type { TitlebarSurfaceAction, TitlebarSurfaceContext } from "./DesktopTitleBar";
import type { OctopusOsProjectWorkspace } from "../../shell/useOctopusOsProjects";
import type { ProjectionResourcePhase } from "../projectionResource";
import type { OctopusProviderInterface } from "../../electron";
import type { ClientAgentDefinition } from "../../agentPresets";
import { clientWindowStateKey, clientWindowStateStorage } from "../../shell/clientWindowModel";
import {
  CHROME_AIM_SPEED,
  CHROME_DISMISS_COOLDOWN_MS,
  CHROME_DOM_POINTER_PRIORITY_MS,
  CHROME_FULLSCREEN_TOP_APPROACH_PX,
  CHROME_FULLSCREEN_TOP_TRIGGER_PX,
  CHROME_GUTTER_PX,
  CHROME_HIDE_DELAY_MS,
  CHROME_HIDE_FAST_MS,
  CHROME_INTERACTIVE_SELECTOR,
  CHROME_LEAVE_WINDOW_DELAY_MS,
  CHROME_LEFT_APPROACH_PX,
  CHROME_LEFT_FLICK_SPEED,
  CHROME_LEFT_KEEP_CUSHION_PX,
  CHROME_LEFT_KEEP_PX,
  CHROME_LEFT_TRIGGER_PX,
  CHROME_RADIUS_PX,
  CHROME_REVEAL_GRACE_MS,
  CHROME_TOP_APPROACH_PX,
  CHROME_TOP_FLICK_SPEED,
  CHROME_TOP_KEEP_PX,
  CHROME_TOP_TRIGGER_PX,
  CHROME_VELOCITY_WINDOW_MS,
  MAIN_CONTENT_MIN_WIDTH,
  NATIVE_CONTROL_INSET_DEFAULT_PX,
  SIDEBAR_AUTO_COLLAPSE_WIDTH,
  SIDEBAR_DEFAULT_WIDTH,
  SIDEBAR_MAX_WIDTH,
  SIDEBAR_MIN_WIDTH,
  SIDEBAR_OPEN_STORAGE_KEY,
  SIDEBAR_WIDTH_STORAGE_KEY,
  TITLEBAR_HEIGHT_PX,
  clampSidebarWidth,
  setShellSecondaryPaneOpen,
} from "../../shell/shellMetrics";

const CHROME_OPEN_SPRING = { type: "spring", stiffness: 360, damping: 32, mass: 0.76 } as const;
const CHROME_CLOSE_SPRING = { type: "spring", stiffness: 270, damping: 32, mass: 0.86 } as const;

type Props = {
  settingsSurface?: ReactNode;
  onCloseSettings?: () => void;
  inboxOpen?: boolean;
  inboxSurface?: ReactNode;
  onToggleInbox?: () => void;
  accessProfile: "live" | "lab";
  paneStateKey: string;
  preferredSidebarOpen: boolean;
  onSidebarOpenChange: (open: boolean) => void;
  sidebarWidth: number;
  surfaces: ClientSurfaceDescriptor[];
  navigationSection: ClientNavigationSection;
  activeSurfaceId: string;
  activeChatRunId: string;
  chatRuns: ChatRun[];
  chatRunsPhase: ProjectionResourcePhase;
  chatRunListError: string;
  onRetryChatRuns: () => Promise<void>;
  homeRunId: string;
  inboxUnreadCount: number;
  inboxArrivalId?: string;
  activityContent: ReactNode;
  dashboardPinnedSurfaceIds: string[];
  agentPinnedChatIds: string[];
  agentChatOrderIds: string[];
  agentProjectFilterEeId: string;
  agentProjectWorkspace: OctopusOsProjectWorkspace;
  agentExecutionContexts: ChatExecutionContext[];
  agentDefinitions: readonly ClientAgentDefinition[];
  agentProviderInterfaces: OctopusProviderInterface[];
  agentModelsLoading: boolean;
  agentModelError: string;
  onRefreshAgentModels: () => Promise<void>;
  dashboardExpanded: boolean;
  interactionDisabled: boolean;
  runtimeHost: RuntimeHostView;
  secondaryPaneFocused: boolean;
  secondaryPaneOpen: boolean;
  secondaryPane: ReactNode;
  secondaryTaskPaneWidthPercent: number;
  titlebarSurfaceActions?: TitlebarSurfaceAction[];
  titlebarSurfaceContext?: TitlebarSurfaceContext;
  titlebarSurfaceTitle?: string;
  surfaceErrors?: string[];
  onNavigationChange: (section: ClientNavigationSection) => void;
  onClientPaneDragOver: (event: DragEvent<HTMLDivElement>) => void;
  onClientPaneDrop: (event: DragEvent<HTMLDivElement>) => void;
  onSurfaceChange: (surfaceId: string) => void;
  onSelectChat: (runId: string) => void;
  onOpenChatInNewWindow: (runId: string) => void;
  onNewChat: (projectId?: string) => void;
  onDashboardPinnedSurfaceIdsChange: (surfaceIds: string[]) => void;
  onAgentPinnedChatIdsChange: (runIds: string[]) => void;
  onAgentChatOrderIdsChange: (runIds: string[]) => void;
  onAgentProjectFilterEeIdChange: (projectId: string) => void;
  onToggleSecondaryPane: () => void;
  onTitlebarSurfaceTitleClick?: () => void;
  onTitlebarSurfaceContextPointerDown?: (event: PointerEvent<HTMLSpanElement>) => void;
  onSecondaryPaneFocusedChange: (focused: boolean) => void;
  onPrimaryPaneBlockedChange: (blocked: boolean) => void;
  onSecondaryPaneOpenChange: (open: boolean) => void;
  onSecondaryTaskPaneWidthPercentChange: (width: number) => void;
  onOpenSettings: (section?: string) => void;
  onLogOut: () => void;
  onSearch: () => void;
  onSidebarWidthChange: (width: number) => void;
  onEffectiveSidebarWidthChange: (width: number) => void;
  canNavigateBack: boolean;
  canNavigateForward: boolean;
  onNavigateBack: () => void;
  onNavigateForward: () => void;
  children: ReactNode;
};

export function AppShell({
  settingsSurface,
  onCloseSettings,
  inboxOpen = false,
  inboxSurface,
  onToggleInbox,
  accessProfile,
  paneStateKey,
  preferredSidebarOpen,
  onSidebarOpenChange,
  sidebarWidth,
  surfaces,
  navigationSection,
  activeSurfaceId,
  activeChatRunId,
  chatRuns,
  chatRunsPhase,
  chatRunListError,
  onRetryChatRuns,
  homeRunId,
  inboxUnreadCount,
  inboxArrivalId,
  activityContent,
  dashboardPinnedSurfaceIds,
  agentPinnedChatIds,
  agentChatOrderIds,
  agentProjectFilterEeId,
  agentProjectWorkspace,
  agentExecutionContexts,
  agentDefinitions,
  agentProviderInterfaces,
  agentModelsLoading,
  agentModelError,
  onRefreshAgentModels,
  dashboardExpanded,
  interactionDisabled,
  runtimeHost,
  secondaryPaneFocused,
  secondaryPaneOpen,
  secondaryPane,
  secondaryTaskPaneWidthPercent,
  titlebarSurfaceActions,
  titlebarSurfaceContext,
  titlebarSurfaceTitle,
  surfaceErrors = [],
  onNavigationChange,
  onClientPaneDragOver,
  onClientPaneDrop,
  onSurfaceChange,
  onSelectChat,
  onOpenChatInNewWindow,
  onNewChat,
  onDashboardPinnedSurfaceIdsChange,
  onAgentPinnedChatIdsChange,
  onAgentChatOrderIdsChange,
  onAgentProjectFilterEeIdChange,
  onToggleSecondaryPane,
  onTitlebarSurfaceTitleClick,
  onTitlebarSurfaceContextPointerDown,
  onSecondaryPaneFocusedChange,
  onPrimaryPaneBlockedChange,
  onSecondaryPaneOpenChange,
  onSecondaryTaskPaneWidthPercentChange,
  onOpenSettings,
  onLogOut,
  onSearch,
  onSidebarWidthChange,
  onEffectiveSidebarWidthChange,
  canNavigateBack,
  canNavigateForward,
  onNavigateBack,
  onNavigateForward,
  children,
}: Props) {
  const viewportWidth = useViewportWidth();
  const reduceMotion = useShouldReduceMotion();
  const isElectron = Boolean(window.octopusTransport);
  const [fullScreen, setFullScreen] = useState(false);
  const [nativeInset, setNativeInset] = useState(isElectron ? NATIVE_CONTROL_INSET_DEFAULT_PX : 0);
  const [customWindowControls, setCustomWindowControls] = useState(false);
  const [compactSidebarState, setCompactSidebarState] = useState({ scope: paneStateKey, open: false });
  const [sidebarResizing, setSidebarResizing] = useState(false);
  const [chromeHovered, setChromeHovered] = useState(false);
  const [leftEdgePeek, setLeftEdgePeek] = useState(false);
  const hideTimerRef = useRef<number | null>(null);
  const dismissRafRef = useRef<number | null>(null);
  const focusOutRafRef = useRef<number | null>(null);
  const lastButtonsRef = useRef(0);
  const lastDomPointerAtRef = useRef(0);
  const lastPointerSourceRef = useRef<"dom" | "native" | null>(null);
  const revealedAtRef = useRef(0);
  const suppressRevealUntilRef = useRef(0);
  const pointerTrailRef = useRef<Array<{ x: number; y: number; t: number }>>([]);
  const lastPointerRef = useRef<{ x: number; y: number } | null>(null);
  const viewportWidthMotion = useMotionValue(viewportWidth);
  const titlebarLeftWidth = useMotionValue(196);
  const titlebarRightWidth = useMotionValue(36);
  const secondaryPaneWidth = useMotionValue(0);
  const shellRef = useRef<HTMLDivElement>(null);
  const [primaryActionsHost, setPrimaryActionsHost] = useState<HTMLDivElement | null>(null);
  const [primaryPaneBlocked, setPrimaryPaneBlocked] = useState(secondaryPaneFocused);
  const sidebarToggleRef = useRef<HTMLButtonElement>(null);
  const lastSidebarFocusRef = useRef<HTMLElement | null>(null);
  const overlayActive = interactionDisabled || inboxOpen;
  const sidebarAvailable = !overlayActive && clientNavigationDestinations.some((destination) => destination.id === navigationSection && destination.sidebar);
  const compactSidebar = viewportWidth <= SIDEBAR_AUTO_COLLAPSE_WIDTH;
  const chromeVisible = (!overlayActive && sidebarResizing) || chromeHovered || (compactSidebar && compactSidebarState.scope === paneStateKey && compactSidebarState.open);
  const sidebarOpen = sidebarAvailable && (compactSidebar
    ? compactSidebarState.scope === paneStateKey && compactSidebarState.open
    : chromeVisible && (preferredSidebarOpen || leftEdgePeek));
  const sidebarMaximumWidth = Math.max(
    SIDEBAR_MIN_WIDTH,
    Math.min(SIDEBAR_MAX_WIDTH, viewportWidth - NAVIGATION_RAIL_WIDTH - MAIN_CONTENT_MIN_WIDTH),
  );
  const effectiveSidebarWidth = Math.min(sidebarWidth, sidebarMaximumWidth);
  const chromeTransition = chromeVisible ? CHROME_OPEN_SPRING : CHROME_CLOSE_SPRING;
  const chromeProgress = useMotionValue(chromeVisible ? 1 : 0);
  const sidebarMotion = useAnimatedPanelSize(sidebarOpen, effectiveSidebarWidth, undefined, sidebarResizing, chromeTransition);
  const sidebarSlideX = useTransform(sidebarMotion.opacity, [0, 1], [-18, 0]);
  const chromeTop = useTransform(chromeProgress, (p) => `${(Math.max(0, Math.min(1, p)) * TITLEBAR_HEIGHT_PX).toFixed(2)}px`);
  const chromeGutter = useTransform(chromeProgress, (p) => `${(Math.max(0, Math.min(1, p)) * CHROME_GUTTER_PX).toFixed(2)}px`);
  const chromeRadius = useTransform(chromeProgress, (p) => `${(Math.max(0, Math.min(1, p)) * CHROME_RADIUS_PX).toFixed(2)}px`);
  const chromeBorderOpacity = useTransform(chromeProgress, (p) => Math.max(0, Math.min(1, p)).toFixed(3));
  const chromeBarY = useTransform(chromeProgress, [0, 1], ["-14px", "0px"]);
  const chromeBarOpacity = useTransform(chromeProgress, [0, 0.22, 1], [0, 0, 1]);
  const chromeDockScale = useTransform(chromeProgress, [0, 1], [0.95, 1]);
  const chromeDockY = useTransform(chromeProgress, [0, 1], ["-6px", "0px"]);
  const headerLeft = useTransform(
    [sidebarMotion.animatedSize, titlebarLeftWidth],
    ([sidebar, controls]) => `max(${NAVIGATION_RAIL_WIDTH + Number(sidebar)}px, ${Number(controls)}px)`,
  );
  const titlebarControlsRight = useTransform(titlebarLeftWidth, (controls) => `${controls}px`);
  const headerRight = useTransform(
    [secondaryPaneWidth, titlebarRightWidth],
    ([secondary, controls]) => `${Number(secondary) + Number(controls)}px`,
  );
  const primaryTopbarInset = useTransform(
    [viewportWidthMotion, sidebarMotion.animatedSize, secondaryPaneWidth],
    ([viewport, sidebar, secondary]) => (
      Number(viewport) - NAVIGATION_RAIL_WIDTH - Number(sidebar) - Number(secondary) <= 760 ? "8px" : "17px"
    ),
  );
  const titlebarRight = useTransform(titlebarRightWidth, (width) => `${width}px`);

  const clearHideTimer = useCallback(() => {
    if (hideTimerRef.current !== null) {
      window.clearTimeout(hideTimerRef.current);
      hideTimerRef.current = null;
    }
    if (dismissRafRef.current !== null) {
      window.cancelAnimationFrame(dismissRafRef.current);
      dismissRafRef.current = null;
    }
    if (focusOutRafRef.current !== null) {
      window.cancelAnimationFrame(focusOutRafRef.current);
      focusOutRafRef.current = null;
    }
  }, []);

  useEffect(() => {
    const transport = window.octopusTransport;
    if (!transport) return;
    let cancelled = false;
    void transport.windowState().then((state) => {
      if (cancelled) return;
      setFullScreen(state.fullScreen);
      setNativeInset(state.nativeControlInset);
      setCustomWindowControls(state.customWindowControls);
    });
    const unsubscribe = transport.onWindowStateChanged((state) => {
      setFullScreen(state.fullScreen);
      setNativeInset(state.nativeControlInset);
      setCustomWindowControls(state.customWindowControls);
    });
    return () => {
      cancelled = true;
      unsubscribe();
    };
  }, []);

  useLayoutEffect(() => {
    if (reduceMotion) {
      chromeProgress.set(chromeVisible ? 1 : 0);
      return;
    }
    const controls = animate(chromeProgress, chromeVisible ? 1 : 0, chromeVisible ? CHROME_OPEN_SPRING : CHROME_CLOSE_SPRING);
    return () => controls.stop();
  }, [chromeProgress, chromeVisible, reduceMotion]);

  useEffect(() => {
    if (!chromeVisible) setLeftEdgePeek(false);
    document.documentElement.dataset.chromeRevealed = String(chromeVisible);
    window.octopusTransport?.setWindowChromeVisible?.(chromeVisible);
  }, [chromeVisible]);

  useEffect(() => () => {
    delete document.documentElement.dataset.chromeRevealed;
    window.octopusTransport?.setWindowChromeVisible?.(true);
  }, []);

  useEffect(() => {
    setShellSecondaryPaneOpen(secondaryPaneOpen);
    return () => setShellSecondaryPaneOpen(false);
  }, [secondaryPaneOpen]);

  const chromeStateRef = useRef({
    chromeVisible,
    compactSidebar,
    effectiveSidebarWidth,
    fullScreen,
    overlayActive,
    preferredSidebarOpen,
    sidebarAvailable,
    sidebarOpen,
    sidebarResizing,
  });
  chromeStateRef.current = {
    chromeVisible,
    compactSidebar,
    effectiveSidebarWidth,
    fullScreen,
    overlayActive,
    preferredSidebarOpen,
    sidebarAvailable,
    sidebarOpen,
    sidebarResizing,
  };

  useEffect(() => {
    let active = true;

    const isInsideKeepZone = (x: number, y: number) => {
      const state = chromeStateRef.current;
      if (y <= CHROME_TOP_KEEP_PX) return true;
      if (!state.compactSidebar && state.sidebarOpen && x <= state.effectiveSidebarWidth + CHROME_LEFT_KEEP_CUSHION_PX) return true;
      if (!state.overlayActive && x <= CHROME_LEFT_KEEP_PX) return true;
      return false;
    };

    const recordVelocity = (x: number, y: number, now: number) => {
      const trail = pointerTrailRef.current;
      const prev = trail.length > 0 ? trail[trail.length - 1] : null;
      if (prev && now - prev.t > CHROME_VELOCITY_WINDOW_MS) {
        trail.length = 0;
      }
      const last = trail.length > 0 ? trail[trail.length - 1] : null;
      if (!last || Math.abs(x - last.x) >= 0.5 || Math.abs(y - last.y) >= 0.5 || now - last.t >= 16) {
        trail.push({ x, y, t: now });
      }
      const cutoff = now - CHROME_VELOCITY_WINDOW_MS;
      while (trail.length > 2 && trail[0].t < cutoff) {
        trail.shift();
      }
      if (trail.length > 8) trail.splice(0, trail.length - 8);
      let origin = trail[0];
      for (let i = trail.length - 2; i >= 0; i--) {
        origin = trail[i];
        if (now - origin.t >= 16) break;
      }
      const dt = now - origin.t;
      if (dt < 8) return { vx: 0, vy: 0 };
      return {
        vx: (x - origin.x) / dt,
        vy: (y - origin.y) / dt,
      };
    };

    const shouldTriggerReveal = (x: number, y: number, vx: number, vy: number) => {
      const state = chromeStateRef.current;
      const topTriggerPx = state.fullScreen ? CHROME_FULLSCREEN_TOP_TRIGGER_PX : CHROME_TOP_TRIGGER_PX;
      const topApproachPx = state.fullScreen ? CHROME_FULLSCREEN_TOP_APPROACH_PX : CHROME_TOP_APPROACH_PX;
      const directTop = y <= topTriggerPx;
      const directLeft = !state.overlayActive && x <= CHROME_LEFT_TRIGGER_PX;
      const directCorner = x <= 22 && y <= 22;
      const flickTop = y <= topApproachPx
        && vy <= -CHROME_TOP_FLICK_SPEED
        && Math.abs(vy) >= Math.abs(vx) * 1.2;
      const flickLeft = !state.overlayActive
        && x <= CHROME_LEFT_APPROACH_PX
        && vx <= -CHROME_LEFT_FLICK_SPEED
        && Math.abs(vx) >= Math.abs(vy) * 1.2;
      const leftTrigger = directLeft || flickLeft;
      return {
        reveal: directTop || directLeft || directCorner || flickTop || flickLeft,
        leftTrigger,
      };
    };

    const isAimingAtChrome = (x: number, y: number, vx: number, vy: number) => {
      const state = chromeStateRef.current;
      const headingUp = y <= 520
        && vy <= -CHROME_AIM_SPEED
        && Math.abs(vy) >= Math.abs(vx) * 0.35;
      if (headingUp) return true;
      if (!state.compactSidebar && state.sidebarOpen) {
        const headingLeft = x <= state.effectiveSidebarWidth + 240
          && vx <= -CHROME_AIM_SPEED
          && Math.abs(vx) >= Math.abs(vy) * 0.35;
        if (headingLeft) return true;
      }
      return false;
    };

    const revealChrome = (leftTrigger: boolean, now: number) => {
      clearHideTimer();
      revealedAtRef.current = now;
      const state = chromeStateRef.current;
      if (leftTrigger && state.sidebarAvailable && !state.compactSidebar) {
        setLeftEdgePeek(true);
      }
      setChromeHovered(true);
    };

    const dismissChrome = (now: number) => {
      clearHideTimer();
      suppressRevealUntilRef.current = now + CHROME_DISMISS_COOLDOWN_MS;
      pointerTrailRef.current = [];
      lastPointerSourceRef.current = null;
      setChromeHovered(false);
    };

    const scheduleHide = (delayMs: number, reset = false) => {
      if (reset) clearHideTimer();
      if (hideTimerRef.current !== null) return;
      hideTimerRef.current = window.setTimeout(() => {
        hideTimerRef.current = null;
        if (!active || hasOpenShellOverlay(shellRef.current)) return;
        const pointer = lastPointerRef.current;
        if (pointer && isInsideKeepZone(pointer.x, pointer.y)) return;
        setChromeHovered(false);
      }, delayMs);
    };

    const handlePointerSample = (sample: {
      source: "dom" | "native";
      x: number;
      y: number;
      insideWindow: boolean;
      resizing: boolean;
      buttons: number;
    }) => {
      const now = performance.now();
      const state = chromeStateRef.current;
      if (sample.source === "dom") {
        lastDomPointerAtRef.current = now;
      } else if (sample.insideWindow && now - lastDomPointerAtRef.current <= CHROME_DOM_POINTER_PRIORITY_MS) {
        return;
      }
      if (!sample.insideWindow) {
        lastButtonsRef.current = 0;
        lastPointerRef.current = null;
        pointerTrailRef.current = [];
        lastPointerSourceRef.current = null;
        if (!state.chromeVisible || hasOpenShellOverlay(shellRef.current)) return;
        scheduleHide(CHROME_LEAVE_WINDOW_DELAY_MS);
        return;
      }
      if (lastPointerSourceRef.current !== sample.source) {
        pointerTrailRef.current = [];
        lastPointerSourceRef.current = sample.source;
      }
      const { x, y } = sample;
      lastPointerRef.current = { x, y };
      const { vx, vy } = recordVelocity(x, y, now);
      if (!state.chromeVisible) {
        if (sample.buttons !== 0 || sample.resizing || now < suppressRevealUntilRef.current) return;
        const trigger = shouldTriggerReveal(x, y, vx, vy);
        if (trigger.reveal) {
          revealChrome(trigger.leftTrigger, now);
        }
        return;
      }
      if (sample.buttons !== 0) {
        clearHideTimer();
        return;
      }
      if (!state.overlayActive && x <= CHROME_LEFT_TRIGGER_PX && state.sidebarAvailable && !state.compactSidebar && !state.preferredSidebarOpen) {
        setLeftEdgePeek(true);
      }
      if (isInsideKeepZone(x, y) || hasOpenShellOverlay(shellRef.current)) {
        clearHideTimer();
        return;
      }
      const movingFastAway = vy > 0.25 && y > 110 && (
        state.compactSidebar
        || !state.sidebarOpen
        || x > state.effectiveSidebarWidth + CHROME_LEFT_KEEP_CUSHION_PX + 24
      );
      if (movingFastAway) {
        scheduleHide(CHROME_HIDE_FAST_MS, true);
        return;
      }
      if (isAimingAtChrome(x, y, vx, vy) || now - revealedAtRef.current < CHROME_REVEAL_GRACE_MS) {
        scheduleHide(CHROME_HIDE_DELAY_MS, true);
        return;
      }
      scheduleHide(CHROME_HIDE_DELAY_MS);
    };

    const onPointerMove = (event: globalThis.PointerEvent) => {
      lastButtonsRef.current = event.buttons;
      handlePointerSample({
        source: "dom",
        x: event.clientX,
        y: event.clientY,
        insideWindow: true,
        resizing: false,
        buttons: event.buttons,
      });
    };

    const onPointerDown = (event: globalThis.PointerEvent) => {
      lastButtonsRef.current = event.buttons;
      const target = event.target instanceof Element ? event.target : null;
      if (!target) return;
      if (target.closest(CHROME_INTERACTIVE_SELECTOR)) {
        clearHideTimer();
      }
    };

    const onPointerUp = (event: globalThis.PointerEvent) => {
      lastButtonsRef.current = event.buttons;
      const state = chromeStateRef.current;
      if (event.buttons !== 0 || state.sidebarResizing || !state.chromeVisible || hasOpenShellOverlay(shellRef.current)) return;
      const target = event.target instanceof Element ? event.target : null;
      if (target?.closest(CHROME_INTERACTIVE_SELECTOR)) {
        return;
      }
      if (!isInsideKeepZone(event.clientX, event.clientY)) {
        if (dismissRafRef.current !== null) window.cancelAnimationFrame(dismissRafRef.current);
        dismissRafRef.current = window.requestAnimationFrame(() => {
          dismissRafRef.current = null;
          const latest = chromeStateRef.current;
          if (!active || latest.sidebarResizing || !latest.chromeVisible || hasOpenShellOverlay(shellRef.current)) return;
          dismissChrome(performance.now());
        });
      }
    };

    const onPointerCancel = () => {
      lastButtonsRef.current = 0;
    };

    const onWheel = (event: WheelEvent) => {
      if (!chromeStateRef.current.chromeVisible || hasOpenShellOverlay(shellRef.current)) return;
      const target = event.target instanceof Element ? event.target : null;
      if (target?.closest(CHROME_INTERACTIVE_SELECTOR)) {
        return;
      }
      const pointer = lastPointerRef.current;
      if (pointer && isInsideKeepZone(pointer.x, pointer.y)) return;
      dismissChrome(performance.now());
    };

    const onDocumentMouseLeave = (event: MouseEvent) => {
      lastButtonsRef.current = 0;
      if (window.octopusTransport?.onWindowPointerUpdate) return;
      lastPointerRef.current = null;
      pointerTrailRef.current = [];
      lastPointerSourceRef.current = null;
      if (!chromeStateRef.current.chromeVisible || hasOpenShellOverlay(shellRef.current)) return;
      const exitedTopOrLeft = event.clientY <= CHROME_TOP_TRIGGER_PX || event.clientX <= CHROME_LEFT_TRIGGER_PX;
      scheduleHide(exitedTopOrLeft ? CHROME_LEAVE_WINDOW_DELAY_MS : CHROME_HIDE_DELAY_MS);
    };

    const onVisibilityChange = () => {
      if (document.visibilityState === "hidden") {
        lastButtonsRef.current = 0;
      }
    };

    const onWindowBlur = () => {
      lastButtonsRef.current = 0;
      lastPointerRef.current = null;
      pointerTrailRef.current = [];
      lastPointerSourceRef.current = null;
      if (!chromeStateRef.current.chromeVisible || hasOpenShellOverlay(shellRef.current)) return;
      if (chromeStateRef.current.fullScreen) {
        scheduleHide(CHROME_LEAVE_WINDOW_DELAY_MS, true);
        return;
      }
      clearHideTimer();
      setChromeHovered(false);
    };

    const onFocusIn = (event: FocusEvent) => {
      const target = event.target instanceof HTMLElement ? event.target : null;
      if (!target || !shellRef.current?.contains(target)) return;
      if (target.closest(".app-titlebar, .app-shell-sidebar-slot") && target.matches("input, textarea, [contenteditable='true'], :focus-visible")) {
        clearHideTimer();
        revealedAtRef.current = performance.now();
        setChromeHovered(true);
      }
    };

    const onFocusOut = () => {
      if (!chromeStateRef.current.chromeVisible) return;
      if (focusOutRafRef.current !== null) window.cancelAnimationFrame(focusOutRafRef.current);
      focusOutRafRef.current = window.requestAnimationFrame(() => {
        focusOutRafRef.current = null;
        if (!active || hasOpenShellOverlay(shellRef.current)) return;
        const pointer = lastPointerRef.current;
        if (!pointer || !isInsideKeepZone(pointer.x, pointer.y)) {
          scheduleHide(CHROME_HIDE_DELAY_MS);
        }
      });
    };

    const unsubscribeNativePointer = window.octopusTransport?.onWindowPointerUpdate?.((pointer) => {
      if (!active) return;
      handlePointerSample({
        source: "native",
        x: pointer.x,
        y: pointer.y,
        insideWindow: pointer.insideWindow && pointer.focused,
        resizing: pointer.resizing,
        buttons: lastButtonsRef.current,
      });
    });

    window.addEventListener("pointermove", onPointerMove, { passive: true });
    window.addEventListener("pointerdown", onPointerDown, { capture: true, passive: true });
    window.addEventListener("pointerup", onPointerUp, { capture: true, passive: true });
    window.addEventListener("pointercancel", onPointerCancel, { capture: true, passive: true });
    window.addEventListener("wheel", onWheel, { passive: true });
    window.addEventListener("blur", onWindowBlur);
    document.addEventListener("mouseleave", onDocumentMouseLeave);
    document.addEventListener("visibilitychange", onVisibilityChange);
    document.addEventListener("focusin", onFocusIn);
    document.addEventListener("focusout", onFocusOut);
    return () => {
      active = false;
      clearHideTimer();
      unsubscribeNativePointer?.();
      window.removeEventListener("pointermove", onPointerMove);
      window.removeEventListener("pointerdown", onPointerDown, { capture: true });
      window.removeEventListener("pointerup", onPointerUp, { capture: true });
      window.removeEventListener("pointercancel", onPointerCancel, { capture: true });
      window.removeEventListener("wheel", onWheel);
      window.removeEventListener("blur", onWindowBlur);
      document.removeEventListener("mouseleave", onDocumentMouseLeave);
      document.removeEventListener("visibilitychange", onVisibilityChange);
      document.removeEventListener("focusin", onFocusIn);
      document.removeEventListener("focusout", onFocusOut);
    };
  }, [clearHideTimer]);

  useLayoutEffect(() => {
    viewportWidthMotion.set(viewportWidth);
  }, [viewportWidth, viewportWidthMotion]);

  useLayoutEffect(() => {
    onEffectiveSidebarWidthChange(effectiveSidebarWidth);
  }, [effectiveSidebarWidth, onEffectiveSidebarWidthChange]);

  const setSidebarOpen = useCallback((open: boolean, moveFocus = true) => {
    const sidebar = shellRef.current?.querySelector(".app-shell-sidebar-slot");
    const focusInsideSidebar = !open && document.activeElement instanceof HTMLElement && sidebar?.contains(document.activeElement);
    if (focusInsideSidebar) {
      lastSidebarFocusRef.current = document.activeElement as HTMLElement;
    }
    onSidebarOpenChange(open);
    setCompactSidebarState({ scope: paneStateKey, open });
    if (open) {
      clearHideTimer();
      revealedAtRef.current = performance.now();
      setChromeHovered(true);
    } else {
      setLeftEdgePeek(false);
    }
    if (focusInsideSidebar) window.requestAnimationFrame(() => sidebarToggleRef.current?.focus());
    if (open && moveFocus) {
      window.requestAnimationFrame(() => {
        const candidate = lastSidebarFocusRef.current;
        if (candidate?.isConnected) candidate.focus();
        else shellRef.current?.querySelector<HTMLElement>(".app-shell-sidebar-slot [data-sidebar-nav-item]")?.focus();
      });
    }
  }, [clearHideTimer, onSidebarOpenChange, paneStateKey]);

  const completeCompactNavigation = () => {
    if (compactSidebar) setCompactSidebarState({ scope: paneStateKey, open: false });
  };

  useEffect(() => subscribeClientCommands((command) => {
    if (command !== "toggle_sidebar" || overlayActive) return;
    if (sidebarAvailable) setSidebarOpen(!sidebarOpen);
  }), [overlayActive, setSidebarOpen, sidebarAvailable, sidebarOpen]);

  useLayoutEffect(() => {
    setCompactSidebarState({ scope: paneStateKey, open: false });
  }, [compactSidebar, paneStateKey]);

  const onTitlebarSlotWidthsChange = useCallback((widths: { left: number; right: number }) => {
    titlebarLeftWidth.set(widths.left);
    titlebarRightWidth.set(widths.right);
  }, [titlebarLeftWidth, titlebarRightWidth]);

  const handlePrimaryBlockedChange = useCallback((blocked: boolean) => {
    setPrimaryPaneBlocked(blocked);
    onPrimaryPaneBlockedChange(blocked);
  }, [onPrimaryPaneBlockedChange]);

  const style = {
    "--app-shell-header-left": overlayActive ? titlebarControlsRight : headerLeft,
    "--app-titlebar-controls-right": titlebarControlsRight,
    "--app-shell-header-right": headerRight,
    "--app-shell-primary-topbar-inset": primaryTopbarInset,
    "--app-shell-titlebar-right": titlebarRight,
    "--chrome-top": chromeTop,
    "--chrome-gutter": chromeGutter,
    "--chrome-radius": chromeRadius,
    "--chrome-border-opacity": chromeBorderOpacity,
    "--chrome-bar-y": chromeBarY,
    "--chrome-bar-opacity": chromeBarOpacity,
    "--chrome-dock-scale": chromeDockScale,
    "--chrome-dock-y": chromeDockY,
    "--sidebar-w": `${effectiveSidebarWidth}px`,
  } as MotionStyle;

  return (
    <motion.div
      className={[
        "app-shell",
        chromeVisible ? "is-chrome-revealed" : "is-chrome-hidden",
        fullScreen && "is-fullscreen",
        overlayActive && "is-overlay-open",
        dashboardExpanded && "is-dashboard-expanded",
        sidebarOpen ? "is-sidebar-open" : "is-sidebar-closed",
        sidebarResizing && "is-sidebar-resizing",
      ].filter(Boolean).join(" ")}
      data-platform={customWindowControls ? "desktop" : "darwin"}
      data-app-shell-page-surface="true"
      data-navigation-section={navigationSection}
      onDragOver={onClientPaneDragOver}
      onDrop={onClientPaneDrop}
      ref={shellRef}
      style={style}
    >
      <DesktopTitleBar
        accessProfile={accessProfile}
        chromeRevealed={chromeVisible}
        contentDisabled={compactSidebar && sidebarOpen}
        dock={(
          <NavigationRail
            activeSection={overlayActive ? null : navigationSection}
            onNavigate={onNavigationChange}
            surfaces={surfaces}
          />
        )}
        fullScreen={fullScreen}
        inboxOpen={inboxOpen}
        nativeInset={nativeInset}
        systemTray={(
          <TopSystemTray
            accessProfile={accessProfile}
            arrivalId={inboxArrivalId}
            hasInbox={surfaces.some((surface) => surface.id === "inbox")}
            inboxOpen={inboxOpen}
            onCloseSettings={onCloseSettings}
            onLogOut={onLogOut}
            onOpenSettings={onOpenSettings}
            onToggleInbox={onToggleInbox}
            runtimeHost={runtimeHost}
            settingsOpen={interactionDisabled}
            unreadCount={inboxUnreadCount}
          />
        )}
        primaryPaneBlocked={primaryPaneBlocked}
        onPrimaryActionsHostChange={setPrimaryActionsHost}
        settingsOpen={interactionDisabled}
        canNavigateBack={canNavigateBack}
        canNavigateForward={canNavigateForward}
        sidebarOpen={sidebarOpen}
        sidebarAvailable={sidebarAvailable}
        secondaryPaneFocused={secondaryPaneFocused}
        secondaryPaneOpen={secondaryPaneOpen}
        surfaceActions={titlebarSurfaceActions}
        surfaceContext={titlebarSurfaceContext}
        surfaceContextAccessory={navigationSection === "dashboard" ? <DashboardViewMenu
          activeSurfaceId={activeSurfaceId}
          pinnedSurfaceIds={dashboardPinnedSurfaceIds}
          surfaces={surfaces}
          onPinnedSurfaceIdsChange={onDashboardPinnedSurfaceIdsChange}
          onSelect={onSurfaceChange}
        /> : undefined}
        surfaceTitle={titlebarSurfaceTitle}
        sidebarToggleRef={sidebarToggleRef}
        onNavigateBack={onNavigateBack}
        onNavigateForward={onNavigateForward}
        onSlotWidthsChange={onTitlebarSlotWidthsChange}
        onToggleSidebar={() => setSidebarOpen(!sidebarOpen)}
        onToggleSecondaryPane={onToggleSecondaryPane}
        onSurfaceContextPointerDown={onTitlebarSurfaceContextPointerDown}
        onSurfaceTitleClick={onTitlebarSurfaceTitleClick}
      />

      <div className="app-shell-page">
        <div className="app-shell-page-region" aria-hidden={overlayActive || undefined} inert={overlayActive || undefined}>
          {compactSidebar && sidebarOpen ? (
            <button aria-label="Close navigation" className="app-shell-sidebar-backdrop" onClick={() => setSidebarOpen(false)} type="button" />
          ) : null}

          <motion.aside
            aria-hidden={!sidebarOpen || undefined}
            className="app-shell-sidebar-slot"
            inert={!sidebarOpen || undefined}
            style={{ opacity: sidebarMotion.opacity, width: compactSidebar ? undefined : sidebarMotion.animatedSize }}
          >
            <div className="app-shell-sidebar-clip">
              <motion.div className="app-shell-sidebar-content" style={{ x: sidebarSlideX }}>
                <PanelContent active={sidebarAvailable} mounted={sidebarMotion.mounted}>
                {sidebarAvailable ? <Sidebar
                  accessProfile={accessProfile}
                  surfaces={surfaces}
                  navigationSection={navigationSection}
                  activeSurfaceId={activeSurfaceId}
                  activeChatRunId={activeChatRunId}
                  chatRuns={chatRuns}
                  chatRunsPhase={chatRunsPhase}
                  chatRunListError={chatRunListError}
                  onRetryChatRuns={onRetryChatRuns}
                  homeRunId={homeRunId}
                  activityContent={activityContent}
                  agentPinnedChatIds={agentPinnedChatIds}
                  agentChatOrderIds={agentChatOrderIds}
                  agentProjectFilterEeId={agentProjectFilterEeId}
                  agentProjectWorkspace={agentProjectWorkspace}
                  agentExecutionContexts={agentExecutionContexts}
                  agentDefinitions={agentDefinitions}
                  agentProviderInterfaces={agentProviderInterfaces}
                  agentModelsLoading={agentModelsLoading}
                  agentModelError={agentModelError}
                  onRefreshAgentModels={onRefreshAgentModels}
                  surfaceErrors={surfaceErrors}
                  runtimeHost={runtimeHost}
                  onSurfaceChange={(surfaceId) => {
                    onSurfaceChange(surfaceId);
                    completeCompactNavigation();
                  }}
                  onSelectChat={(runId) => {
                    onSelectChat(runId);
                    completeCompactNavigation();
                  }}
                  onNewChat={(projectId) => {
                    onNewChat(projectId);
                    completeCompactNavigation();
                  }}
                  onAgentPinnedChatIdsChange={onAgentPinnedChatIdsChange}
                  onAgentChatOrderIdsChange={onAgentChatOrderIdsChange}
                  onAgentProjectFilterEeIdChange={onAgentProjectFilterEeIdChange}
                  onSearch={() => {
                    onSearch();
                    completeCompactNavigation();
                  }}
                /> : null}
                </PanelContent>
              </motion.div>
            </div>

            {sidebarOpen || sidebarResizing ? (
              <PanelResizeHandle
                ariaLabel="Resize sidebar"
                className="app-shell-sidebar-resizer"
                defaultSize={SIDEBAR_DEFAULT_WIDTH}
                edge="right"
                maximum={sidebarMaximumWidth}
                minimum={SIDEBAR_MIN_WIDTH}
                value={effectiveSidebarWidth}
                onChange={onSidebarWidthChange}
                onCommit={onSidebarWidthChange}
                onOpenChange={setSidebarOpen}
                onResizingChange={setSidebarResizing}
              />
            ) : null}
          </motion.aside>

          <main className="workspace main-surface" inert={compactSidebar && sidebarOpen || undefined}>
            <WorkspacePaneManager
              key={paneStateKey}
              onPrimaryBlockedChange={handlePrimaryBlockedChange}
              onOpenChange={onSecondaryPaneOpenChange}
              resolvedWidthMotion={secondaryPaneWidth}
              onSecondaryFocusedChange={onSecondaryPaneFocusedChange}
              onSecondaryWidthPercentChange={onSecondaryTaskPaneWidthPercentChange}
              primary={<WorkspacePaneHeaderActionsProvider host={primaryActionsHost}>{navigationSection === "dashboard" ? (
                <div className="dashboard-page">
                  <div
                    aria-label={titlebarSurfaceContext?.title}
                    className="dashboard-page-content"
                    id="dashboard-surface"
                    role="region"
                  ><SurfacePage>{children}</SurfacePage></div>
                </div>
              ) : <SurfacePage>{children}</SurfacePage>}</WorkspacePaneHeaderActionsProvider>}
              secondary={secondaryPane}
              secondaryFocused={secondaryPaneFocused}
              secondaryOpen={secondaryPaneOpen}
              secondaryWidthPercent={secondaryTaskPaneWidthPercent}
            />
          </main>
        </div>
        <AnimatePresence initial={false} mode="wait">
          {settingsSurface ? (
            <motion.div
              animate={{ opacity: 1, scale: 1 }}
              className="app-shell-page-overlay"
              exit={{ opacity: 0, scale: 0.995, transition: reduceMotion ? { duration: 0 } : { duration: 0.1, ease: [0.4, 0, 1, 1] } }}
              initial={{ opacity: 0, scale: 0.995 }}
              key="settings-overlay"
              transition={reduceMotion ? { duration: 0 } : { duration: 0.14, ease: [0.22, 1, 0.36, 1] }}
            >
              <OverlaySidebarResizeProvider
                value={{
                  defaultWidth: SIDEBAR_DEFAULT_WIDTH,
                  maximumWidth: sidebarMaximumWidth,
                  minimumWidth: SIDEBAR_MIN_WIDTH,
                  onResizingChange: setSidebarResizing,
                  onWidthChange: onSidebarWidthChange,
                  width: effectiveSidebarWidth,
                }}
              >
                {settingsSurface}
              </OverlaySidebarResizeProvider>
            </motion.div>
          ) : inboxOpen && inboxSurface ? (
            <motion.div
              animate={{ opacity: 1, scale: 1 }}
              className="app-shell-page-overlay"
              exit={{ opacity: 0, scale: 0.995, transition: reduceMotion ? { duration: 0 } : { duration: 0.1, ease: [0.4, 0, 1, 1] } }}
              initial={{ opacity: 0, scale: 0.995 }}
              key="inbox-overlay"
              transition={reduceMotion ? { duration: 0 } : { duration: 0.14, ease: [0.22, 1, 0.36, 1] }}
            >
              <InboxOverlayContainer
                activityContent={activityContent}
                effectiveSidebarWidth={effectiveSidebarWidth}
                inboxSurface={inboxSurface}
                onSidebarWidthChange={onSidebarWidthChange}
                setSidebarResizing={setSidebarResizing}
                sidebarMaximumWidth={sidebarMaximumWidth}
              />
            </motion.div>
          ) : null}
        </AnimatePresence>
      </div>
      <TaskActionsController
        runtimeHome={runtimeHost.home}
        accessProfile={accessProfile}
        activeChatRunId={activeChatRunId}
        chatOrderIds={agentChatOrderIds}
        chatRuns={chatRuns}
        homeRunId={homeRunId}
        pinnedChatIds={agentPinnedChatIds}
        projectWorkspace={agentProjectWorkspace}
        onChatOrderIdsChange={onAgentChatOrderIdsChange}
        onOpenChatInNewWindow={onOpenChatInNewWindow}
        onPinnedChatIdsChange={onAgentPinnedChatIdsChange}
        onSelectChat={onSelectChat}
      />
    </motion.div>
  );
}

function InboxOverlayContainer({
  activityContent,
  effectiveSidebarWidth,
  inboxSurface,
  onSidebarWidthChange,
  setSidebarResizing,
  sidebarMaximumWidth,
}: {
  activityContent: ReactNode;
  effectiveSidebarWidth: number;
  inboxSurface: ReactNode;
  onSidebarWidthChange: (width: number) => void;
  setSidebarResizing: (resizing: boolean) => void;
  sidebarMaximumWidth: number;
}) {
  const containerRef = useRef<HTMLDivElement>(null);
  const previousFocusRef = useRef(document.activeElement instanceof HTMLElement ? document.activeElement : null);

  useEffect(() => {
    const container = containerRef.current;
    if (container && !container.contains(document.activeElement)) {
      const target = container.querySelector<HTMLElement>(
        'button:not([disabled]), [href], input:not([disabled]), [tabindex]:not([tabindex="-1"])',
      ) ?? container;
      target.focus({ preventScroll: true });
    }
    return () => {
      restoreModalFocus(previousFocusRef.current);
    };
  }, []);

  return (
    <div aria-label="Inbox" className="shell-overlay-layout inbox-overlay-page" ref={containerRef} role="region" tabIndex={-1}>
      <aside className="shell-overlay-rail inbox-overlay-rail">
        <div className="inbox-overlay-rail-scroll">
          {activityContent}
        </div>
        <PanelResizeHandle
          ariaLabel="Resize Inbox sidebar"
          className="app-shell-sidebar-resizer"
          collapsible={false}
          defaultSize={SIDEBAR_DEFAULT_WIDTH}
          edge="right"
          maximum={sidebarMaximumWidth}
          minimum={SIDEBAR_MIN_WIDTH}
          value={effectiveSidebarWidth}
          onChange={onSidebarWidthChange}
          onCommit={onSidebarWidthChange}
          onOpenChange={() => {}}
          onResizingChange={setSidebarResizing}
        />
      </aside>
      <div className="shell-overlay-main inbox-overlay-main">
        {inboxSurface}
      </div>
    </div>
  );
}

function hasOpenShellOverlay(shell: HTMLElement | null): boolean {
  if (document.querySelector('[data-oc-dropdown], [role="menu"], [data-sidebar-dragging="true"]')) return true;
  for (const dialog of document.querySelectorAll<HTMLElement>('[role="dialog"]')) {
    if (dialog.getAttribute("data-state") === "closed") continue;
    if (dialog.closest('[hidden], [inert], [aria-hidden="true"]')) continue;
    return true;
  }
  const active = document.activeElement;
  if (active instanceof HTMLElement && shell?.contains(active)) {
    if (active.closest(".app-titlebar, .app-shell-sidebar-slot")) {
      if (active.matches("input, textarea, [contenteditable='true'], :focus-visible")) return true;
    }
  }
  return false;
}

function useViewportWidth() {
  const [width, setWidth] = useState(() => window.innerWidth);
  useEffect(() => {
    const update = () => setWidth(window.innerWidth);
    window.addEventListener("resize", update);
    return () => window.removeEventListener("resize", update);
  }, []);
  return width;
}

export function readSidebarOpen(accessProfile: "live" | "lab", clientWindowScope = "main", defaultOpen = true) {
  const stored = clientWindowStateStorage(clientWindowScope).getItem(
    clientWindowStateKey(clientWindowScope, profileStorageKey(SIDEBAR_OPEN_STORAGE_KEY, accessProfile)),
  );
  return stored === null ? defaultOpen : stored !== "false";
}

export function readSidebarWidth(accessProfile: "live" | "lab", clientWindowScope = "main") {
  const width = Number(clientWindowStateStorage(clientWindowScope).getItem(
    clientWindowStateKey(clientWindowScope, profileStorageKey(SIDEBAR_WIDTH_STORAGE_KEY, accessProfile)),
  ));
  return Number.isFinite(width) && width > 0 ? clampSidebarWidth(width) : SIDEBAR_DEFAULT_WIDTH;
}

export function writeSidebarWidth(accessProfile: "live" | "lab", width: number, clientWindowScope = "main") {
  if (!Number.isFinite(width) || width <= 0) return;
  try {
    clientWindowStateStorage(clientWindowScope).setItem(
      clientWindowStateKey(clientWindowScope, profileStorageKey(SIDEBAR_WIDTH_STORAGE_KEY, accessProfile)),
      String(clampSidebarWidth(width)),
    );
  } catch {
    // Ignore storage quota or private-mode errors.
  }
}
