import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type ReactNode,
  type DragEvent, type PointerEvent,
} from "react";
import { motion, useMotionValue, useTransform, type MotionStyle } from "motion/react";
import type { ClientSurfaceDescriptor } from "../../shell/surfaceTypes";
import type { ChatExecutionContext, ChatRun } from "../../chatRuns";
import {
  profileStorageKey,
} from "../../shell/clientWorkspaces";
import { Sidebar } from "../Sidebar";
import { NavigationRail } from "./NavigationRail";
import { DashboardViewMenu } from "./DashboardViewMenu";
import { WindowPaneControlsProvider } from "./WindowPaneControls";
import { WorkspacePaneHeaderActionsProvider } from "./WorkspacePaneHeaderActions";
import { SurfacePage } from "./SurfacePage";
import { TaskActionsController } from "../task/TaskActionsController";
import { NAVIGATION_RAIL_WIDTH, clientNavigationDestinations, type ClientNavigationSection } from "../../shell/navigation";
import { DesktopTitleBar } from "./DesktopTitleBar";
import { PanelContent, PanelResizeHandle, useAnimatedPanelSize } from "./PanelResizeHandle";
import { WorkspacePaneManager } from "./WorkspacePaneManager";
import { subscribeClientCommands } from "../../clientActions";
import type { RuntimeHostView } from "../../runtimeHost";
import type { TitlebarSurfaceAction, TitlebarSurfaceContext } from "./DesktopTitleBar";
import type { OctopusOsProjectWorkspace } from "../../shell/useOctopusOsProjects";
import type { ProjectionResourcePhase } from "../projectionResource";
import type { OctopusProviderInterface } from "../../electron";
import type { ClientAgentDefinition } from "../../agentPresets";
import { clientWindowStateKey, clientWindowStateStorage } from "../../shell/clientWindowModel";

const SIDEBAR_OPEN_STORAGE_KEY = "octopus.sidebar.open.v1";
const SIDEBAR_WIDTH_STORAGE_KEY = "octopus.sidebar.width.v1";
const SIDEBAR_MIN_WIDTH = 238;
const SIDEBAR_MAX_WIDTH = 468;
const SIDEBAR_DEFAULT_WIDTH = 288;
const MAIN_CONTENT_MIN_WIDTH = 320;
const SIDEBAR_AUTO_COLLAPSE_WIDTH = 768;

type Props = {
  settingsSurface?: ReactNode;
  onCloseSettings?: () => void;
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
  const [compactSidebarState, setCompactSidebarState] = useState({ scope: paneStateKey, open: false });
  const [sidebarResizing, setSidebarResizing] = useState(false);
  const viewportWidthMotion = useMotionValue(viewportWidth);
  const titlebarLeftWidth = useMotionValue(196);
  const titlebarRightWidth = useMotionValue(36);
  const secondaryPaneWidth = useMotionValue(0);
  const shellRef = useRef<HTMLDivElement>(null);
  const [windowPaneControlsHost, setWindowPaneControlsHost] = useState<HTMLDivElement | null>(null);
  const [primaryActionsHost, setPrimaryActionsHost] = useState<HTMLDivElement | null>(null);
  const [primaryPaneBlocked, setPrimaryPaneBlocked] = useState(secondaryPaneFocused);
  const sidebarToggleRef = useRef<HTMLButtonElement>(null);
  const lastSidebarFocusRef = useRef<HTMLElement | null>(null);
  const sidebarAvailable = clientNavigationDestinations.some((destination) => destination.id === navigationSection && destination.sidebar);
  const compactSidebar = viewportWidth <= SIDEBAR_AUTO_COLLAPSE_WIDTH;
  const sidebarOpen = sidebarAvailable && (compactSidebar
    ? compactSidebarState.scope === paneStateKey && compactSidebarState.open
    : preferredSidebarOpen);
  const sidebarMaximumWidth = Math.max(
    SIDEBAR_MIN_WIDTH,
    Math.min(SIDEBAR_MAX_WIDTH, viewportWidth - NAVIGATION_RAIL_WIDTH - MAIN_CONTENT_MIN_WIDTH),
  );
  const effectiveSidebarWidth = Math.min(sidebarWidth, sidebarMaximumWidth);
  const sidebarMotion = useAnimatedPanelSize(sidebarOpen, effectiveSidebarWidth, undefined, sidebarResizing);
  const headerLeft = useTransform(
    [sidebarMotion.animatedSize, titlebarLeftWidth],
    ([sidebar, controls]) => `max(${NAVIGATION_RAIL_WIDTH + Number(sidebar)}px, ${Number(controls)}px)`,
  );
  const settingsHeaderLeft = useTransform(titlebarLeftWidth, (controls) => `${controls}px`);
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
  const sidebarLayoutWidth = useTransform(sidebarMotion.animatedSize, (width) => `${NAVIGATION_RAIL_WIDTH + width}px`);
  const titlebarRight = useTransform(titlebarRightWidth, (width) => `${width}px`);

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
    if (focusInsideSidebar) window.requestAnimationFrame(() => sidebarToggleRef.current?.focus());
    if (open && moveFocus) {
      window.requestAnimationFrame(() => {
        const candidate = lastSidebarFocusRef.current;
        if (candidate?.isConnected) candidate.focus();
        else shellRef.current?.querySelector<HTMLElement>(".app-shell-sidebar-slot [data-sidebar-nav-item]")?.focus();
      });
    }
  }, [onSidebarOpenChange, paneStateKey]);

  const completeCompactNavigation = () => {
    if (compactSidebar) setCompactSidebarState({ scope: paneStateKey, open: false });
  };

  useEffect(() => subscribeClientCommands((command) => {
    if (command !== "toggle_sidebar" || interactionDisabled) return;
    if (sidebarAvailable) setSidebarOpen(!sidebarOpen);
  }), [interactionDisabled, setSidebarOpen, sidebarAvailable, sidebarOpen]);

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
    "--app-shell-header-left": interactionDisabled ? settingsHeaderLeft : headerLeft,
    "--app-shell-header-right": headerRight,
    "--app-shell-primary-topbar-inset": primaryTopbarInset,
    "--app-shell-titlebar-right": titlebarRight,
    "--sidebar-layout-w": sidebarLayoutWidth,
    "--sidebar-w": `${effectiveSidebarWidth}px`,
    "--navigation-rail-width": `${NAVIGATION_RAIL_WIDTH}px`,
  } as MotionStyle;

  return (
    <motion.div
      className={[
        "app-shell",
        dashboardExpanded && "is-dashboard-expanded",
        sidebarOpen ? "is-sidebar-open" : "is-sidebar-closed",
        sidebarResizing && "is-sidebar-resizing",
      ].filter(Boolean).join(" ")}
      data-platform="darwin"
      data-app-shell-page-surface="true"
      data-navigation-section={navigationSection}
      onDragOver={onClientPaneDragOver}
      onDrop={onClientPaneDrop}
      ref={shellRef}
      style={style}
    >
      <DesktopTitleBar
        accessProfile={accessProfile}
        contentDisabled={compactSidebar && sidebarOpen}
        primaryPaneBlocked={primaryPaneBlocked}
        onPrimaryActionsHostChange={setPrimaryActionsHost}
        onCloseSettings={onCloseSettings}
        onPaneControlsHostChange={setWindowPaneControlsHost}
        settingsOpen={interactionDisabled}
        canNavigateBack={canNavigateBack}
        canNavigateForward={canNavigateForward}
        sidebarOpen={sidebarOpen}
        sidebarAvailable={sidebarAvailable}
        secondaryPaneFocused={secondaryPaneFocused}
        secondaryPaneOpen={secondaryPaneOpen}
        secondaryPaneWidth={secondaryPaneWidth}
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

      <NavigationRail
        accessProfile={accessProfile}
        activeSection={navigationSection}
        surfaces={surfaces}
        unreadCount={inboxUnreadCount}
        arrivalId={inboxArrivalId}
        runtimeHost={runtimeHost}
        onNavigate={(section) => {
          onNavigationChange(section);
        }}
        onOpenSettings={onOpenSettings}
        onLogOut={onLogOut}
      />
      <div className="app-shell-page">
        <div className="app-shell-page-region" aria-hidden={interactionDisabled || undefined} inert={interactionDisabled || undefined}>
          {compactSidebar && sidebarOpen ? (
            <button aria-label="Close navigation" className="app-shell-sidebar-backdrop" onClick={() => setSidebarOpen(false)} type="button" />
          ) : null}

          <motion.aside
            aria-hidden={!sidebarOpen || undefined}
            className="app-shell-sidebar-slot"
            inert={!sidebarOpen || undefined}
            style={{ opacity: sidebarMotion.opacity, width: sidebarMotion.animatedSize }}
          >
            <div className="app-shell-sidebar-clip">
              <div className="app-shell-sidebar-content">
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
              </div>
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
            <WindowPaneControlsProvider host={windowPaneControlsHost}>
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
            </WindowPaneControlsProvider>
          </main>
        </div>
        {settingsSurface ? <div className="app-shell-page-overlay">{settingsSurface}</div> : null}
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

function clampSidebarWidth(width: number, maximum = SIDEBAR_MAX_WIDTH) {
  return Math.max(SIDEBAR_MIN_WIDTH, Math.min(maximum, Math.round(width)));
}
